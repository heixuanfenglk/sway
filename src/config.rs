use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMethod {
    Password,
    PrivateKey,
}

impl Default for AuthMethod {
    fn default() -> Self {
        Self::Password
    }
}

/// 单个 SSH 主机档案
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostProfile {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: AuthMethod,
    #[serde(skip)]
    pub password: String,
    pub private_key_path: String,
}

impl Default for HostProfile {
    fn default() -> Self {
        Self {
            id: new_id(),
            name: "新主机".into(),
            host: String::new(),
            port: 22,
            username: String::new(),
            auth_method: AuthMethod::Password,
            password: String::new(),
            private_key_path: String::new(),
        }
    }
}

impl HostProfile {
    pub fn list_title(&self) -> String {
        let name = self.name.trim();
        if !name.is_empty() {
            name.to_string()
        } else {
            "未命名主机".into()
        }
    }

    pub fn display_label(&self) -> String {
        let name = self.name.trim();
        let host = self.host.trim();
        if !name.is_empty() && name != "新主机" {
            if host.is_empty() {
                name.to_string()
            } else {
                format!("{name} ({host})")
            }
        } else if !host.is_empty() {
            let user = self.username.trim();
            if user.is_empty() {
                host.to_string()
            } else {
                format!("{user}@{host}")
            }
        } else {
            "未命名主机".into()
        }
    }

    pub fn validate_ssh(&self) -> Result<()> {
        if self.host.trim().is_empty() {
            anyhow::bail!("请填写 SSH 主机地址");
        }
        if self.username.trim().is_empty() {
            anyhow::bail!("请填写用户名");
        }
        if self.port == 0 {
            anyhow::bail!("SSH 端口无效");
        }
        match self.auth_method {
            AuthMethod::Password => {
                if self.password.is_empty() {
                    anyhow::bail!("请填写密码（密码仅保存在内存，切换后需重填）");
                }
            }
            AuthMethod::PrivateKey => {
                if self.private_key_path.trim().is_empty() {
                    anyhow::bail!("请选择私钥文件");
                }
                if !PathBuf::from(&self.private_key_path).exists() {
                    anyhow::bail!("私钥文件不存在");
                }
            }
        }
        Ok(())
    }
}

/// 全局代理相关设置（所有主机共用本地端口）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub socks_port: u16,
    pub http_port: u16,
    pub auto_set_system_proxy: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            socks_port: 1080,
            http_port: 7890,
            auto_set_system_proxy: true,
        }
    }
}

impl AppSettings {
    pub fn validate(&self) -> Result<()> {
        if self.socks_port == 0 || self.http_port == 0 {
            anyhow::bail!("本地代理端口无效");
        }
        if self.socks_port == self.http_port {
            anyhow::bail!("SOCKS 与 HTTP 端口不能相同");
        }
        Ok(())
    }
}

/// 传给隧道的完整连接参数
#[derive(Debug, Clone)]
pub struct TunnelRequest {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: AuthMethod,
    pub password: String,
    pub private_key_path: String,
    pub socks_port: u16,
    pub http_port: u16,
}

impl TunnelRequest {
    pub fn from_profile(profile: &HostProfile, settings: &AppSettings) -> Result<Self> {
        profile.validate_ssh()?;
        settings.validate()?;
        Ok(Self {
            host: profile.host.clone(),
            port: profile.port,
            username: profile.username.clone(),
            auth_method: profile.auth_method,
            password: profile.password.clone(),
            private_key_path: profile.private_key_path.clone(),
            socks_port: settings.socks_port,
            http_port: settings.http_port,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub profiles: Vec<HostProfile>,
    #[serde(default)]
    pub active_profile_id: String,
    #[serde(default)]
    pub settings: AppSettings,
}

impl Default for AppConfig {
    fn default() -> Self {
        let profile = HostProfile::default();
        let id = profile.id.clone();
        Self {
            profiles: vec![profile],
            active_profile_id: id,
            settings: AppSettings::default(),
        }
    }
}

impl AppConfig {
    pub fn config_path() -> Result<PathBuf> {
        let dir = dirs::config_dir()
            .context("无法定位配置目录")?
            .join("sway");
        fs::create_dir_all(&dir).context("创建配置目录失败")?;
        Ok(dir.join("config.toml"))
    }

    pub fn load() -> Self {
        let Ok(path) = Self::config_path() else {
            return Self::default();
        };
        // 优先读新目录；若无则尝试迁移旧版 sslink / sshtools 配置
        let content = fs::read_to_string(&path).or_else(|_| {
            let config_root = dirs::config_dir().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "no config dir")
            })?;
            let legacy_dirs = ["sslink", "sshtools"];
            let mut last_err = std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no legacy config",
            );
            for name in legacy_dirs {
                let legacy = config_root.join(name).join("config.toml");
                match fs::read_to_string(&legacy) {
                    Ok(c) => {
                        if let Some(parent) = path.parent() {
                            let _ = fs::create_dir_all(parent);
                        }
                        let _ = fs::write(&path, &c);
                        return Ok(c);
                    }
                    Err(e) => last_err = e,
                }
            }
            Err(last_err)
        });
        let Ok(content) = content else {
            return Self::default();
        };

        if let Ok(cfg) = toml::from_str::<AppConfig>(&content) {
            return cfg.normalized();
        }

        // 兼容旧版单主机配置
        if let Ok(legacy) = toml::from_str::<LegacyConfig>(&content) {
            return legacy.into_app_config().normalized();
        }

        Self::default()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        let mut to_save = self.clone();
        for p in &mut to_save.profiles {
            p.password.clear();
        }
        let content = toml::to_string_pretty(&to_save).context("序列化配置失败")?;
        fs::write(&path, content).context("写入配置失败")?;
        Ok(())
    }

    fn normalized(mut self) -> Self {
        if self.profiles.is_empty() {
            self.profiles.push(HostProfile::default());
        }
        if self
            .profiles
            .iter()
            .all(|p| p.id != self.active_profile_id)
        {
            self.active_profile_id = self.profiles[0].id.clone();
        }
        for p in &mut self.profiles {
            if p.id.trim().is_empty() {
                p.id = new_id();
            }
            if p.port == 0 {
                p.port = 22;
            }
            if p.name.trim().is_empty() {
                p.name = p.display_label();
            }
        }
        self
    }

    pub fn active_index(&self) -> usize {
        self.profiles
            .iter()
            .position(|p| p.id == self.active_profile_id)
            .unwrap_or(0)
    }

    pub fn active_profile(&self) -> &HostProfile {
        let i = self.active_index();
        &self.profiles[i]
    }

    pub fn active_profile_mut(&mut self) -> &mut HostProfile {
        let i = self.active_index();
        &mut self.profiles[i]
    }

    pub fn set_active(&mut self, id: &str) -> bool {
        if self.profiles.iter().any(|p| p.id == id) {
            self.active_profile_id = id.to_string();
            true
        } else {
            false
        }
    }

    pub fn add_profile(&mut self) -> String {
        let mut p = HostProfile::default();
        let n = self.profiles.len() + 1;
        p.name = format!("主机 {n}");
        let id = p.id.clone();
        self.profiles.push(p);
        self.active_profile_id = id.clone();
        id
    }

    pub fn duplicate_active(&mut self) -> Option<String> {
        let src = self.active_profile().clone();
        let mut p = src;
        p.id = new_id();
        p.name = format!("{} (副本)", p.name);
        p.password.clear();
        let id = p.id.clone();
        self.profiles.push(p);
        self.active_profile_id = id.clone();
        Some(id)
    }

    pub fn remove_active(&mut self) -> Result<()> {
        if self.profiles.len() <= 1 {
            anyhow::bail!("至少保留一个主机");
        }
        let idx = self.active_index();
        self.profiles.remove(idx);
        self.active_profile_id = self.profiles[idx.min(self.profiles.len() - 1)].id.clone();
        Ok(())
    }
}

/// 旧版单主机配置（迁移用）
#[derive(Debug, Deserialize)]
struct LegacyConfig {
    #[serde(default)]
    host: String,
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default)]
    username: String,
    #[serde(default)]
    auth_method: AuthMethod,
    #[serde(default)]
    private_key_path: String,
    #[serde(default = "default_socks")]
    socks_port: u16,
    #[serde(default = "default_http")]
    http_port: u16,
    #[serde(default = "default_true")]
    auto_set_system_proxy: bool,
}

fn default_port() -> u16 {
    22
}
fn default_socks() -> u16 {
    1080
}
fn default_http() -> u16 {
    7890
}
fn default_true() -> bool {
    true
}

impl LegacyConfig {
    fn into_app_config(self) -> AppConfig {
        let mut profile = HostProfile::default();
        profile.name = if self.host.is_empty() {
            "默认主机".into()
        } else {
            self.host.clone()
        };
        profile.host = self.host;
        profile.port = self.port;
        profile.username = self.username;
        profile.auth_method = self.auth_method;
        profile.private_key_path = self.private_key_path;
        let id = profile.id.clone();
        AppConfig {
            profiles: vec![profile],
            active_profile_id: id,
            settings: AppSettings {
                socks_port: self.socks_port,
                http_port: self.http_port,
                auto_set_system_proxy: self.auto_set_system_proxy,
            },
        }
    }
}

fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("host-{nanos}")
}
