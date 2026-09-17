use crate::config::{AppConfig, AuthMethod, TunnelRequest};
use crate::keys;
use crate::proxy::{self, ProxyEndpoints};
use crate::stats::{self, TrafficSnapshot, TrafficStats};
use crate::tunnel::{self, TunnelHandle};
use eframe::egui::{self, Align, Layout, RichText, Vec2};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

const LABEL_W: f32 = 72.0;
const PAGE_PAD: f32 = 12.0;
const CARD_GAP: f32 = 10.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConnState {
    Off,
    Connecting,
    On,
    Error,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Connect,
    Hosts,
    Settings,
}

pub struct ProxyApp {
    rt: Arc<Runtime>,
    config: AppConfig,
    page: Page,
    state: ConnState,
    status_text: String,
    tunnel: Arc<Mutex<Option<TunnelHandle>>>,
    live_stats: Arc<Mutex<Option<Arc<TrafficStats>>>>,
    last_error: Arc<Mutex<Option<String>>>,
    connect_started_at: Option<Instant>,
    connected_profile_id: Option<String>,
    show_password: bool,
    rate_prev: TrafficSnapshot,
    rate_prev_at: Instant,
    up_rate: f64,
    down_rate: f64,
    cached_snap: TrafficSnapshot,
    toast: Option<(String, Instant)>,
    /// 本进程是否已成功写入系统代理（退出/断开时必须清掉）
    system_proxy_held: Arc<AtomicBool>,
}

impl ProxyApp {
    pub fn new(rt: Arc<Runtime>) -> Self {
        let config = AppConfig::load();
        let endpoints = ProxyEndpoints::local(config.settings.http_port, config.settings.socks_port);
        let mut status_text = "未连接".to_string();
        match proxy::cleanup_stale_local_proxy(&endpoints) {
            Ok(true) => {
                status_text = "已清除上次残留的系统代理".into();
                tracing::info!("启动时已清除残留系统代理");
            }
            Ok(false) => {}
            Err(e) => tracing::warn!("启动时检查/清除系统代理失败: {e:#}"),
        }

        Self {
            rt,
            config,
            page: Page::Connect,
            state: ConnState::Off,
            status_text,
            tunnel: Arc::new(Mutex::new(None)),
            live_stats: Arc::new(Mutex::new(None)),
            last_error: Arc::new(Mutex::new(None)),
            connect_started_at: None,
            connected_profile_id: None,
            show_password: false,
            rate_prev: TrafficSnapshot::default(),
            rate_prev_at: Instant::now(),
            up_rate: 0.0,
            down_rate: 0.0,
            cached_snap: TrafficSnapshot::default(),
            toast: None,
            system_proxy_held: Arc::new(AtomicBool::new(false)),
        }
    }

    fn endpoints(&self) -> ProxyEndpoints {
        ProxyEndpoints::local(
            self.config.settings.http_port,
            self.config.settings.socks_port,
        )
    }

    fn show_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    fn save_config(&mut self) {
        match self.config.save() {
            Ok(()) => self.show_toast("配置已保存"),
            Err(e) => {
                self.status_text = format!("保存失败: {e}");
                self.show_toast(format!("保存失败: {e}"));
            }
        }
    }

    fn create_keypair_for_active(&mut self) {
        let suggested = dirs::home_dir()
            .map(|h| h.join(".ssh").join("sway_ed25519"))
            .unwrap_or_else(|| PathBuf::from("sway_ed25519"));

        let Some(path) = rfd::FileDialog::new()
            .set_title("选择私钥保存路径")
            .set_file_name(
                suggested
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("sway_ed25519"),
            )
            .set_directory(suggested.parent().unwrap_or(std::path::Path::new(".")))
            .save_file()
        else {
            return;
        };

        let comment = {
            let p = self.config.active_profile();
            let user = p.username.trim();
            let host = p.host.trim();
            if !user.is_empty() && !host.is_empty() {
                format!("{user}@{host}")
            } else if !user.is_empty() {
                user.to_string()
            } else {
                "sway".into()
            }
        };

        match keys::generate_ed25519_keypair(&path, &comment) {
            Ok(_) => {
                self.config.active_profile_mut().private_key_path =
                    path.to_string_lossy().to_string();
                self.config.active_profile_mut().auth_method = AuthMethod::PrivateKey;
                let _ = self.config.save();
                self.show_toast(format!("已生成密钥对：{}", path.display()));
            }
            Err(e) => self.show_toast(format!("生成失败: {e:#}")),
        }
    }

    fn path_nodes(&self) -> [String; 4] {
        let p = self.config.active_profile();
        let user = p.username.trim();
        let host = p.host.trim();
        let ssh = if user.is_empty() || host.is_empty() {
            "SSH".to_string()
        } else {
            format!("{user}@{host}:{}", p.port)
        };
        [
            "本机".into(),
            format!(
                "HTTP:{} / SOCKS:{}",
                self.config.settings.http_port, self.config.settings.socks_port
            ),
            ssh,
            "目标站点".into(),
        ]
    }

    fn is_busy(&self) -> bool {
        matches!(self.state, ConnState::Connecting | ConnState::On)
    }

    fn start(&mut self) {
        if self.state == ConnState::Connecting || self.state == ConnState::On {
            return;
        }

        let profile = self.config.active_profile().clone();
        let settings = self.config.settings.clone();
        let req = match TunnelRequest::from_profile(&profile, &settings) {
            Ok(r) => r,
            Err(e) => {
                self.state = ConnState::Error;
                self.status_text = format!("配置错误: {e}");
                self.page = if e.to_string().contains("端口") {
                    Page::Settings
                } else {
                    Page::Hosts
                };
                return;
            }
        };

        self.state = ConnState::Connecting;
        self.status_text = format!("正在连接 {}…", profile.display_label());
        self.connect_started_at = Some(Instant::now());
        self.connected_profile_id = Some(profile.id.clone());
        self.up_rate = 0.0;
        self.down_rate = 0.0;
        self.cached_snap = TrafficSnapshot::default();
        self.rate_prev = TrafficSnapshot::default();
        self.rate_prev_at = Instant::now();
        if let Ok(mut e) = self.last_error.try_lock() {
            *e = None;
        }
        if let Ok(mut s) = self.live_stats.try_lock() {
            *s = None;
        }

        let tunnel_slot = Arc::clone(&self.tunnel);
        let live_stats = Arc::clone(&self.live_stats);
        let last_error = Arc::clone(&self.last_error);
        let auto_proxy = settings.auto_set_system_proxy;
        let endpoints = self.endpoints();
        let bypass = vec![profile.host.trim().to_string()];
        let proxy_held = Arc::clone(&self.system_proxy_held);

        self.rt.spawn(async move {
            match tunnel::start_tunnel(req).await {
                Ok(handle) => {
                    *live_stats.lock().await = Some(Arc::clone(&handle.stats));
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    if auto_proxy {
                        match proxy::enable_system_proxy(&endpoints, &bypass) {
                            Ok(()) => {
                                proxy_held.store(true, Ordering::SeqCst);
                            }
                            Err(e) => {
                                tracing::error!("设置系统代理失败: {e:#}");
                                *last_error.lock().await =
                                    Some(format!("隧道已建立，但设置系统代理失败: {e}"));
                            }
                        }
                    }
                    *tunnel_slot.lock().await = Some(handle);
                }
                Err(e) => {
                    tracing::error!("启动失败: {e:#}");
                    *tunnel_slot.lock().await = None;
                    *live_stats.lock().await = None;
                    *last_error.lock().await = Some(format!("{e:#}"));
                }
            }
        });
    }

    fn stop(&mut self) {
        let tunnel_slot = Arc::clone(&self.tunnel);
        let live_stats = Arc::clone(&self.live_stats);
        let proxy_held = Arc::clone(&self.system_proxy_held);
        let endpoints = self.endpoints();
        self.state = ConnState::Off;
        self.status_text = "已断开".into();
        self.connected_profile_id = None;
        self.up_rate = 0.0;
        self.down_rate = 0.0;

        self.rt.spawn(async move {
            let handle = tunnel_slot.lock().await.take();
            *live_stats.lock().await = None;
            if let Some(h) = handle {
                h.stop().await;
            }
            let held = proxy_held.swap(false, Ordering::SeqCst);
            let stale = proxy::is_stale_local_proxy(&endpoints);
            if held || stale {
                if let Err(e) = proxy::disable_system_proxy() {
                    tracing::error!("清除系统代理失败: {e:#}");
                    if held {
                        proxy_held.store(true, Ordering::SeqCst);
                    }
                }
            }
        });
    }

    fn switch_profile(&mut self, id: &str) {
        if id == self.config.active_profile_id {
            return;
        }
        if self.is_busy() {
            self.stop();
            self.show_toast("已断开当前连接，可重新开启代理");
        }
        if self.config.set_active(id) {
            let label = self.config.active_profile().display_label();
            self.status_text = format!("已切换到 {label}");
        }
    }

    fn refresh_traffic(&mut self) {
        let snap = if let Ok(guard) = self.live_stats.try_lock() {
            guard.as_ref().map(|s| s.snapshot()).unwrap_or_default()
        } else {
            return;
        };

        let now = Instant::now();
        let dt = now.duration_since(self.rate_prev_at).as_secs_f64();
        if dt >= 0.4 {
            let dup = snap.bytes_up.saturating_sub(self.rate_prev.bytes_up) as f64 / dt;
            let ddown = snap.bytes_down.saturating_sub(self.rate_prev.bytes_down) as f64 / dt;
            self.up_rate = dup;
            self.down_rate = ddown;
            self.rate_prev = snap.clone();
            self.rate_prev_at = now;
        }
        self.cached_snap = snap;
    }

    fn sync_state_from_tunnel(&mut self) {
        if let Ok(mut err) = self.last_error.try_lock() {
            if let Some(msg) = err.take() {
                if self.state == ConnState::Connecting {
                    self.state = ConnState::Error;
                    self.status_text = format!("连接失败: {msg}");
                    self.connect_started_at = None;
                    self.connected_profile_id = None;
                    return;
                } else if self.state == ConnState::On {
                    self.status_text = msg;
                }
            }
        }

        let tunnel_present = self.tunnel.try_lock().ok().map(|g| g.is_some());
        if let Some(has) = tunnel_present {
            match (self.state, has) {
                (ConnState::Connecting, true) => {
                    self.state = ConnState::On;
                    self.connect_started_at = None;
                    let label = self.config.active_profile().display_label();
                    self.status_text = format!(
                        "已连接 · {label} · HTTP :{} / SOCKS :{}",
                        self.config.settings.http_port, self.config.settings.socks_port
                    );
                    self.rate_prev_at = Instant::now();
                    self.show_toast(format!("已连接 {label}"));
                }
                (ConnState::On, false) => {
                    self.state = ConnState::Off;
                    self.status_text = "连接已结束".into();
                    self.connected_profile_id = None;
                }
                (ConnState::Connecting, false) => {
                    if let Some(started) = self.connect_started_at {
                        if started.elapsed() > std::time::Duration::from_secs(45) {
                            self.state = ConnState::Error;
                            self.status_text = "连接超时，请检查主机与认证信息".into();
                            self.connect_started_at = None;
                            self.connected_profile_id = None;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // ─── shared chrome ───────────────────────────────────────────

    fn ui_nav(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            nav_tab(ui, &mut self.page, Page::Connect, "连接");
            nav_tab(ui, &mut self.page, Page::Hosts, "主机");
            nav_tab(ui, &mut self.page, Page::Settings, "设置");

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(4.0);
                let (text, color) = match self.state {
                    ConnState::Off => ("未连接", egui::Color32::GRAY),
                    ConnState::Connecting => ("连接中", egui::Color32::from_rgb(220, 160, 40)),
                    ConnState::On => ("已连接", egui::Color32::from_rgb(40, 170, 90)),
                    ConnState::Error => ("错误", egui::Color32::from_rgb(220, 70, 70)),
                };
                status_pill(ui, text, color);
            });
        });
    }

    fn ui_toast_overlay(&mut self, ctx: &egui::Context) {
        let Some((msg, at)) = &self.toast else {
            return;
        };
        if at.elapsed().as_secs_f32() >= 2.4 {
            self.toast = None;
            return;
        }
        let msg = msg.clone();
        egui::Area::new(egui::Id::new("toast"))
            .anchor(egui::Align2::CENTER_TOP, [0.0, 52.0])
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_unmultiplied(32, 36, 44, 230))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(14, 8))
                    .shadow(egui::Shadow {
                        offset: [0, 2],
                        blur: 8,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(60),
                    })
                    .show(ui, |ui| {
                        ui.label(RichText::new(msg).color(egui::Color32::from_rgb(200, 230, 200)));
                    });
            });
    }

    // ─── Connect ─────────────────────────────────────────────────

    fn ui_connect_page(&mut self, ui: &mut egui::Ui) {
        ui.add_space(PAGE_PAD);

        // 控制条：主机 + 密码 + 开关
        card(ui, |ui| {
            form_row(ui, "主机", |ui| {
                let active_id = self.config.active_profile_id.clone();
                let labels: Vec<(String, String)> = self
                    .config
                    .profiles
                    .iter()
                    .map(|p| (p.id.clone(), p.display_label()))
                    .collect();
                let current = labels
                    .iter()
                    .find(|(id, _)| id == &active_id)
                    .map(|(_, l)| l.clone())
                    .unwrap_or_else(|| "未选择".into());

                let combo_w = (ui.available_width() - 88.0).max(160.0);
                egui::ComboBox::from_id_salt("host_switcher")
                    .selected_text(current)
                    .width(combo_w)
                    .show_ui(ui, |ui| {
                        for (id, label) in &labels {
                            let selected = id == &active_id;
                            if ui.selectable_label(selected, label).clicked() && !selected {
                                self.switch_profile(id);
                            }
                        }
                    });
                if ui
                    .add_sized([80.0, 24.0], egui::Button::new("管理…"))
                    .clicked()
                {
                    self.page = Page::Hosts;
                }
            });

            if self.config.active_profile().auth_method == AuthMethod::Password {
                ui.add_space(6.0);
                form_row(ui, "密码", |ui| {
                    let w = (ui.available_width() - 64.0).max(120.0);
                    {
                        let profile = self.config.active_profile_mut();
                        ui.add(
                            egui::TextEdit::singleline(&mut profile.password)
                                .password(!self.show_password)
                                .desired_width(w)
                                .hint_text("仅本次会话"),
                        );
                    }
                    ui.checkbox(&mut self.show_password, "显示");
                });
            }

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(10.0);

            let enabled = matches!(self.state, ConnState::On | ConnState::Connecting);
            ui.horizontal(|ui| {
                let switch_label = match self.state {
                    ConnState::On => "已连接",
                    ConnState::Connecting => "连接中",
                    _ => "连接",
                };
                let switch = egui::Button::new(
                    RichText::new(switch_label)
                        .strong()
                        .size(16.0)
                        .color(egui::Color32::WHITE),
                )
                .fill(if enabled {
                    egui::Color32::from_rgb(40, 170, 90)
                } else {
                    egui::Color32::from_rgb(120, 120, 120)
                })
                .min_size(Vec2::new(120.0, 36.0));

                if ui
                    .add_enabled(self.state != ConnState::Connecting, switch)
                    .clicked()
                {
                    if enabled {
                        self.stop();
                    } else {
                        self.start();
                    }
                }

                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.add_space(4.0);
                    ui.label(RichText::new(&self.status_text).size(13.0));
                    ui.weak(format!(
                        "本地  HTTP 127.0.0.1:{}  ·  SOCKS 127.0.0.1:{}",
                        self.config.settings.http_port, self.config.settings.socks_port
                    ));
                });
            });
        });

        ui.add_space(CARD_GAP);

        // 路径
        card(ui, |ui| {
            section_title(ui, "连接路径");
            ui.add_space(8.0);
            let active = self.state == ConnState::On;
            let nodes = self.path_nodes();
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for (i, node) in nodes.iter().enumerate() {
                    if i > 0 {
                        ui.label(RichText::new("→").color(egui::Color32::GRAY));
                    }
                    path_chip(ui, node, active);
                }
            });
        });

        ui.add_space(CARD_GAP);

        // 流量指标 + 最近目标（下半部分吃满剩余高度）
        let snap = self.cached_snap.clone();
        let remain = ui.available_height() - PAGE_PAD;
        card_fill(ui, remain.max(180.0), |ui| {
            section_title(ui, "流量");
            ui.add_space(8.0);

            // 4 列指标
            let metrics = [
                (
                    "上行",
                    format!(
                        "{}\n{}",
                        stats::format_bytes(snap.bytes_up),
                        stats::format_rate(self.up_rate)
                    ),
                ),
                (
                    "下行",
                    format!(
                        "{}\n{}",
                        stats::format_bytes(snap.bytes_down),
                        stats::format_rate(self.down_rate)
                    ),
                ),
                (
                    "合计",
                    stats::format_bytes(snap.bytes_up.saturating_add(snap.bytes_down)),
                ),
                (
                    "连接",
                    format!(
                        "活跃 {}\n累计 {} / 失败 {}",
                        snap.active_conns, snap.total_conns, snap.failed_conns
                    ),
                ),
            ];
            let n = metrics.len() as f32;
            let gap = 8.0;
            let cell_w = ((ui.available_width() - gap * (n - 1.0)) / n).max(90.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (title, value) in &metrics {
                    metric_cell(ui, cell_w, title, value);
                }
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("时长");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.monospace(if self.state == ConnState::On {
                        stats::format_duration(snap.uptime_secs)
                    } else {
                        "--:--".into()
                    });
                });
            });

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(6.0);
            section_title(ui, "最近目标");
            ui.add_space(4.0);

            let list_h = (ui.available_height() - 4.0).max(60.0);
            egui::ScrollArea::vertical()
                .max_height(list_h)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if snap.recent.is_empty() {
                        ui.add_space(8.0);
                        ui.weak(if self.state == ConnState::On {
                            "暂无流量，打开浏览器或使用 curl 试试"
                        } else {
                            "连接后将显示经过代理的目标地址"
                        });
                    } else {
                        for ev in &snap.recent {
                            let age = ev.at.elapsed().as_secs();
                            let age_txt = if age < 60 {
                                format!("{age:>2}s")
                            } else {
                                format!("{:>2}m", age / 60)
                            };
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    [36.0, 18.0],
                                    egui::Label::new(
                                        RichText::new(age_txt)
                                            .weak()
                                            .monospace()
                                            .size(12.0),
                                    ),
                                );
                                ui.add_sized(
                                    [52.0, 18.0],
                                    egui::Label::new(
                                        RichText::new(format!("[{}]", ev.proto))
                                            .color(egui::Color32::from_rgb(70, 130, 180))
                                            .size(12.0),
                                    ),
                                );
                                ui.monospace(&ev.target);
                            });
                        }
                    }
                });
        });

        ui.add_space(PAGE_PAD);
    }

    // ─── Hosts ───────────────────────────────────────────────────

    fn ui_hosts_page(&mut self, ui: &mut egui::Ui) {
        ui.add_space(PAGE_PAD);

        // 工具栏
        card(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("＋ 新建").clicked() {
                    if self.is_busy() {
                        self.stop();
                    }
                    self.config.add_profile();
                    self.show_toast("已新建主机");
                }
                if ui.button("复制").clicked() {
                    if self.is_busy() {
                        self.stop();
                    }
                    self.config.duplicate_active();
                    self.show_toast("已复制主机");
                }
                if ui.button("删除").clicked() {
                    if self.is_busy() {
                        self.stop();
                    }
                    match self.config.remove_active() {
                        Ok(()) => self.show_toast("已删除主机"),
                        Err(e) => self.show_toast(e.to_string()),
                    }
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_enabled(!self.is_busy(), egui::Button::new("保存并连接"))
                        .clicked()
                    {
                        self.save_config();
                        self.page = Page::Connect;
                        self.start();
                    }
                    if ui.button("保存").clicked() {
                        self.save_config();
                    }
                });
            });
        });

        ui.add_space(CARD_GAP);

        let body_h = (ui.available_height() - PAGE_PAD).max(240.0);
        let list_w = 200.0f32;

        ui.horizontal(|ui| {
            ui.set_min_height(body_h);
            ui.spacing_mut().item_spacing.x = CARD_GAP;

            // 左侧列表
            ui.allocate_ui_with_layout(
                Vec2::new(list_w, body_h),
                Layout::top_down(Align::Min),
                |ui| {
                    card_fill(ui, body_h, |ui| {
                        section_title(ui, "主机列表");
                        ui.add_space(6.0);
                        let list_h = ui.available_height();
                        egui::ScrollArea::vertical()
                            .max_height(list_h)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let items: Vec<(String, String, bool)> = self
                                    .config
                                    .profiles
                                    .iter()
                                    .map(|p| {
                                        (
                                            p.id.clone(),
                                            p.list_title(),
                                            p.id == self.config.active_profile_id,
                                        )
                                    })
                                    .collect();
                                for (id, title, selected) in items {
                                    let connected = self.state == ConnState::On
                                        && self.connected_profile_id.as_deref()
                                            == Some(id.as_str());
                                    let text = if connected {
                                        format!("● {title}")
                                    } else {
                                        title
                                    };
                                    let row = ui.add_sized(
                                        [ui.available_width(), 28.0],
                                        egui::SelectableLabel::new(selected, text),
                                    );
                                    if row.clicked() {
                                        self.switch_profile(&id);
                                    }
                                }
                            });
                    });
                },
            );

            // 右侧编辑
            let edit_w = ui.available_width();
            ui.allocate_ui_with_layout(
                Vec2::new(edit_w, body_h),
                Layout::top_down(Align::Min),
                |ui| {
                    card_fill(ui, body_h, |ui| {
                        section_title(ui, "编辑主机");
                        ui.add_space(6.0);

                        let busy = self.is_busy()
                            && self.connected_profile_id.as_deref()
                                == Some(self.config.active_profile_id.as_str());
                        if busy {
                            ui.colored_label(
                                egui::Color32::from_rgb(220, 160, 40),
                                "当前主机已连接，修改后需断开再连",
                            );
                            ui.add_space(6.0);
                        }

                        let field_w = (ui.available_width() - LABEL_W - 8.0).max(120.0);

                        form_row(ui, "名称", |ui| {
                            let p = self.config.active_profile_mut();
                            ui.add(
                                egui::TextEdit::singleline(&mut p.name)
                                    .desired_width(field_w)
                                    .hint_text("例如：香港 VPS"),
                            );
                        });
                        ui.add_space(6.0);
                        form_row(ui, "主机", |ui| {
                            let p = self.config.active_profile_mut();
                            ui.add(
                                egui::TextEdit::singleline(&mut p.host)
                                    .desired_width(field_w)
                                    .hint_text("IP 或域名"),
                            );
                        });
                        ui.add_space(6.0);
                        form_row(ui, "端口", |ui| {
                            let p = self.config.active_profile_mut();
                            ui.add(egui::DragValue::new(&mut p.port).range(1..=65535));
                        });
                        ui.add_space(6.0);
                        form_row(ui, "用户名", |ui| {
                            let p = self.config.active_profile_mut();
                            ui.add(
                                egui::TextEdit::singleline(&mut p.username).desired_width(field_w),
                            );
                        });
                        ui.add_space(6.0);
                        form_row(ui, "认证", |ui| {
                            let p = self.config.active_profile_mut();
                            ui.selectable_value(&mut p.auth_method, AuthMethod::Password, "密码");
                            ui.selectable_value(
                                &mut p.auth_method,
                                AuthMethod::PrivateKey,
                                "私钥",
                            );
                        });

                        ui.add_space(6.0);
                        if self.config.active_profile().auth_method == AuthMethod::Password {
                            form_row(ui, "密码", |ui| {
                                let w = (ui.available_width() - 64.0).max(100.0);
                                {
                                    let p = self.config.active_profile_mut();
                                    ui.add(
                                        egui::TextEdit::singleline(&mut p.password)
                                            .desired_width(w)
                                            .password(!self.show_password)
                                            .hint_text("不写入磁盘"),
                                    );
                                }
                                ui.checkbox(&mut self.show_password, "显示");
                            });
                        } else {
                            form_row(ui, "私钥", |ui| {
                                let w = (ui.available_width() - 148.0).max(80.0);
                                {
                                    let p = self.config.active_profile_mut();
                                    ui.add(
                                        egui::TextEdit::singleline(&mut p.private_key_path)
                                            .desired_width(w),
                                    );
                                }
                                if ui.button("浏览…").clicked() {
                                    if let Some(path) = rfd::FileDialog::new().pick_file() {
                                        self.config.active_profile_mut().private_key_path =
                                            path.to_string_lossy().to_string();
                                    }
                                }
                                if ui.button("创建密钥对").clicked() {
                                    self.create_keypair_for_active();
                                }
                            });
                        }

                        ui.add_space(ui.available_height().max(0.0));
                    });
                },
            );
        });

        ui.add_space(PAGE_PAD);
    }

    // ─── Settings ────────────────────────────────────────────────

    fn ui_settings_page(&mut self, ui: &mut egui::Ui) {
        ui.add_space(PAGE_PAD);

        let locked = self.is_busy();

        card(ui, |ui| {
            ui.horizontal(|ui| {
                section_title(ui, "本地端口");
                if locked {
                    ui.add_space(12.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 160, 40),
                        "运行中不可改",
                    );
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_sized([88.0, 28.0], egui::Button::new("保存"))
                        .clicked()
                    {
                        if let Err(e) = self.config.settings.validate() {
                            self.show_toast(format!("设置无效: {e}"));
                        } else {
                            self.save_config();
                        }
                    }
                });
            });

            ui.add_space(12.0);
            ui.add_enabled_ui(!locked, |ui| {
                let w = ui.available_width();
                port_setting_card(ui, w, "HTTP", &mut self.config.settings.http_port);
                ui.add_space(8.0);
                port_setting_card(ui, w, "SOCKS5", &mut self.config.settings.socks_port);
            });
        });

        ui.add_space(CARD_GAP);

        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("自动设置系统代理").strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let mut on = self.config.settings.auto_set_system_proxy;
                    if ui
                        .add_sized(
                            [72.0, 28.0],
                            egui::Button::new(
                                RichText::new(if on { "开" } else { "关" })
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(if on {
                                egui::Color32::from_rgb(40, 170, 90)
                            } else {
                                egui::Color32::from_rgb(140, 140, 140)
                            }),
                        )
                        .clicked()
                    {
                        on = !on;
                        self.config.settings.auto_set_system_proxy = on;
                    }
                });
            });
            ui.add_space(8.0);
            ui.label(
                RichText::new("若关掉程序后无法上网，多半是系统代理未清干净。可点下方按钮强制清除。")
                    .small()
                    .color(egui::Color32::GRAY),
            );
            ui.add_space(6.0);
            if ui.button("强制清除系统代理").clicked() {
                match proxy::disable_system_proxy() {
                    Ok(()) => {
                        self.system_proxy_held.store(false, Ordering::SeqCst);
                        self.show_toast("已清除系统代理与相关环境变量");
                    }
                    Err(e) => self.show_toast(format!("清除失败: {e}")),
                }
            }
        });

        ui.add_space(PAGE_PAD);
    }
}

impl eframe::App for ProxyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.sync_state_from_tunnel();
        self.refresh_traffic();
        ctx.request_repaint_after(std::time::Duration::from_millis(200));

        egui::TopBottomPanel::top("nav")
            .exact_height(44.0)
            .show_separator_line(true)
            .show(ctx, |ui| {
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    self.ui_nav(ui);
                });
            });

        self.ui_toast_overlay(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            // 主机页自己吃满高度；其它页允许滚动
            match self.page {
                Page::Hosts => self.ui_hosts_page(ui),
                Page::Settings => self.ui_settings_page(ui),
                Page::Connect => {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.ui_connect_page(ui));
                }
            }
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let tunnel_slot = Arc::clone(&self.tunnel);
        let live_stats = Arc::clone(&self.live_stats);
        let proxy_held = Arc::clone(&self.system_proxy_held);
        let endpoints = self.endpoints();
        self.rt.block_on(async move {
            *live_stats.lock().await = None;
            if let Some(h) = tunnel_slot.lock().await.take() {
                h.stop().await;
            }
            let held = proxy_held.swap(false, Ordering::SeqCst);
            let stale = proxy::is_stale_local_proxy(&endpoints);
            if held || stale {
                if let Err(e) = proxy::disable_system_proxy() {
                    tracing::error!("退出时清除系统代理失败: {e:#}");
                }
            }
        });
    }
}

// ─── layout helpers ──────────────────────────────────────────────

fn nav_tab(ui: &mut egui::Ui, page: &mut Page, target: Page, label: &str) {
    let selected = *page == target;
    let text = if selected {
        RichText::new(label).strong()
    } else {
        RichText::new(label)
    };
    if ui.add(egui::SelectableLabel::new(selected, text)).clicked() {
        *page = target;
    }
}

fn status_pill(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.18))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(10, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(color).strong().size(12.0));
        });
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui);
        });
}

fn card_fill(ui: &mut egui::Ui, height: f32, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_size(Vec2::new(ui.available_width(), height));
            ui.set_max_height(height);
            add(ui);
        });
}

fn section_title(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).strong().size(14.0));
}

fn form_row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.set_min_height(24.0);
        ui.add_sized(
            [LABEL_W, 20.0],
            egui::Label::new(RichText::new(label).weak()),
        );
        ui.with_layout(Layout::left_to_right(Align::Center), add);
    });
}

fn path_chip(ui: &mut egui::Ui, text: &str, active: bool) {
    let (fg, bg) = if active {
        (
            egui::Color32::from_rgb(30, 120, 70),
            egui::Color32::from_rgb(220, 245, 230),
        )
    } else {
        (
            egui::Color32::from_rgb(90, 90, 90),
            egui::Color32::from_rgb(235, 235, 235),
        )
    };
    egui::Frame::new()
        .fill(bg)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(fg).size(12.0));
        });
}

fn metric_cell(ui: &mut egui::Ui, width: f32, title: &str, value: &str) {
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(245, 246, 248))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_min_width(width - 4.0);
            ui.set_max_width(width);
            ui.vertical(|ui| {
                ui.label(RichText::new(title).weak().size(12.0));
                ui.add_space(2.0);
                ui.label(RichText::new(value).strong().size(13.0));
            });
        });
}

fn port_setting_card(ui: &mut egui::Ui, width: f32, title: &str, port: &mut u16) {
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(245, 246, 248))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_width(width);
            ui.set_max_width(width);

            ui.label(RichText::new(title).strong());
            ui.add_space(8.0);
            form_row(ui, "端口", |ui| {
                ui.add(egui::DragValue::new(port).range(1..=65535).speed(1.0));
            });
            ui.add_space(4.0);
            form_row(ui, "地址", |ui| {
                ui.monospace(format!("127.0.0.1:{port}"));
            });
        });
}
