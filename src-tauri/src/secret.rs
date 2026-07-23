//! 凭据存储抽象: 生产环境走 OS 钥匙串 (macOS Keychain / Windows Credential
//! Manager / Linux Secret Service), 测试用内存实现。
//!
//! 设计约束: 任何失败都显式报错, 绝不静默降级为明文落盘。

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use parking_lot::Mutex;

/// 凭据种类: 主机密码 / 私钥口令
pub const KIND_PASSWORD: &str = "password";
pub const KIND_PASSPHRASE: &str = "passphrase";

const SERVICE: &str = "oneshell";

/// 凭据后端。所有方法都是同步阻塞的 (keychain API 本身同步);
/// 在 async 上下文调用时需自行包 spawn_blocking。
pub trait SecretStore: Send + Sync {
    fn set(&self, host_id: &str, kind: &str, secret: &str) -> Result<(), String>;
    /// Ok(None) 表示条目不存在
    fn get(&self, host_id: &str, kind: &str) -> Result<Option<String>, String>;
    /// 删除不存在的条目视为成功
    fn delete(&self, host_id: &str, kind: &str) -> Result<(), String>;
}

// ── 生产实现: keyring crate ─────────────────────────────────────────────

pub struct KeyringSecrets;

impl KeyringSecrets {
    fn entry(host_id: &str, kind: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, &format!("{host_id}:{kind}"))
            .map_err(|e| format!("钥匙串不可用: {e}"))
    }
}

impl SecretStore for KeyringSecrets {
    fn set(&self, host_id: &str, kind: &str, secret: &str) -> Result<(), String> {
        Self::entry(host_id, kind)?
            .set_password(secret)
            .map_err(|e| format!("写入钥匙串失败 ({kind}): {e}"))
    }

    fn get(&self, host_id: &str, kind: &str) -> Result<Option<String>, String> {
        match Self::entry(host_id, kind)?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(format!("读取钥匙串失败 ({kind}): {e}")),
        }
    }

    fn delete(&self, host_id: &str, kind: &str) -> Result<(), String> {
        match Self::entry(host_id, kind)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("删除钥匙串条目失败 ({kind}): {e}")),
        }
    }
}

// ── 测试实现: 内存 map, 可注入故障 ──────────────────────────────────────

#[cfg(test)]
#[derive(Default)]
pub struct MemorySecrets {
    map: Mutex<HashMap<(String, String), String>>,
    /// 置位后所有 set/get 立即失败, 模拟 Linux 无 session bus / 钥匙串未解锁
    pub fail: Mutex<Option<String>>,
    /// 置位后 set 的条目被"污染": 后续 get 对该条目返回错值 (模拟后端报
    /// 成功但存错)。未污染条目的读取不受影响, 事务的旧值快照保持准确。
    /// 通过 set_corrupt 关闭时清除污染 (模拟后端恢复正常)。
    corrupt: Mutex<bool>,
    tainted: Mutex<std::collections::HashSet<(String, String)>>,
    /// 置位后第 n 次 set 调用失败 (从 0 计), 注入部分写失败
    pub fail_set_at: Mutex<Option<(usize, String)>>,
}

#[cfg(test)]
impl MemorySecrets {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn check(&self) -> Result<(), String> {
        match &*self.fail.lock() {
            Some(msg) => Err(msg.clone()),
            None => Ok(()),
        }
    }

    /// 开关污染模式; 关闭时清除全部污染 (后端恢复正常)
    pub fn set_corrupt(&self, on: bool) {
        *self.corrupt.lock() = on;
        if !on {
            self.tainted.lock().clear();
        }
    }
}

#[cfg(test)]
impl SecretStore for MemorySecrets {
    fn set(&self, host_id: &str, kind: &str, secret: &str) -> Result<(), String> {
        self.check()?;
        if let Some((n, msg)) = &mut *self.fail_set_at.lock() {
            if *n == 0 {
                return Err(msg.clone());
            }
            *n -= 1;
        }
        // 健康写入先解除污染; 污染模式下再重新标记
        self.tainted.lock().remove(&(host_id.into(), kind.into()));
        if *self.corrupt.lock() {
            self.tainted.lock().insert((host_id.into(), kind.into()));
        }
        self.map
            .lock()
            .insert((host_id.into(), kind.into()), secret.into());
        Ok(())
    }

    fn get(&self, host_id: &str, kind: &str) -> Result<Option<String>, String> {
        self.check()?;
        let key = (host_id.into(), kind.into());
        if self.tainted.lock().contains(&key) {
            return Ok(Some("corrupted-readback".into()));
        }
        Ok(self.map.lock().get(&key).cloned())
    }

    fn delete(&self, host_id: &str, kind: &str) -> Result<(), String> {
        self.check()?;
        self.tainted.lock().remove(&(host_id.into(), kind.into()));
        self.map.lock().remove(&(host_id.into(), kind.into()));
        Ok(())
    }
}
