use std::{fs, path::PathBuf};

use parking_lot::Mutex;

use crate::models::HostConfig;

/// 主机配置持久化: ~/.config/oneshell/hosts.json
pub struct Store {
    path: PathBuf,
    hosts: Mutex<Vec<HostConfig>>,
}

impl Store {
    pub fn load() -> Result<Self, String> {
        let dir = dirs::config_dir()
            .ok_or("无法定位配置目录")?
            .join("oneshell");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("hosts.json");
        let hosts = if path.exists() {
            let raw = fs::read_to_string(&path).map_err(|e| e.to_string())?;
            if raw.trim().is_empty() {
                Vec::new()
            } else {
                serde_json::from_str(&raw).map_err(|e| format!("hosts.json 解析失败: {e}"))?
            }
        } else {
            Vec::new()
        };
        Ok(Self {
            path,
            hosts: Mutex::new(hosts),
        })
    }

    fn persist(&self, hosts: &[HostConfig]) -> Result<(), String> {
        let raw = serde_json::to_string_pretty(hosts).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, raw).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }

    pub fn list(&self) -> Vec<HostConfig> {
        self.hosts.lock().clone()
    }

    pub fn get(&self, id: &str) -> Result<HostConfig, String> {
        self.hosts
            .lock()
            .iter()
            .find(|h| h.id == id)
            .cloned()
            .ok_or_else(|| format!("主机不存在: {id}"))
    }

    pub fn save(&self, host: HostConfig) -> Result<(), String> {
        let mut hosts = self.hosts.lock();
        match hosts.iter_mut().find(|h| h.id == host.id) {
            Some(existing) => *existing = host,
            None => hosts.push(host),
        }
        self.persist(&hosts)
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        let mut hosts = self.hosts.lock();
        hosts.retain(|h| h.id != id);
        self.persist(&hosts)
    }
}
