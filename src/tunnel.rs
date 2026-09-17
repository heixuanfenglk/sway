use anyhow::{Context, Result};
use std::io;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use crate::config::{AuthMethod, TunnelRequest};
use crate::stats::TrafficStats;

struct ClientHandler;

impl russh::client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // 简化：接受任意主机密钥（生产环境应做 known_hosts 校验）
        Ok(true)
    }
}

pub struct TunnelHandle {
    stop_tx: watch::Sender<bool>,
    join: tokio::task::JoinHandle<()>,
    pub stats: Arc<TrafficStats>,
}

impl TunnelHandle {
    pub async fn stop(self) {
        let _ = self.stop_tx.send(true);
        let _ = self.join.await;
    }
}

pub async fn start_tunnel(config: TunnelRequest) -> Result<TunnelHandle> {
    let (stop_tx, stop_rx) = watch::channel(false);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Result<()>>();

    let stats = TrafficStats::new();
    let cfg = config.clone();
    let stats_for_task = Arc::clone(&stats);
    let join = tokio::spawn(async move {
        match setup_and_run(cfg, stop_rx, ready_tx, stats_for_task).await {
            Ok(()) => info!("SSH 隧道已停止"),
            Err(e) => error!("SSH 隧道异常退出: {e:#}"),
        }
    });

    match ready_rx.await {
        Ok(Ok(())) => {
            stats.mark_connected();
            Ok(TunnelHandle {
                stop_tx,
                join,
                stats,
            })
        }
        Ok(Err(e)) => {
            let _ = stop_tx.send(true);
            let _ = join.await;
            Err(e)
        }
        Err(_) => {
            let _ = stop_tx.send(true);
            let _ = join.await;
            anyhow::bail!("隧道任务意外结束")
        }
    }
}

async fn setup_and_run(
    config: TunnelRequest,
    stop_rx: watch::Receiver<bool>,
    ready_tx: tokio::sync::oneshot::Sender<Result<()>>,
    stats: Arc<TrafficStats>,
) -> Result<()> {
    connect_and_listen(config, stop_rx, ready_tx, stats).await
}

async fn connect_and_listen(
    config: TunnelRequest,
    mut stop_rx: watch::Receiver<bool>,
    ready_tx: tokio::sync::oneshot::Sender<Result<()>>,
    stats: Arc<TrafficStats>,
) -> Result<()> {
    let ready_tx = std::sync::Mutex::new(Some(ready_tx));

    let notify_ready = |r: Result<()>| {
        if let Ok(mut slot) = ready_tx.lock() {
            if let Some(tx) = slot.take() {
                let _ = tx.send(r);
            }
        }
    };

    let addr = format!("{}:{}", config.host.trim(), config.port);
    info!("正在连接 SSH: {addr}");

    let mut ssh_config = russh::client::Config::default();
    // 保活，避免长时间空闲被中间设备掐断
    ssh_config.keepalive_interval = Some(std::time::Duration::from_secs(15));
    ssh_config.keepalive_max = 6;
    ssh_config.inactivity_timeout = None;
    // 更大窗口/缓冲，提升多连接并发吞吐
    ssh_config.window_size = 8 * 1024 * 1024;
    ssh_config.channel_buffer_size = 128;
    let ssh_config = Arc::new(ssh_config);

    let mut handle = match russh::client::connect(ssh_config, &addr, ClientHandler).await {
        Ok(h) => h,
        Err(e) => {
            let err = anyhow::anyhow!(e).context(format!("无法连接 SSH 服务器 {addr}"));
            notify_ready(Err(anyhow::anyhow!("{err:#}")));
            return Err(err);
        }
    };

    let auth_result = match config.auth_method {
        AuthMethod::Password => handle
            .authenticate_password(config.username.trim(), config.password.as_str())
            .await
            .context("密码认证失败"),
        AuthMethod::PrivateKey => {
            let key = russh::keys::load_secret_key(&config.private_key_path, None)
                .context("读取/解析私钥失败")?;
            let hash = handle
                .best_supported_rsa_hash()
                .await
                .context("协商 RSA 哈希算法失败")?
                .flatten();
            handle
                .authenticate_publickey(
                    config.username.trim(),
                    russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await
                .context("公钥认证失败")
        }
    };

    let authenticated = match auth_result {
        Ok(v) => v.success(),
        Err(e) => {
            notify_ready(Err(anyhow::anyhow!("{e:#}")));
            return Err(e);
        }
    };

    if !authenticated {
        let err = anyhow::anyhow!("SSH 认证被拒绝");
        notify_ready(Err(anyhow::anyhow!("{err:#}")));
        return Err(err);
    }
    info!("SSH 认证成功");

    let socks_listener = match TcpListener::bind(("127.0.0.1", config.socks_port)).await {
        Ok(l) => l,
        Err(e) => {
            let err = anyhow::anyhow!(e).context(format!("无法绑定 SOCKS 端口 {}", config.socks_port));
            notify_ready(Err(anyhow::anyhow!("{err:#}")));
            return Err(err);
        }
    };
    let http_listener = match TcpListener::bind(("127.0.0.1", config.http_port)).await {
        Ok(l) => l,
        Err(e) => {
            let err = anyhow::anyhow!(e).context(format!("无法绑定 HTTP 端口 {}", config.http_port));
            notify_ready(Err(anyhow::anyhow!("{err:#}")));
            return Err(err);
        }
    };

    info!(
        "本地代理已启动 SOCKS5=127.0.0.1:{} HTTP=127.0.0.1:{}",
        config.socks_port, config.http_port
    );

    notify_ready(Ok(()));

    // Handle 的 channel_open_* 是 &self，可并发打开；勿用 Mutex 把建连串行化（会严重拖慢浏览）
    let handle = Arc::new(handle);

    probe_egress(&handle).await;

    loop {
        tokio::select! {
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    info!("正在停止 SSH 隧道");
                    break;
                }
            }
            accept = socks_listener.accept() => {
                match accept {
                    Ok((stream, peer)) => {
                        let h = Arc::clone(&handle);
                        let s = Arc::clone(&stats);
                        tokio::spawn(async move {
                            optimize_socket(&stream);
                            if let Err(e) = handle_socks5(stream, h, s).await {
                                log_proxy_err("SOCKS", peer, &e);
                            }
                        });
                    }
                    Err(e) => warn!("SOCKS accept 错误: {e}"),
                }
            }
            accept = http_listener.accept() => {
                match accept {
                    Ok((stream, peer)) => {
                        let h = Arc::clone(&handle);
                        let s = Arc::clone(&stats);
                        tokio::spawn(async move {
                            optimize_socket(&stream);
                            if let Err(e) = handle_http_proxy(stream, h, s).await {
                                log_proxy_err("HTTP", peer, &e);
                            }
                        });
                    }
                    Err(e) => warn!("HTTP accept 错误: {e}"),
                }
            }
        }
    }

    Ok(())
}

fn optimize_socket(stream: &TcpStream) {
    let _ = stream.set_nodelay(true);
}

type SshHandle = Arc<russh::client::Handle<ClientHandler>>;

async fn open_direct(
    ssh: &SshHandle,
    host: &str,
    port: u16,
) -> std::result::Result<russh::Channel<russh::client::Msg>, russh::Error> {
    ssh.channel_open_direct_tcpip(host, port as u32, "127.0.0.1", 0)
        .await
}

async fn open_channel(
    ssh: &SshHandle,
    host: &str,
    port: u16,
) -> Result<russh::Channel<russh::client::Msg>> {
    // 1) 先按主机名走远端 DNS（等同 ssh -D）
    match open_direct(ssh, host, port).await {
        Ok(ch) => return Ok(ch),
        Err(e) => {
            let msg = format!("{e}");
            // ConnectFailed：远端连不上目标，常因远端 DNS/IPv6/防火墙。再试本机解析的 IPv4。
            if !msg.contains("ConnectFailed") && !msg.contains("connect failed") {
                return Err(anyhow::anyhow!(e)
                    .context(format!("无法通过 SSH 打开到 {host}:{port} 的通道")));
            }
            tracing::debug!("远端直连 {host}:{port} 失败 ({e})，尝试本机 IPv4 解析后重试");
        }
    }

    let mut last_err = None;
    for ip in resolve_ipv4(host).await? {
        match open_direct(ssh, &ip.to_string(), port).await {
            Ok(ch) => {
                tracing::debug!("经本机解析 {host} -> {ip} 打开通道成功");
                return Ok(ch);
            }
            Err(e) => {
                last_err = Some(anyhow::anyhow!(e));
            }
        }
    }

    Err(last_err
        .unwrap_or_else(|| anyhow::anyhow!("无可用 IPv4 地址"))
        .context(format!(
            "无法通过 SSH 打开到 {host}:{port} 的通道（远端 ConnectFailed）。\
             请在 SSH 服务器上执行: curl -vI https://{host} 或 nc -vz {host} {port}；\
             并确认 sshd_config 中 AllowTcpForwarding yes"
        )))
}

async fn resolve_ipv4(host: &str) -> Result<Vec<std::net::Ipv4Addr>> {
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        return Ok(vec![ip]);
    }
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
        anyhow::bail!("暂不支持纯 IPv6 目标");
    }

    let mut ips = Vec::new();
    for addr in tokio::net::lookup_host((host, 80))
        .await
        .with_context(|| format!("本机 DNS 解析失败: {host}"))?
    {
        if let std::net::SocketAddr::V4(v4) = addr {
            let ip = *v4.ip();
            if !ips.contains(&ip) {
                ips.push(ip);
            }
        }
    }
    if ips.is_empty() {
        anyhow::bail!("本机未能解析到 {host} 的 IPv4 地址");
    }
    Ok(ips)
}

async fn probe_egress(ssh: &SshHandle) {
    // 国内/海外出口各试一个，便于定位“转发被禁”还是“特定站点不可达”
    let probes = [("www.baidu.com", 443u16), ("1.1.1.1", 443u16), ("www.google.com", 443u16)];
    let mut ok = 0usize;
    for (host, port) in probes {
        match open_channel(ssh, host, port).await {
            Ok(_ch) => {
                info!("出口探测成功: {host}:{port}");
                ok += 1;
                break;
            }
            Err(e) => warn!("出口探测失败: {host}:{port} · {e:#}"),
        }
    }
    if ok == 0 {
        warn!(
            "所有出口探测均失败。SSH 已登录，但服务器无法代连外网目标。\
             常见原因: 1) sshd AllowTcpForwarding 未开启 2) 云厂商安全组禁止出站 3) 服务器本身无外网"
        );
    }
}

fn is_benign_disconnect(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(ioe) = cause.downcast_ref::<io::Error>() {
            match ioe.kind() {
                io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::UnexpectedEof
                | io::ErrorKind::NotConnected
                | io::ErrorKind::TimedOut => return true,
                _ => {}
            }
            #[cfg(windows)]
            {
                // WSAECONNABORTED / WSAECONNRESET —— 浏览器取消请求很常见
                if matches!(ioe.raw_os_error(), Some(10053 | 10054 | 10058)) {
                    return true;
                }
            }
        }
    }
    let msg = err.to_string();
    msg.contains("10053")
        || msg.contains("10054")
        || msg.contains("connection abort")
        || msg.contains("connection reset")
        || msg.contains("中止了一个已建立的连接")
        || msg.contains("强制关闭了一个现有的连接")
}

fn log_proxy_err(kind: &str, peer: std::net::SocketAddr, err: &anyhow::Error) {
    if is_benign_disconnect(err) {
        debug!("{kind} 连接 {peer} 已断开: {err}");
    } else {
        warn!("{kind} 连接 {peer} 失败: {err:#}");
    }
}

async fn handle_socks5(
    mut client: TcpStream,
    ssh: SshHandle,
    stats: Arc<TrafficStats>,
) -> Result<()> {
    let mut buf = [0u8; 258];
    client.read_exact(&mut buf[..2]).await?;
    if buf[0] != 0x05 {
        anyhow::bail!("非 SOCKS5 协议");
    }
    let nmethods = buf[1] as usize;
    client.read_exact(&mut buf[..nmethods]).await?;
    client.write_all(&[0x05, 0x00]).await?;

    client.read_exact(&mut buf[..4]).await?;
    if buf[0] != 0x05 || buf[1] != 0x01 {
        let _ = client
            .write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await;
        anyhow::bail!("仅支持 SOCKS5 CONNECT");
    }
    let atyp = buf[3];
    let (host, port) = match atyp {
        0x01 => {
            client.read_exact(&mut buf[..4]).await?;
            let ip = format!("{}.{}.{}.{}", buf[0], buf[1], buf[2], buf[3]);
            client.read_exact(&mut buf[..2]).await?;
            let port = u16::from_be_bytes([buf[0], buf[1]]);
            (ip, port)
        }
        0x03 => {
            client.read_exact(&mut buf[..1]).await?;
            let len = buf[0] as usize;
            client.read_exact(&mut buf[..len]).await?;
            let host = String::from_utf8_lossy(&buf[..len]).to_string();
            client.read_exact(&mut buf[..2]).await?;
            let port = u16::from_be_bytes([buf[0], buf[1]]);
            (host, port)
        }
        0x04 => anyhow::bail!("暂不支持 IPv6 目标"),
        _ => anyhow::bail!("未知地址类型 {atyp}"),
    };

    let target = format!("{host}:{port}");
    stats.begin_flow("SOCKS5", &target);

    let channel = match open_channel(&ssh, &host, port).await {
        Ok(ch) => ch,
        Err(e) => {
            stats.fail_flow();
            let _ = client
                .write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await;
            return Err(e);
        }
    };

    client
        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await?;

    let result = relay(client, channel, &stats).await;
    stats.end_flow();
    result
}

async fn handle_http_proxy(
    mut client: TcpStream,
    ssh: SshHandle,
    stats: Arc<TrafficStats>,
) -> Result<()> {
    let mut header = Vec::with_capacity(4096);
    let mut buf = [0u8; 1024];
    loop {
        let n = client.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        header.extend_from_slice(&buf[..n]);
        if header.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if header.len() > 64 * 1024 {
            anyhow::bail!("HTTP 请求头过大");
        }
    }

    let header_str = String::from_utf8_lossy(&header);
    let first_line = header_str.lines().next().unwrap_or("");
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");

    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = parse_host_port(target, 443)?;
        let dest = format!("{host}:{port}");
        stats.begin_flow("HTTPS", &dest);
        match open_channel(&ssh, &host, port).await {
            Ok(channel) => {
                client
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await?;
                let result = relay(client, channel, &stats).await;
                stats.end_flow();
                return result;
            }
            Err(e) => {
                stats.fail_flow();
                let body = format!("SSH tunnel ConnectFailed: {e:#}");
                let resp = format!(
                    "HTTP/1.1 502 Bad Gateway\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = client.write_all(resp.as_bytes()).await;
                return Err(e);
            }
        }
    }

    let (host, port, path) = parse_absolute_http_url(target)?;
    let dest = format!("{host}:{port}");
    stats.begin_flow("HTTP", &dest);
    let channel = match open_channel(&ssh, &host, port).await {
        Ok(ch) => ch,
        Err(e) => {
            stats.fail_flow();
            return Err(e);
        }
    };

    let rest = header_str
        .find("\r\n")
        .map(|i| &header_str[i..])
        .unwrap_or("\r\n\r\n");
    let rewritten = format!("{method} {path} HTTP/1.1{rest}");
    let body_start = header
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .unwrap_or(header.len());
    let leftover = &header[body_start..];

    let mut channel = channel.into_stream();
    channel.write_all(rewritten.as_bytes()).await?;
    stats.add_up(rewritten.len() as u64 + leftover.len() as u64);
    if !leftover.is_empty() {
        channel.write_all(leftover).await?;
    }

    let result = copy_counted(client, channel, &stats).await;
    stats.end_flow();
    result
}

fn parse_host_port(target: &str, default_port: u16) -> Result<(String, u16)> {
    if let Some((h, p)) = target.rsplit_once(':') {
        if !h.is_empty() && p.chars().all(|c| c.is_ascii_digit()) {
            let port: u16 = p.parse().context("端口解析失败")?;
            return Ok((h.trim_matches(|c| c == '[' || c == ']').to_string(), port));
        }
    }
    Ok((target.to_string(), default_port))
}

fn parse_absolute_http_url(url: &str) -> Result<(String, u16, String)> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("HTTP://"))
        .context("仅支持 http:// 绝对 URL 代理")?;
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{p}")),
        None => (rest, "/".to_string()),
    };
    let (host, port) = parse_host_port(authority, 80)?;
    Ok((host, port, path))
}

async fn relay(
    client: TcpStream,
    channel: russh::Channel<russh::client::Msg>,
    stats: &TrafficStats,
) -> Result<()> {
    let ssh_stream = channel.into_stream();
    copy_counted(client, ssh_stream, stats).await
}

async fn copy_counted<S>(
    client: TcpStream,
    ssh_stream: S,
    stats: &TrafficStats,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut client_r, mut client_w) = client.into_split();
    let (mut ssh_r, mut ssh_w) = tokio::io::split(ssh_stream);

    let up = async {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match client_r.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            if ssh_w.write_all(&buf[..n]).await.is_err() {
                break;
            }
            stats.add_up(n as u64);
        }
        let _ = ssh_w.shutdown().await;
    };

    let down = async {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match ssh_r.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            if client_w.write_all(&buf[..n]).await.is_err() {
                break;
            }
            stats.add_down(n as u64);
        }
        let _ = client_w.shutdown().await;
    };

    tokio::join!(up, down);
    Ok(())
}
