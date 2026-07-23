use serde::{Deserialize, Serialize};

fn default_port() -> u16 {
    22
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AuthMethod {
    Password {
        password: String,
    },
    Key {
        key_path: String,
        #[serde(default)]
        passphrase: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardRule {
    pub id: String,
    /// "local" | "remote" | "dynamic" (SOCKS5)
    pub kind: String,
    pub name: String,
    /// local 转发: 本地监听地址; remote 转发: 远端监听地址
    pub bind_host: String,
    pub bind_port: u16,
    pub target_host: String,
    pub target_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub group: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
    #[serde(default)]
    pub forwards: Vec<ForwardRule>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub host_id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub mtime: i64,
    pub permissions: u32,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ServerStats {
    pub hostname: String,
    pub os: String,
    pub cpu_percent: f64,
    pub cpu_cores: u32,
    pub mem_total: u64,
    pub mem_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub disk_total: u64,
    pub disk_used: u64,
    pub uptime_secs: u64,
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
    pub net_rx_bps: f64,
    pub net_tx_bps: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ForwardStatus {
    pub key: String,
    pub session_id: String,
    pub rule_id: String,
    pub active: bool,
    pub detail: String,
}
