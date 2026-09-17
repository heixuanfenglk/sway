use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct ProxyEndpoints {
    pub http_host: String,
    pub http_port: u16,
    pub socks_host: String,
    pub socks_port: u16,
}

impl ProxyEndpoints {
    pub fn local(http_port: u16, socks_port: u16) -> Self {
        Self {
            http_host: "127.0.0.1".into(),
            http_port,
            socks_host: "127.0.0.1".into(),
            socks_port,
        }
    }

    pub fn http_url(&self) -> String {
        format!("http://{}:{}", self.http_host, self.http_port)
    }

    pub fn socks_url(&self) -> String {
        format!("socks5://{}:{}", self.socks_host, self.socks_port)
    }

    /// Windows 系统代理：只用 HTTP(S)，避免系统层再塞一层 SOCKS 造成双栈混乱。
    /// SOCKS 仍可通过 ALL_PROXY / 手动指定给 CLI。
    #[cfg(windows)]
    pub fn windows_proxy_server(&self) -> String {
        format!(
            "http={0}:{1};https={0}:{1}",
            self.http_host, self.http_port
        )
    }
}

pub fn enable_system_proxy(endpoints: &ProxyEndpoints, bypass_hosts: &[String]) -> Result<()> {
    #[cfg(windows)]
    {
        windows_impl::set_proxy(true, Some(endpoints), bypass_hosts)?;
        windows_impl::set_user_env_proxies(Some(endpoints), bypass_hosts)?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        macos_impl::set_proxy(true, Some(endpoints), bypass_hosts)?;
        Ok(())
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let _ = (endpoints, bypass_hosts);
        anyhow::bail!("当前平台尚未实现系统代理自动设置，请手动配置 HTTP/SOCKS 代理");
    }
}

pub fn disable_system_proxy() -> Result<()> {
    #[cfg(windows)]
    {
        windows_impl::set_proxy(false, None, &[])?;
        windows_impl::set_user_env_proxies(None, &[])?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        macos_impl::set_proxy(false, None, &[])?;
        Ok(())
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        Ok(())
    }
}

/// 系统代理是否指向本机指定端口（上次异常退出后的残留）。
pub fn is_stale_local_proxy(endpoints: &ProxyEndpoints) -> bool {
    #[cfg(windows)]
    {
        windows_impl::is_stale_local_proxy(endpoints)
    }
    #[cfg(target_os = "macos")]
    {
        macos_impl::is_stale_local_proxy(endpoints)
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let _ = endpoints;
        false
    }
}

/// 若系统代理仍指向本机这些端口，则关闭并清理环境变量。
/// 返回是否执行了清理。
pub fn cleanup_stale_local_proxy(endpoints: &ProxyEndpoints) -> Result<bool> {
    if !is_stale_local_proxy(endpoints) {
        return Ok(false);
    }
    tracing::warn!(
        "检测到残留系统代理 {}:{}，正在清除",
        endpoints.http_host,
        endpoints.http_port
    );
    disable_system_proxy()?;
    Ok(true)
}

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use winreg::enums::*;
    use winreg::RegKey;
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::Networking::WinInet::{
        InternetSetOptionW, INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
    };

    const INTERNET_SETTINGS: &str =
        r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    const ENV_KEY: &str = r"Environment";

    pub fn is_stale_local_proxy(endpoints: &ProxyEndpoints) -> bool {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok(key) = hkcu.open_subkey(INTERNET_SETTINGS) {
            let enabled: u32 = key.get_value("ProxyEnable").unwrap_or(0);
            if enabled == 1 {
                if let Ok(server) = key.get_value::<String, _>("ProxyServer") {
                    if server_points_to(endpoints, &server) {
                        return true;
                    }
                }
            }
        }
        if let Ok(env) = hkcu.open_subkey(ENV_KEY) {
            for name in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy", "ALL_PROXY", "all_proxy"] {
                if let Ok(v) = env.get_value::<String, _>(name) {
                    if env_points_to(endpoints, &v) {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn server_points_to(ep: &ProxyEndpoints, server: &str) -> bool {
        let s = server.to_ascii_lowercase();
        let http = format!("{}:{}", ep.http_host, ep.http_port);
        let socks = format!("{}:{}", ep.socks_host, ep.socks_port);
        s.contains(&http.to_ascii_lowercase()) || s.contains(&socks.to_ascii_lowercase())
    }

    fn env_points_to(ep: &ProxyEndpoints, value: &str) -> bool {
        let v = value.to_ascii_lowercase();
        v.contains(&format!("127.0.0.1:{}", ep.http_port))
            || v.contains(&format!("localhost:{}", ep.http_port))
            || v.contains(&format!("127.0.0.1:{}", ep.socks_port))
            || v.contains(&format!("localhost:{}", ep.socks_port))
    }

    pub fn set_proxy(
        enable: bool,
        endpoints: Option<&ProxyEndpoints>,
        bypass_hosts: &[String],
    ) -> Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu
            .create_subkey(INTERNET_SETTINGS)
            .context("打开 Internet Settings 失败")?;

        if enable {
            let ep = endpoints.context("启用代理时需要 endpoints")?;
            key.set_value("ProxyEnable", &1u32)
                .context("写入 ProxyEnable 失败")?;
            key.set_value("ProxyServer", &ep.windows_proxy_server())
                .context("写入 ProxyServer 失败")?;
            let mut overrides = String::from("localhost;127.*;10.*;192.168.*;<local>");
            for h in bypass_hosts {
                let h = h.trim();
                if !h.is_empty() {
                    overrides.push(';');
                    overrides.push_str(h);
                }
            }
            let _ = key.set_value("ProxyOverride", &overrides);
        } else {
            key.set_value("ProxyEnable", &0u32)
                .context("关闭 ProxyEnable 失败")?;
            // 清掉地址，避免下次误开或其它软件读到死代理
            let _ = key.delete_value("ProxyServer");
        }

        notify_proxy_change()?;
        Ok(())
    }

    pub fn set_user_env_proxies(
        endpoints: Option<&ProxyEndpoints>,
        bypass_hosts: &[String],
    ) -> Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (env, _) = hkcu
            .create_subkey(ENV_KEY)
            .context("打开用户 Environment 失败")?;

        let keys = [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "NO_PROXY",
            "no_proxy",
        ];

        if let Some(ep) = endpoints {
            let http = ep.http_url();
            let socks = ep.socks_url();
            env.set_value("HTTP_PROXY", &http)?;
            env.set_value("HTTPS_PROXY", &http)?;
            env.set_value("http_proxy", &http)?;
            env.set_value("https_proxy", &http)?;
            env.set_value("ALL_PROXY", &socks)?;
            env.set_value("all_proxy", &socks)?;
            let mut no_proxy = String::from("localhost,127.0.0.1,::1");
            for h in bypass_hosts {
                let h = h.trim();
                if !h.is_empty() {
                    no_proxy.push(',');
                    no_proxy.push_str(h);
                }
            }
            env.set_value("NO_PROXY", &no_proxy)?;
            env.set_value("no_proxy", &no_proxy)?;
        } else {
            for k in keys {
                let _ = env.delete_value(k);
            }
        }

        broadcast_env_change()?;
        Ok(())
    }

    fn notify_proxy_change() -> Result<()> {
        unsafe {
            let _ = InternetSetOptionW(None, INTERNET_OPTION_SETTINGS_CHANGED, None, 0);
            let _ = InternetSetOptionW(None, INTERNET_OPTION_REFRESH, None, 0);
        }
        Ok(())
    }

    fn broadcast_env_change() -> Result<()> {
        let wide: Vec<u16> = OsStr::new("Environment")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let mut result = 0usize;
            let _ = SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                WPARAM(0),
                LPARAM(wide.as_ptr() as isize),
                SMTO_ABORTIFHUNG,
                3000,
                Some(&mut result),
            );
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod macos_impl {
    use super::*;
    use std::process::Command;

    pub fn set_proxy(
        enable: bool,
        endpoints: Option<&ProxyEndpoints>,
        bypass_hosts: &[String],
    ) -> Result<()> {
        let services = network_services()?;
        if services.is_empty() {
            anyhow::bail!("未找到可用的 macOS 网络服务");
        }

        let mut last_err: Option<anyhow::Error> = None;
        let mut ok = 0usize;

        for service in &services {
            let result = if enable {
                let ep = endpoints.context("启用代理时需要 endpoints")?;
                enable_for_service(service, ep, bypass_hosts)
            } else {
                disable_for_service(service)
            };
            match result {
                Ok(()) => ok += 1,
                Err(e) => last_err = Some(e),
            }
        }

        if ok == 0 {
            return Err(last_err.unwrap_or_else(|| {
                anyhow::anyhow!("设置 macOS 系统代理失败（可能需要管理员权限）")
            }));
        }
        Ok(())
    }

    pub fn is_stale_local_proxy(endpoints: &ProxyEndpoints) -> bool {
        let Ok(services) = network_services() else {
            return false;
        };
        for service in services {
            if proxy_enabled_on(&service, "getwebproxy", endpoints.http_port)
                || proxy_enabled_on(&service, "getsecurewebproxy", endpoints.http_port)
                || proxy_enabled_on(&service, "getsocksfirewallproxy", endpoints.socks_port)
            {
                return true;
            }
        }
        false
    }

    fn proxy_enabled_on(service: &str, get_flag: &str, port: u16) -> bool {
        let out = Command::new("networksetup")
            .args([format!("-{get_flag}"), service.to_string()])
            .output();
        let Ok(out) = out else {
            return false;
        };
        if !out.status.success() {
            return false;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let enabled = text.lines().any(|l| {
            let l = l.trim().to_ascii_lowercase();
            l.starts_with("enabled:") && l.contains("yes")
        });
        if !enabled {
            return false;
        }
        let port_s = port.to_string();
        text.lines().any(|l| {
            let l = l.trim().to_ascii_lowercase();
            (l.starts_with("port:") && l.contains(&port_s))
                || (l.starts_with("server:")
                    && (l.contains("127.0.0.1") || l.contains("localhost")))
        }) && text.lines().any(|l| {
            let l = l.trim().to_ascii_lowercase();
            l.starts_with("server:") && (l.contains("127.0.0.1") || l.contains("localhost"))
        }) && text.lines().any(|l| {
            let l = l.trim().to_ascii_lowercase();
            l.starts_with("port:") && l.contains(&port_s)
        })
    }

    fn network_services() -> Result<Vec<String>> {
        let out = Command::new("networksetup")
            .arg("-listallnetworkservices")
            .output()
            .context("无法执行 networksetup")?;
        if !out.status.success() {
            anyhow::bail!(
                "networksetup 失败: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let services = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .filter(|l| !l.starts_with('*')) // 跳过说明行 / 禁用服务
            .filter(|l| !l.contains("An asterisk"))
            .map(|s| s.to_string())
            .collect();
        Ok(services)
    }

    fn enable_for_service(
        service: &str,
        ep: &ProxyEndpoints,
        bypass_hosts: &[String],
    ) -> Result<()> {
        run_ns(&[
            "-setwebproxy",
            service,
            &ep.http_host,
            &ep.http_port.to_string(),
        ])?;
        run_ns(&[
            "-setsecurewebproxy",
            service,
            &ep.http_host,
            &ep.http_port.to_string(),
        ])?;
        run_ns(&[
            "-setsocksfirewallproxy",
            service,
            &ep.socks_host,
            &ep.socks_port.to_string(),
        ])?;
        run_ns(&["-setwebproxystate", service, "on"])?;
        run_ns(&["-setsecurewebproxystate", service, "on"])?;
        run_ns(&["-setsocksfirewallproxystate", service, "on"])?;

        let mut bypass = vec![
            "127.0.0.1".to_string(),
            "localhost".to_string(),
            "*.local".to_string(),
        ];
        for h in bypass_hosts {
            let h = h.trim();
            if !h.is_empty() {
                bypass.push(h.to_string());
            }
        }
        let mut args = vec!["-setproxybypassdomains".to_string(), service.to_string()];
        args.extend(bypass);
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        run_ns(&args_ref)?;
        Ok(())
    }

    fn disable_for_service(service: &str) -> Result<()> {
        run_ns(&["-setwebproxystate", service, "off"])?;
        run_ns(&["-setsecurewebproxystate", service, "off"])?;
        run_ns(&["-setsocksfirewallproxystate", service, "off"])?;
        Ok(())
    }

    fn run_ns(args: &[&str]) -> Result<()> {
        let out = Command::new("networksetup")
            .args(args)
            .output()
            .context("无法执行 networksetup")?;
        if !out.status.success() {
            anyhow::bail!(
                "networksetup {} 失败: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }
}
