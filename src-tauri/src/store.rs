//! 主机配置持久化: ~/.config/oneshell/hosts.json
//!
//! 凭据安全模型 (P0):
//! - 密码/私钥口令只存 OS 钥匙串 (secret::SecretStore 抽象, 生产=keyring);
//!   hosts.json 里永远是脱敏后的空串, 绝不明文落盘。
//! - 旧版本明文文件: 启动时事务式迁移 — 先全部写入钥匙串并读回校验,
//!   成功后才原子重写 JSON; 任何一步失败都回滚已写的钥匙串条目,
//!   原文件一个字节不动, 应用进入"待迁移"状态 (前端横幅 + 可重试),
//!   而不是启动失败锁死用户。
//! - 迁移未完成时, 携带明文凭据的主机禁止连接 (resolve_auth 显式报错)。

use std::{fs, path::PathBuf, sync::Arc};

use parking_lot::Mutex;

use crate::{
    models::{AuthMethod, HostConfig},
    secret::{KeyringSecrets, SecretStore, KIND_PASSPHRASE, KIND_PASSWORD},
};

pub struct Store {
    path: PathBuf,
    hosts: Mutex<Vec<HostConfig>>,
    secrets: Arc<dyn SecretStore>,
    migration_pending: Mutex<bool>,
}

/// 从 HostConfig 里提取明文凭据 (host_id, kind, secret)
fn legacy_secrets(hosts: &[HostConfig]) -> Vec<(String, &'static str, String)> {
    let mut out = Vec::new();
    for h in hosts {
        match &h.auth {
            AuthMethod::Password { password } if !password.is_empty() => {
                out.push((h.id.clone(), KIND_PASSWORD, password.clone()));
            }
            AuthMethod::Key {
                passphrase: Some(p),
                ..
            } if !p.is_empty() => {
                out.push((h.id.clone(), KIND_PASSPHRASE, p.clone()));
            }
            _ => {}
        }
    }
    out
}

/// 脱敏: 清空 HostConfig 里的明文凭据字段
fn strip_secrets(host: &mut HostConfig) {
    match &mut host.auth {
        AuthMethod::Password { password } => password.clear(),
        AuthMethod::Key { passphrase, .. } => *passphrase = None,
    }
}

impl Store {
    pub fn load() -> Result<Self, String> {
        let dir = dirs::config_dir()
            .ok_or("无法定位配置目录")?
            .join("oneshell");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        Self::load_from(dir.join("hosts.json"), Arc::new(KeyringSecrets))
    }

    /// 从指定路径与凭据后端加载 (测试可注入内存后端)。
    /// 加载后自动尝试旧版明文迁移; 迁移失败不致命, 进入待迁移状态。
    fn load_from(path: PathBuf, secrets: Arc<dyn SecretStore>) -> Result<Self, String> {
        let hosts: Vec<HostConfig> = if path.exists() {
            let raw = fs::read_to_string(&path).map_err(|e| e.to_string())?;
            if raw.trim().is_empty() {
                Vec::new()
            } else {
                serde_json::from_str(&raw).map_err(|e| format!("hosts.json 解析失败: {e}"))?
            }
        } else {
            Vec::new()
        };
        let store = Self {
            path,
            hosts: Mutex::new(hosts),
            secrets,
            migration_pending: Mutex::new(false),
        };
        if let Err(e) = store.migrate_legacy() {
            log::warn!("凭据迁移未完成: {e}");
            *store.migration_pending.lock() = true;
        }
        Ok(store)
    }

    pub fn migration_pending(&self) -> bool {
        *self.migration_pending.lock()
    }

    /// 事务式迁移旧版明文凭据:
    /// 1. 逐条写入钥匙串并读回校验 (不一致视为失败);
    /// 2. 全部成功 → 脱敏 + 原子重写 JSON;
    /// 3. 任何失败 → 逆序回滚已写条目 (恢复原值或删除), JSON 不动。
    pub fn migrate_legacy(&self) -> Result<(), String> {
        let secrets = legacy_secrets(&self.hosts.lock());
        if secrets.is_empty() {
            *self.migration_pending.lock() = false;
            return Ok(());
        }

        // 回滚日志: (host_id, kind, 写入前的旧值)
        let mut journal: Vec<(String, &'static str, Option<String>)> = Vec::new();
        let result = (|| -> Result<(), String> {
            for (host_id, kind, secret) in &secrets {
                let prior = self.secrets.get(host_id, kind)?;
                self.secrets.set(host_id, kind, secret)?;
                journal.push((host_id.clone(), kind, prior));
                // 读回校验: 写进去的和读出来的必须一致
                let readback = self.secrets.get(host_id, kind)?;
                if readback.as_deref() != Some(secret.as_str()) {
                    return Err(format!("钥匙串读回校验失败 ({kind})"));
                }
            }
            Ok(())
        })();

        if let Err(e) = result {
            for (host_id, kind, prior) in journal.iter().rev() {
                let rollback = match prior {
                    Some(old) => self.secrets.set(host_id, kind, old),
                    None => self.secrets.delete(host_id, kind),
                };
                if let Err(re) = rollback {
                    log::error!("钥匙串回滚失败 ({host_id}:{kind}): {re}");
                }
            }
            return Err(e);
        }

        // 全部入钥匙串 → 在克隆上脱敏, persist 成功后才替换内存。
        // 否则 persist 失败会让内存永久处于已脱敏状态: 重试迁移看不到明文
        // 误判成功, 而磁盘仍是旧明文、钥匙串已回滚 — 凭据两头落空。
        let mut scrubbed = self.hosts.lock().clone();
        for h in scrubbed.iter_mut() {
            strip_secrets(h);
        }
        if let Err(e) = self.persist(&scrubbed) {
            // 持久化失败同样回滚钥匙串, 保持"文件不动"的事务语义
            for (host_id, kind, prior) in journal.iter().rev() {
                let _ = match prior {
                    Some(old) => self.secrets.set(host_id, kind, old),
                    None => self.secrets.delete(host_id, kind),
                };
            }
            return Err(format!("重写 hosts.json 失败: {e}"));
        }
        *self.hosts.lock() = scrubbed;
        *self.migration_pending.lock() = false;
        Ok(())
    }

    fn persist(&self, hosts: &[HostConfig]) -> Result<(), String> {
        let raw = serde_json::to_string_pretty(hosts).map_err(|e| e.to_string())?;
        // 符号链接/异常归属的 hosts.json 拒绝替换
        #[cfg(unix)]
        if let Ok(meta) = fs::symlink_metadata(&self.path) {
            use std::os::unix::fs::MetadataExt;
            if meta.file_type().is_symlink() {
                return Err("hosts.json 是符号链接, 拒绝写入".into());
            }
            if meta.uid() != unsafe { libc::geteuid() } {
                return Err(format!(
                    "hosts.json 归属异常 (uid {}), 拒绝写入",
                    meta.uid()
                ));
            }
        }
        // 权限钳制 0600: 不沿用明文时代可能过宽的旧权限
        let tmp = self
            .path
            .with_file_name(format!(".hosts.{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> Result<(), String> {
            {
                use std::io::Write;
                let mut opts = fs::OpenOptions::new();
                opts.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    opts.mode(0o600);
                }
                let mut f = opts.open(&tmp).map_err(|e| e.to_string())?;
                f.write_all(raw.as_bytes())
                    .and_then(|()| f.sync_all())
                    .map_err(|e| e.to_string())?;
            }
            fs::rename(&tmp, &self.path).map_err(|e| e.to_string())?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp); // 任何失败路径都清理临时文件
        }
        result?;
        #[cfg(unix)]
        if let Some(parent) = self.path.parent() {
            if let Ok(dir) = fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
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

    /// 连接时取凭据: 钥匙串是唯一来源, 不做明文回退。
    /// 内存里仍有明文 = 迁移未完成 → 拒绝连接并引导用户重试迁移。
    pub fn resolve_auth(&self, id: &str) -> Result<AuthMethod, String> {
        let host = self.get(id)?;
        match host.auth {
            AuthMethod::Password { password } if !password.is_empty() => {
                Err("凭据尚未迁入系统钥匙串, 请先在应用中完成迁移再连接".into())
            }
            AuthMethod::Password { .. } => match self.secrets.get(id, KIND_PASSWORD)? {
                Some(password) => Ok(AuthMethod::Password { password }),
                None => Err("钥匙串中找不到该主机的密码, 请编辑主机重新保存".into()),
            },
            AuthMethod::Key {
                passphrase: Some(p),
                ..
            } if !p.is_empty() => Err("凭据尚未迁入系统钥匙串, 请先在应用中完成迁移再连接".into()),
            AuthMethod::Key { key_path, .. } => {
                let passphrase = self.secrets.get(id, KIND_PASSPHRASE)?;
                Ok(AuthMethod::Key {
                    key_path,
                    passphrase,
                })
            }
        }
    }

    /// 保存主机: 非空凭据写入钥匙串 (写失败直接报错, 不落盘);
    /// 空凭据 = 保持不变 (编辑场景)。持久化失败回滚钥匙串写入。
    pub fn save(&self, mut host: HostConfig) -> Result<(), String> {
        // 回滚日志: (host_id, kind, 写入前的旧值)
        let mut journal: Vec<(String, &'static str, Option<String>)> = Vec::new();
        // host 会被移入列表, 提前取出后续要用的值
        let host_id = host.id.clone();
        let drop_kind = match &host.auth {
            AuthMethod::Password { .. } => KIND_PASSPHRASE,
            AuthMethod::Key { .. } => KIND_PASSWORD,
        };

        let result = (|| -> Result<(), String> {
            let pending: Vec<(&'static str, &str)> = match &host.auth {
                AuthMethod::Password { password } if !password.is_empty() => {
                    vec![(KIND_PASSWORD, password.as_str())]
                }
                AuthMethod::Key {
                    passphrase: Some(p),
                    ..
                } if !p.is_empty() => vec![(KIND_PASSPHRASE, p.as_str())],
                _ => Vec::new(),
            };
            for (kind, value) in pending {
                let prior = self.secrets.get(&host.id, kind)?;
                self.secrets.set(&host.id, kind, value)?;
                journal.push((host.id.clone(), kind, prior));
                // 读回校验: 后端报成功但存错值时, 在脱敏落盘前拦下
                let readback = self.secrets.get(&host.id, kind)?;
                if readback.as_deref() != Some(value) {
                    return Err(format!("钥匙串读回校验失败 ({kind}), 已取消保存"));
                }
            }
            strip_secrets(&mut host);
            // 先快照内存态, 持久化失败时精确恢复 (不读文件,
            // 瞬时 IO/解析错误不会清空内存主机列表)
            let snapshot;
            {
                let mut hosts = self.hosts.lock();
                snapshot = hosts.clone();
                match hosts.iter_mut().find(|h| h.id == host.id) {
                    Some(existing) => *existing = host,
                    None => hosts.push(host),
                }
                match self.persist(&hosts) {
                    Ok(()) => {}
                    Err(e) => {
                        *hosts = snapshot;
                        return Err(e);
                    }
                }
            }
            Ok(())
        })();

        if let Err(e) = result {
            for (host_id, kind, prior) in journal.iter().rev() {
                let rollback = match prior {
                    Some(old) => self.secrets.set(host_id, kind, old),
                    None => self.secrets.delete(host_id, kind),
                };
                if let Err(re) = rollback {
                    log::error!("钥匙串回滚失败 ({host_id}:{kind}): {re}");
                }
            }
            return Err(e);
        }

        // 认证方式切换后清理另一类凭据 (best-effort)
        if let Err(e) = self.secrets.delete(&host_id, drop_kind) {
            log::warn!("清理旧类型凭据失败: {e}");
        }
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        let mut hosts = self.hosts.lock();
        hosts.retain(|h| h.id != id);
        self.persist(&hosts)?;
        // 清理钥匙串条目 (best-effort)
        if let Err(e) = self.secrets.delete(id, KIND_PASSWORD) {
            log::warn!("清理钥匙串密码失败: {e}");
        }
        if let Err(e) = self.secrets.delete(id, KIND_PASSPHRASE) {
            log::warn!("清理钥匙串口令失败: {e}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::MemorySecrets;

    fn pw_host(id: &str, password: &str) -> HostConfig {
        HostConfig {
            id: id.into(),
            name: format!("主机{id}"),
            group: "生产".into(),
            host: format!("{id}.example.com"),
            port: 22,
            username: "root".into(),
            auth: AuthMethod::Password {
                password: password.into(),
            },
            forwards: Vec::new(),
        }
    }

    fn key_host(id: &str, passphrase: Option<&str>) -> HostConfig {
        HostConfig {
            id: id.into(),
            name: String::new(),
            group: String::new(),
            host: format!("{id}.example.com"),
            port: 2222,
            username: "deploy".into(),
            auth: AuthMethod::Key {
                key_path: "~/.ssh/id_ed25519".into(),
                passphrase: passphrase.map(Into::into),
            },
            forwards: Vec::new(),
        }
    }

    fn setup(hosts_json: Option<&str>) -> (tempfile::TempDir, PathBuf, Arc<MemorySecrets>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.json");
        if let Some(raw) = hosts_json {
            fs::write(&path, raw).unwrap();
        }
        (dir, path, MemorySecrets::new())
    }

    /// 真实格式夹具: 字段/顺序/分组/转发规则与现网 hosts.json 一致
    const LEGACY_JSON: &str = r##"[
  {
    "id": "a1",
    "name": "跳板机",
    "group": "生产",
    "host": "jump.example.com",
    "port": 22,
    "username": "ops",
    "auth": { "kind": "password", "password": "s3cret-a" },
    "forwards": [
      {
        "id": "f1",
        "kind": "local",
        "name": "db",
        "bind_host": "127.0.0.1",
        "bind_port": 15432,
        "target_host": "db.internal",
        "target_port": 5432
      }
    ]
  },
  {
    "id": "b2",
    "name": "应用服",
    "group": "生产",
    "host": "app.example.com",
    "port": 2222,
    "username": "deploy",
    "auth": { "kind": "key", "key_path": "~/.ssh/id_ed25519", "passphrase": "pp-b" },
    "forwards": []
  }
]"##;

    #[test]
    fn migration_success_scrubs_file_preserving_shape_and_order() {
        let (_dir, path, secrets) = setup(Some(LEGACY_JSON));
        let store = Store::load_from(path.clone(), secrets.clone()).unwrap();

        assert!(!store.migration_pending());
        assert_eq!(
            secrets.get("a1", KIND_PASSWORD).unwrap().as_deref(),
            Some("s3cret-a")
        );
        assert_eq!(
            secrets.get("b2", KIND_PASSPHRASE).unwrap().as_deref(),
            Some("pp-b")
        );

        // 文件: 无明文, 但字段/顺序/转发规则原样保留
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("s3cret-a"), "文件不得残留明文密码");
        assert!(!raw.contains("pp-b"), "文件不得残留明文口令");
        let hosts: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0]["id"], "a1"); // 顺序不变
        assert_eq!(hosts[1]["id"], "b2");
        assert_eq!(hosts[0]["forwards"][0]["bind_port"], 15432); // 嵌套字段不丢
        assert_eq!(hosts[0]["auth"]["kind"], "password");
        assert_eq!(hosts[1]["auth"]["key_path"], "~/.ssh/id_ed25519");

        match store.resolve_auth("a1").unwrap() {
            AuthMethod::Password { password } => assert_eq!(password, "s3cret-a"),
            _ => panic!(),
        }
        match store.resolve_auth("b2").unwrap() {
            AuthMethod::Key { passphrase, .. } => {
                assert_eq!(passphrase.as_deref(), Some("pp-b"))
            }
            _ => panic!(),
        }
    }

    #[test]
    fn migration_failure_leaves_file_untouched_and_retryable() {
        let (_dir, path, secrets) = setup(Some(LEGACY_JSON));
        let before = fs::read_to_string(&path).unwrap();
        *secrets.fail.lock() = Some("no session bus".into());

        let store = Store::load_from(path.clone(), secrets.clone()).unwrap();
        assert!(store.migration_pending(), "失败应进入待迁移状态");
        assert_eq!(fs::read_to_string(&path).unwrap(), before, "文件原封不动");

        // 待迁移时禁止连接携带凭据的主机
        assert!(store.resolve_auth("a1").is_err());

        // 修复后端后重试迁移 → 成功且幂等
        *secrets.fail.lock() = None;
        store.migrate_legacy().unwrap();
        assert!(!store.migration_pending());
        assert!(!fs::read_to_string(&path).unwrap().contains("s3cret-a"));
        store.migrate_legacy().unwrap(); // 第二次: 无明文, 空转成功
        assert!(store.resolve_auth("a1").is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn migration_persist_failure_keeps_memory_retryable() {
        use std::os::unix::fs::PermissionsExt;

        let (_dir, path, secrets) = setup(Some(LEGACY_JSON));
        let before = fs::read_to_string(&path).unwrap();
        // 目录只读 → persist 创建临时文件失败
        let dir_path = path.parent().unwrap().to_path_buf();
        fs::set_permissions(&dir_path, fs::Permissions::from_mode(0o500)).unwrap();

        let store = Store::load_from(path.clone(), secrets.clone()).unwrap();
        assert!(store.migration_pending(), "persist 失败应进入待迁移");
        assert_eq!(fs::read_to_string(&path).unwrap(), before, "文件原样");
        assert!(
            store.resolve_auth("a1").is_err(),
            "内存仍含待迁移明文, 连接应被拦截而不是误判已迁移"
        );
        assert_eq!(
            secrets.get("a1", KIND_PASSWORD).unwrap(),
            None,
            "钥匙串已回滚"
        );

        // 恢复可写后重试: 必须能真正完成迁移 (内存明文还在)
        fs::set_permissions(&dir_path, fs::Permissions::from_mode(0o700)).unwrap();
        store.migrate_legacy().unwrap();
        assert!(!store.migration_pending());
        assert!(!fs::read_to_string(&path).unwrap().contains("s3cret-a"));
        assert_eq!(
            secrets.get("a1", KIND_PASSWORD).unwrap().as_deref(),
            Some("s3cret-a")
        );
        assert!(store.resolve_auth("a1").is_ok());
    }

    #[test]
    fn migration_rolls_back_partial_keychain_writes() {
        let (_dir, path, secrets) = setup(Some(LEGACY_JSON));
        let before = fs::read_to_string(&path).unwrap();
        // 第 1 次 set 成功, 第 2 次失败 → a1 已写入的条目必须回滚删除
        *secrets.fail_set_at.lock() = Some((1, "dbus vanished".into()));

        let store = Store::load_from(path.clone(), secrets.clone()).unwrap();
        assert!(store.migration_pending());
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        assert_eq!(
            secrets.get("a1", KIND_PASSWORD).unwrap(),
            None,
            "部分写入必须回滚"
        );
    }

    #[test]
    fn migration_detects_wrong_readback_and_rolls_back() {
        let (_dir, path, secrets) = setup(Some(LEGACY_JSON));
        let before = fs::read_to_string(&path).unwrap();
        secrets.set_corrupt(true); // 读回错值

        let store = Store::load_from(path.clone(), secrets.clone()).unwrap();
        assert!(store.migration_pending());
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        secrets.set_corrupt(false);
        assert_eq!(
            secrets.get("a1", KIND_PASSWORD).unwrap(),
            None,
            "回滚应精确恢复到写入前 (无条目)"
        );
    }

    #[test]
    fn save_corrupt_readback_aborts_before_scrubbing() {
        // 后端报成功但读回错值: 必须在脱敏落盘前拦下,
        // 文件/内存不变, 回滚后钥匙串精确恢复旧值
        let (_dir, path, secrets) = setup(None);
        let store = Store::load_from(path.clone(), secrets.clone()).unwrap();
        secrets.set("c1", KIND_PASSWORD, "old-pw").unwrap(); // 预置旧凭据
        secrets.set_corrupt(true);

        assert!(store.save(pw_host("c1", "pw-c")).is_err());
        assert!(store.list().is_empty(), "内存不得变更");
        assert!(!path.exists(), "文件不得创建");

        secrets.set_corrupt(false);
        assert_eq!(
            secrets.get("c1", KIND_PASSWORD).unwrap().as_deref(),
            Some("old-pw"),
            "回滚应精确恢复旧值"
        );
    }

    #[test]
    fn save_writes_keychain_and_strips_file() {
        let (_dir, path, secrets) = setup(None);
        let store = Store::load_from(path.clone(), secrets.clone()).unwrap();

        store.save(pw_host("x1", "pw-x")).unwrap();
        assert_eq!(
            secrets.get("x1", KIND_PASSWORD).unwrap().as_deref(),
            Some("pw-x")
        );
        assert!(!fs::read_to_string(&path).unwrap().contains("pw-x"));

        // 编辑时密码留空 = 不变
        let mut h = pw_host("x1", "");
        h.name = "改名".into();
        store.save(h).unwrap();
        assert_eq!(
            secrets.get("x1", KIND_PASSWORD).unwrap().as_deref(),
            Some("pw-x"),
            "留空不应清空钥匙串"
        );
    }

    #[test]
    fn save_keychain_failure_happens_before_mutation() {
        // 阶段一: 钥匙串写在主机列表变更之前, 失败时内存/文件都不动
        let (_dir, path, secrets) = setup(None);
        let store = Store::load_from(path, secrets.clone()).unwrap();
        *secrets.fail.lock() = Some("keychain locked".into());

        assert!(store.save(pw_host("k1", "pw-k")).is_err());
        assert!(store.list().is_empty(), "内存不得变更");
        assert!(!store.path.exists(), "文件不得创建");
    }

    #[test]
    fn save_rolls_back_keychain_when_persist_fails() {
        // 阶段二: 钥匙串已写, 持久化失败 → 钥匙串回滚 + 内存快照恢复
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no-such-dir").join("hosts.json");
        let secrets = MemorySecrets::new();
        let store = Store::load_from(path, secrets.clone()).unwrap();

        assert!(store.save(pw_host("y1", "pw-y")).is_err());
        assert_eq!(
            secrets.get("y1", KIND_PASSWORD).unwrap(),
            None,
            "持久化失败应回滚钥匙串"
        );
        assert!(store.list().is_empty(), "内存不得残留未持久化的主机");
        // 临时文件已清理
        assert!(!dir.path().join("no-such-dir").exists());
    }

    #[test]
    fn save_auth_switch_clears_stale_opposite_secret() {
        let (_dir, path, secrets) = setup(None);
        let store = Store::load_from(path, secrets.clone()).unwrap();
        store.save(key_host("z1", Some("pp-z"))).unwrap();
        assert!(secrets.get("z1", KIND_PASSPHRASE).unwrap().is_some());

        // 改成密码认证 → 旧口令条目被清理
        store.save(pw_host("z1", "pw-z")).unwrap();
        assert_eq!(secrets.get("z1", KIND_PASSPHRASE).unwrap(), None);
        assert_eq!(
            secrets.get("z1", KIND_PASSWORD).unwrap().as_deref(),
            Some("pw-z")
        );
    }

    #[test]
    fn resolve_auth_missing_keychain_entry_is_actionable_error() {
        let (_dir, path, secrets) = setup(None);
        let store = Store::load_from(path, secrets).unwrap();
        store.save(pw_host("m1", "pw-m")).unwrap();
        store.secrets.delete("m1", KIND_PASSWORD).unwrap();
        let err = store.resolve_auth("m1").unwrap_err();
        assert!(err.contains("钥匙串"), "错误应引导用户: {err}");
    }

    #[test]
    fn delete_removes_keychain_entries() {
        let (_dir, path, secrets) = setup(None);
        let store = Store::load_from(path, secrets.clone()).unwrap();
        store.save(pw_host("d1", "pw-d")).unwrap();
        store.delete("d1").unwrap();
        assert_eq!(secrets.get("d1", KIND_PASSWORD).unwrap(), None);
        assert!(store.list().is_empty());
    }
}
