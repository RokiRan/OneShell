//! 主机密钥信任: known_hosts 查验 + TOFU(首次使用信任)。
//!
//! 安全模型:
//! - `~/.ssh/known_hosts` 只读复用 (OpenSSH 已信任的主机免确认);
//!   解析用 ssh-key crate 的 KnownHosts 解析器, 不在本仓库维护第二份。
//! - OneShell 自己接受的 key 写入独立的 `~/.config/oneshell/known_hosts`,
//!   追加前读全量再 temp-file + rename 原子替换, 全局锁串行化写。
//! - `check_server_key` 永不阻塞等用户决定: 未知 key 直接拒绝连接,
//!   把实际呈现的 key 以随机 check_id 登记进 pending map, 由前端弹窗确认。
//!   接受时按 check_id 取出并校验 host+port+key 精确一致才持久化 —
//!   并发连接/多窗口无法覆盖用户真正批准的那把 key。
//! - mismatch(密钥变更) 与 revoked(已吊销) 都是硬失败路径, 无"仍然接受"。
//!
//! 为什么这是 P0: 没有主机密钥校验时, 中间人可以伪造整个 SSH 通道,
//! 在伪造的 shell 里投递恶意命令 — 而这些命令的退出码/cwd 会通过 OSC 5151
//! 进入 AI 自动失败分析链路, 把"AI 解释你的错误"变成攻击者的提示词注入面。
//!
//! 匹配语义 (已用 ssh-keygen -F 逐项验证):
//! - 候选字符串: 端口 22 为 `host`, 非标端口为 `[host]:port`;
//!   普通 `host` 条目不匹配非标端口连接, 反之亦然。
//! - 支持 `*`/`?` 通配 (跨点匹配)、`!` 否定、逗号列表、`|1|salt|hash`
//!   散列主机名 (HMAC-SHA1, 对候选字符串)。
//! - `@revoked` 匹配且 key 相同 → 硬拒绝; `@cert-authority` 匹配但无普通
//!   条目 → 显式"未实现证书机构校验"错误, 不允许落入 TOFU 绕过 CA 策略。

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use hmac::Mac;
use parking_lot::Mutex;
use russh::keys::{
    ssh_key::known_hosts::{Entry, HostPatterns, KnownHosts, Marker},
    HashAlg, PublicKey,
};
use serde::Serialize;
use tauri::State;
use uuid::Uuid;

use crate::AppState;

// ── 类型 ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct StoredKeyInfo {
    pub key_type: String,
    pub fingerprint: String,
}

#[derive(Debug)]
pub enum KeyLookup {
    /// 呈现的 key 与某条适用记录完全一致
    Match,
    /// 没有任何适用于该 host:port 的记录
    Unknown,
    /// 有适用的普通记录但都不匹配 (可能被篡改, 也可能是合法轮换)。
    /// 携带全部适用记录的指纹, 不只第一条 — 多算法并存时逐条展示。
    Mismatch { stored: Vec<StoredKeyInfo> },
    /// 命中 @revoked 且 key 相同: OpenSSH 语义下的硬拒绝
    Revoked,
    /// 只命中 @cert-authority: 证书校验未实现, 显式报错而非落入 TOFU
    UnsupportedCertAuthority,
}

/// 一次"未知主机密钥"待决记录; 以随机 check_id 索引
#[derive(Debug, Clone)]
pub struct PendingHostKey {
    pub host_id: String,
    pub host: String,
    pub port: u16,
    pub key_type: String,
    pub fingerprint: String,
    /// OpenSSH 单行格式 ("ssh-ed25519 AAAA…"), 用于写入信任文件
    pub key_openssh: String,
    created: Instant,
}

pub type PendingMap = Arc<Mutex<HashMap<String, PendingHostKey>>>;

const PENDING_TTL: Duration = Duration::from_secs(600);
const PENDING_CAP: usize = 64;

// ── 路径 ────────────────────────────────────────────────────────────────

/// OneShell 自有信任文件 (只写这个)
pub fn trust_file_path() -> Result<PathBuf, String> {
    Ok(dirs::config_dir()
        .ok_or("无法定位配置目录")?
        .join("oneshell")
        .join("known_hosts"))
}

/// 用户 OpenSSH known_hosts (只读)
fn user_known_hosts_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".ssh").join("known_hosts"))
}

// ── 主机名匹配 ──────────────────────────────────────────────────────────

/// 查找候选字符串: OpenSSH 语义, 非标端口用 [host]:port
fn candidate_of(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    }
}

/// glob 匹配: `*` 任意序列 (可跨点), `?` 单字符。
/// 逐字节 ASCII 大小写折叠 (与 OpenSSH 的 tolower 语义一致);
/// 不做 Unicode 小写化/IDNA 转换 — OpenSSH 对 known_hosts 也不做,
/// 非 ASCII 字节按原值比较, 避免 `İ` 等多字节折叠分歧。
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<u8> = pattern.bytes().map(|b| b.to_ascii_lowercase()).collect();
    let t: Vec<u8> = text.bytes().map(|b| b.to_ascii_lowercase()).collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = pi;
            mark = ti;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// OpenSSH 主机模式匹配: 任一非否定模式命中且所有否定模式都不命中
fn host_patterns_match(patterns: &HostPatterns, candidate: &str) -> bool {
    match patterns {
        HostPatterns::HashedName { salt, hash } => {
            let Ok(mut mac) = hmac::Hmac::<sha1::Sha1>::new_from_slice(salt) else {
                return false;
            };
            mac.update(candidate.as_bytes());
            mac.verify_slice(hash).is_ok()
        }
        HostPatterns::Patterns(list) => {
            let mut matched = false;
            for pat in list {
                if let Some(neg) = pat.strip_prefix('!') {
                    if glob_match(neg, candidate) {
                        return false;
                    }
                } else if glob_match(pat, candidate) {
                    matched = true;
                }
            }
            matched
        }
    }
}

// ── 查验 ────────────────────────────────────────────────────────────────

pub fn fingerprint_of(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

fn key_info(key: &PublicKey) -> StoredKeyInfo {
    StoredKeyInfo {
        key_type: key.algorithm().as_str().to_string(),
        fingerprint: fingerprint_of(key),
    }
}

/// 解析一个 known_hosts 文件; 坏行跳过并告警, 不阻断整体查验
/// (用户手工编辑的 ~/.ssh/known_hosts 可能有坏行)。
fn load_entries(path: &Path) -> Vec<Entry> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new(); // 文件不存在视为无条目
    };
    KnownHosts::new(&content)
        .filter_map(|r| match r {
            Ok(e) => Some(e),
            Err(e) => {
                log::warn!("known_hosts 坏行跳过 {}: {e}", path.display());
                None
            }
        })
        .collect()
}

/// 在给定 known_hosts 文件集合里查验。
fn lookup_in(host: &str, port: u16, key: &PublicKey, files: &[PathBuf]) -> KeyLookup {
    let candidate = candidate_of(host, port);
    let mut applicable_plain: Vec<PublicKey> = Vec::new();
    let mut ca_matched = false;

    for file in files {
        for entry in load_entries(file) {
            if !host_patterns_match(entry.host_patterns(), &candidate) {
                continue;
            }
            match entry.marker() {
                // 吊销优先于一切: 同 key 命中 @revoked → 硬拒绝
                Some(Marker::Revoked) => {
                    if entry.public_key() == key {
                        return KeyLookup::Revoked;
                    }
                }
                Some(Marker::CertAuthority) => ca_matched = true,
                None => applicable_plain.push(entry.public_key().clone()),
            }
        }
    }

    // 任一适用普通记录精确匹配即接受; 全部试完才判定 mismatch
    if applicable_plain.iter().any(|k| k == key) {
        return KeyLookup::Match;
    }
    if !applicable_plain.is_empty() {
        return KeyLookup::Mismatch {
            stored: applicable_plain.iter().map(key_info).collect(),
        };
    }
    // 只有 CA 条目: 不做证书校验, 也绝不落入 TOFU 绕过 CA 策略
    if ca_matched {
        return KeyLookup::UnsupportedCertAuthority;
    }
    KeyLookup::Unknown
}

/// 标准查验: 用户 known_hosts + OneShell 信任文件
pub fn lookup(host: &str, port: u16, key: &PublicKey) -> KeyLookup {
    let mut files = Vec::new();
    if let Some(p) = user_known_hosts_path() {
        files.push(p);
    }
    if let Ok(p) = trust_file_path() {
        files.push(p);
    }
    lookup_in(host, port, key, &files)
}

// ── 连接路径集成 ────────────────────────────────────────────────────────

/// 主机密钥拒绝原因; ssh.rs 将其映射为类型化 ConnectError
#[derive(Debug, Clone)]
pub enum KeyRejection {
    Unknown {
        check_id: String,
        key_type: String,
        fingerprint: String,
    },
    Mismatch {
        key_type: String,
        fingerprint: String,
        stored: Vec<StoredKeyInfo>,
    },
    Revoked {
        key_type: String,
        fingerprint: String,
    },
    UnsupportedCertAuthority,
    /// 内部错误 (如 key 序列化失败); 不归于任何安全类别
    Internal(String),
}

/// check_server_key 的核心 (可测试): Ok(true) 放行; Err 携带拒绝原因。
/// `files: None` 走标准文件集 (用户 known_hosts + OneShell 信任文件)。
pub fn evaluate_key(
    host_id: &str,
    host: &str,
    port: u16,
    key: &PublicKey,
    pending: &PendingMap,
    files: Option<&[PathBuf]>,
) -> Result<bool, KeyRejection> {
    let result = match files {
        Some(f) => lookup_in(host, port, key, f),
        None => lookup(host, port, key),
    };
    let key_type = || key.algorithm().as_str().to_string();
    let fp = || fingerprint_of(key);
    match result {
        KeyLookup::Match => Ok(true),
        KeyLookup::Unknown => {
            let entry = PendingHostKey::new(host_id.into(), host.into(), port, key)
                .map_err(KeyRejection::Internal)?;
            let check_id = register_pending(pending, entry);
            Err(KeyRejection::Unknown {
                check_id,
                key_type: key_type(),
                fingerprint: fp(),
            })
        }
        KeyLookup::Mismatch { stored } => Err(KeyRejection::Mismatch {
            key_type: key_type(),
            fingerprint: fp(),
            stored,
        }),
        KeyLookup::Revoked => Err(KeyRejection::Revoked {
            key_type: key_type(),
            fingerprint: fp(),
        }),
        KeyLookup::UnsupportedCertAuthority => Err(KeyRejection::UnsupportedCertAuthority),
    }
}

// ── pending 登记 ────────────────────────────────────────────────────────

/// 登记一把未知 key, 返回 check_id。顺带清理过期/超量条目, 防泄漏。
pub fn register_pending(pending: &PendingMap, mut entry: PendingHostKey) -> String {
    let mut map = pending.lock();
    map.retain(|_, p| p.created.elapsed() < PENDING_TTL);
    if map.len() >= PENDING_CAP {
        // 超容量时丢最老的
        if let Some(oldest) = map
            .iter()
            .max_by_key(|(_, p)| p.created.elapsed())
            .map(|(id, _)| id.clone())
        {
            map.remove(&oldest);
        }
    }
    entry.created = Instant::now();
    let check_id = Uuid::new_v4().to_string();
    map.insert(check_id.clone(), entry);
    check_id
}

impl PendingHostKey {
    pub fn new(host_id: String, host: String, port: u16, key: &PublicKey) -> Result<Self, String> {
        Ok(Self {
            host_id,
            host,
            port,
            key_type: key.algorithm().as_str().to_string(),
            fingerprint: fingerprint_of(key),
            key_openssh: key
                .to_openssh()
                .map_err(|e| format!("序列化主机密钥失败: {e}"))?,
            created: Instant::now(),
        })
    }
}

// ── 信任文件写入 ────────────────────────────────────────────────────────

#[cfg(unix)]
fn current_euid() -> u32 {
    // SAFETY: geteuid 无参数无失败模式
    unsafe { libc::geteuid() }
}

static TRUST_WRITE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// 把一把 key 追加进 OneShell 信任文件。
/// 读全量 → 去重 → temp-file + rename 原子替换; 全局锁串行化并发写。
/// 跨进程咨询锁 (flock): 配合进程内的 TRUST_WRITE_LOCK, 覆盖多个
/// OneShell 实例 / 协作进程同时写信任文件的场景。非协作的外部写入者
/// 由读-改-写期间的身份+内容复查兜底。
#[cfg(unix)]
struct FlockGuard(fs::File);

#[cfg(unix)]
impl FlockGuard {
    fn acquire(dir: &Path) -> Result<Self, String> {
        use std::os::unix::io::AsRawFd;
        let f = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(dir.join(".known_hosts.lock"))
            .map_err(|e| format!("打开信任锁文件失败: {e}"))?;
        // SAFETY: fd 有效; flock 失败仅返回错误码
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(format!(
                "获取信任文件锁失败: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self(f))
    }
}

#[cfg(unix)]
impl Drop for FlockGuard {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn append_trust_to(path: &Path, host: &str, port: u16, key_openssh: &str) -> Result<(), String> {
    let _guard = TRUST_WRITE_LOCK.lock();

    let new_line = format!("{} {key_openssh}", candidate_of(host, port));

    let parent = path.parent().ok_or("信任文件路径无父目录")?;
    let existed = parent.exists();
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if !existed {
            // 新建的专用目录: 直接定 0700
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("设置信任目录权限失败: {e}"))?;
        } else {
            // 既有目录: 只校验归属, 不擅自修改权限策略
            let uid = fs::metadata(parent).map_err(|e| e.to_string())?.uid();
            if uid != current_euid() {
                return Err(format!("信任目录归属异常 (uid {uid}), 拒绝写入"));
            }
        }
    }

    // 跨进程锁先于一切读取: 首次读取即权威的, 去重判断在锁内完成
    #[cfg(unix)]
    let _flock = FlockGuard::acquire(parent)?;

    // 拒绝符号链接: 防止信任文件被指向用户控制的其他路径后遭替换
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            return Err(format!("信任文件是符号链接, 拒绝写入: {}", path.display()));
        }
    }

    // 既有信任文件校验归属
    #[cfg(unix)]
    if let Ok(meta) = fs::metadata(path) {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != current_euid() {
            return Err(format!("信任文件归属异常 (uid {}), 拒绝写入", meta.uid()));
        }
    }

    let existing = match fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("读取信任文件失败: {e}")),
    };
    if existing
        .as_deref()
        .is_some_and(|s| s.lines().any(|l| l.trim() == new_line))
    {
        return Ok(()); // 已信任, 幂等
    }

    let mut content = existing.clone().unwrap_or_default();
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&new_line);
    content.push('\n');

    // 乐观复查 (兜底非协作写入者, flock 不约束它们):
    // 原本存在 → 任何读取错误都致命; 原本不存在 → 仅接受 NotFound。
    let reread = match fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("复查信任文件失败: {e}")),
    };
    if reread != existing {
        return Err("信任文件在写入期间被外部修改, 请重试".into());
    }

    // 随机临时名 + O_EXCL: 防陈旧文件/并行进程/预置符号链接干扰
    let tmp = parent.join(format!(".known_hosts.{}.tmp", Uuid::new_v4()));
    let write_result = (|| -> Result<(), String> {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut tmp_file = opts
            .open(&tmp)
            .map_err(|e| format!("创建临时信任文件失败: {e}"))?;
        {
            use std::io::Write;
            tmp_file
                .write_all(content.as_bytes())
                .and_then(|()| tmp_file.sync_all())
                .map_err(|e| format!("写入临时信任文件失败: {e}"))?;
        }
        drop(tmp_file);
        fs::rename(&tmp, path).map_err(|e| format!("替换信任文件失败: {e}"))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&tmp); // 任何错误路径都清理临时文件
    }
    write_result?;

    // fsync 父目录, 保证 rename 的目录项落盘
    #[cfg(unix)]
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

// ── Tauri 命令 ──────────────────────────────────────────────────────────

/// 接受逻辑 (可测试): 按 check_id 校验 pending 记录并持久化。
/// 过期记录视为无效并移除 — 用户盯着弹窗半小时后点“信任”不该写入陈旧 key。
/// 写入失败时记录保留, 前端可安全重试; 锁住 map 完成整个校验+写入,
/// 并发重复接受只会有一方成功。
pub(crate) fn accept_pending(pending: &PendingMap, check_id: &str, trust_path: &Path) -> Result<(), String> {
    let mut map = pending.lock();
    let entry = map.get(check_id).ok_or("该确认请求已过期, 请重新连接")?;
    if entry.created.elapsed() >= PENDING_TTL {
        map.remove(check_id);
        return Err("该确认请求已过期, 请重新连接".into());
    }
    append_trust_to(trust_path, &entry.host, entry.port, &entry.key_openssh)?;
    map.remove(check_id);
    Ok(())
}

/// 用户确认信任: 按 check_id 取出 pending 记录, 持久化到信任文件。
/// 之后前端重新发起连接, check_server_key 会用持久化的记录对实际呈现的
/// key 做精确再校验 — 两次连接之间 key 被换掉会走 mismatch 硬失败。
#[tauri::command]
pub fn accept_host_key(state: State<'_, AppState>, check_id: String) -> Result<(), String> {
    let path = trust_file_path()?;
    accept_pending(&state.pending_host_keys, &check_id, &path)
}

/// 用户拒绝 / 关闭弹窗: 丢弃 pending 记录
#[tauri::command]
pub fn dismiss_host_key(state: State<'_, AppState>, check_id: String) -> Result<(), String> {
    state.pending_host_keys.lock().remove(&check_id);
    Ok(())
}

// ── 测试 ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::parse_public_key_base64;

    // ssh-keygen 生成的测试 key 与对应 SHA256 指纹 (ssh-keygen -lf 输出),
    // 用于独立交叉验证指纹计算格式。
    const ED25519_B64: &str =
        "AAAAC3NzaC1lZDI1NTE5AAAAIAv7LkWvYCqZ8CObhgvuYzRCPZqKgp2v5My6zPOl7jUX";
    const ED25519_FP: &str = "SHA256:Tc0OqI/PKFuYHFoCsAwBpM1G+Cf8NaVo00ovvqFNJig";
    const ECDSA_B64: &str = "AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBDpMA7+lwR6Vd0pg6cdN/66+kFOQtGB+nJ3CenSj5QiKrKj5QgsiNEZhbysYmJLB+H2ilxMo4T9hBh7sBqb8/t4=";
    const ECDSA_FP: &str = "SHA256:dFSp49WZP3dWVGcAjYDY/rI0YJCsvTKB0vILHwtKk+w";
    const RSA_B64: &str = "AAAAB3NzaC1yc2EAAAADAQABAAABgQC/CIkggOIIthWLQ9l682i6kVsLX27pp5qo4mMfHI+AMhMzjpRRyUY8tzFkWi2RJH5IeduvGCk+bOq1Ae+J9LCDMkmodsyBJ5eoX8jZhbPCkF9CeftiAG0gUhMhf208dR5dDXckGJYbWlOYolEFimGtHj3BnDX9pE7z5o2M2S3bU26dMMrVwu0GAO3mgdocqepb5Cx1yPFrhxFQpRVgOCR05ctTm8BPaQmW920wQIhFmi7UUbjrGAFBy42H4rAxLDLoicQDIBnhecV3DRzuQbEsCMpnAETuWPYotXdE3MdT31YttnCwv7y+g4gknRiTXxuR0sZl2JJBgJAVTLd4sUuRo4Wtk1pndtjnQP9+Zz6a42BH0pXgfvFBlX/CjrfRd6EUTmQpRhzuS4vu451Tz4ops++Jj93pg1vTAE9kpSzynvI6xm9Fbj/GF8TUIT5TeiTgnzuSwljW9VRzT7OxSmcGSWX1nHjjirCOXH+ShlXAmqamx3PnYGjhRo/s+JjU2jM=";
    const RSA_FP: &str = "SHA256:qJThp9oHmZL2pB2rBnO22dRdq/u47Q3AllH2IkrLF2s";

    fn key(b64: &str) -> PublicKey {
        parse_public_key_base64(b64).unwrap()
    }

    fn ed() -> PublicKey {
        key(ED25519_B64)
    }
    fn ec() -> PublicKey {
        key(ECDSA_B64)
    }
    fn rsa() -> PublicKey {
        key(RSA_B64)
    }

    // OpenSSH 生成夹具: ssh-keygen 按格式文档手工构造 + ssh-keygen -H 散列。
    // 匹配行为已逐条对照 ssh-keygen -F 输出验证。
    const FIXTURE: &str = concat!(
        "plain.example.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAv7LkWvYCqZ8CObhgvuYzRCPZqKgp2v5My6zPOl7jUX\n",
        "*.wild.example ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBDpMA7+lwR6Vd0pg6cdN/66+kFOQtGB+nJ3CenSj5QiKrKj5QgsiNEZhbysYmJLB+H2ilxMo4T9hBh7sBqb8/t4=\n",
        "*.example.com,!secure.example.com ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQC/CIkggOIIthWLQ9l682i6kVsLX27pp5qo4mMfHI+AMhMzjpRRyUY8tzFkWi2RJH5IeduvGCk+bOq1Ae+J9LCDMkmodsyBJ5eoX8jZhbPCkF9CeftiAG0gUhMhf208dR5dDXckGJYbWlOYolEFimGtHj3BnDX9pE7z5o2M2S3bU26dMMrVwu0GAO3mgdocqepb5Cx1yPFrhxFQpRVgOCR05ctTm8BPaQmW920wQIhFmi7UUbjrGAFBy42H4rAxLDLoicQDIBnhecV3DRzuQbEsCMpnAETuWPYotXdE3MdT31YttnCwv7y+g4gknRiTXxuR0sZl2JJBgJAVTLd4sUuRo4Wtk1pndtjnQP9+Zz6a42BH0pXgfvFBlX/CjrfRd6EUTmQpRhzuS4vu451Tz4ops++Jj93pg1vTAE9kpSzynvI6xm9Fbj/GF8TUIT5TeiTgnzuSwljW9VRzT7OxSmcGSWX1nHjjirCOXH+ShlXAmqamx3PnYGjhRo/s+JjU2jM=\n",
        "[port.example.com]:2222 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAv7LkWvYCqZ8CObhgvuYzRCPZqKgp2v5My6zPOl7jUX\n",
        "@revoked revoked.example.com ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBDpMA7+lwR6Vd0pg6cdN/66+kFOQtGB+nJ3CenSj5QiKrKj5QgsiNEZhbysYmJLB+H2ilxMo4T9hBh7sBqb8/t4=\n",
        "|1|T9m/DTAFGpZAZT0JnaOLtY0/XFE=|ecpQxWrQEdZcYQ5eAF+T5cDUNew= ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAv7LkWvYCqZ8CObhgvuYzRCPZqKgp2v5My6zPOl7jUX\n",
    );

    fn fixture_file() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("known_hosts");
        fs::write(&p, FIXTURE).unwrap();
        (dir, p)
    }

    #[test]
    fn fingerprint_vectors_match_ssh_keygen() {
        assert_eq!(fingerprint_of(&ed()), ED25519_FP);
        assert_eq!(fingerprint_of(&ec()), ECDSA_FP);
        assert_eq!(fingerprint_of(&rsa()), RSA_FP);
    }

    #[test]
    fn key_type_strings() {
        assert_eq!(ed().algorithm().as_str(), "ssh-ed25519");
        assert_eq!(ec().algorithm().as_str(), "ecdsa-sha2-nistp256");
        assert_eq!(rsa().algorithm().as_str(), "ssh-rsa");
    }

    #[test]
    fn openssh_fixture_lookup_matrix() {
        let (_dir, f) = fixture_file();
        let at = |h: &str, p: u16, k: &PublicKey| lookup_in(h, p, k, std::slice::from_ref(&f));

        // 普通精确条目
        assert!(matches!(
            at("plain.example.com", 22, &ed()),
            KeyLookup::Match
        ));
        // 通配符跨点匹配
        assert!(matches!(
            at("foo.wild.example", 22, &ec()),
            KeyLookup::Match
        ));
        // 逗号列表 + 通配
        assert!(matches!(
            at("bar.example.com", 22, &rsa()),
            KeyLookup::Match
        ));
        // 否定排除 (ssh-keygen -F secure.example.com 同样查不到)
        assert!(matches!(
            at("secure.example.com", 22, &rsa()),
            KeyLookup::Unknown
        ));
        // 非标端口: [host]:port 条目命中
        assert!(matches!(
            at("port.example.com", 2222, &ed()),
            KeyLookup::Match
        ));
        // 关键不变量: 普通 `host` 条目不自动覆盖非标端口连接 —
        // [port.example.com]:2222 有条目, 但 22 端口无适用普通条目…
        // (注意 *.example.com,!secure.example.com 的 RSA 条目适用于 22 端口,
        //  呈现 ED25519 → Mismatch 而非 Match, 证明没有跨端口信任)
        match at("port.example.com", 22, &ed()) {
            KeyLookup::Mismatch { stored } => {
                assert_eq!(stored.len(), 1);
                assert_eq!(stored[0].fingerprint, RSA_FP);
            }
            _ => panic!("22 端口不应命中 [host]:2222 条目"),
        }
        // 反向: 普通 plain.example.com 条目不覆盖其 2222 端口
        assert!(matches!(
            at("plain.example.com", 2222, &ed()),
            KeyLookup::Unknown
        ));
        // 散列条目 (ssh-keygen -H 生成) 命中
        assert!(matches!(
            at("hashme.example.com", 22, &ed()),
            KeyLookup::Match
        ));
        // 散列条目不匹配其他主机
        assert!(matches!(
            at("other.example.com", 22, &ed()),
            KeyLookup::Mismatch { .. } // 命中 *.example.com RSA 条目 → mismatch
        ));
        // @revoked 同 key → 硬拒绝
        assert!(matches!(
            at("revoked.example.com", 22, &ec()),
            KeyLookup::Revoked
        ));
        // @revoked 不同 key → 该 revoked 条目不拦截; 但 *.example.com 通配的
        // RSA 条目适用 → Mismatch (与 ssh-keygen -F 能找到通配条目一致)
        match at("revoked.example.com", 22, &ed()) {
            KeyLookup::Mismatch { stored } => {
                assert_eq!(stored.len(), 1);
                assert_eq!(stored[0].fingerprint, RSA_FP);
            }
            other => panic!("revoked 不同 key 应为 Mismatch, 实际 {other:?}"),
        }
    }

    #[test]
    fn cert_authority_only_match_is_explicit_error_not_tofu() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("kh");
        fs::write(
            &p,
            format!("@cert-authority *.corp.com ssh-ed25519 {ED25519_B64}\n"),
        )
        .unwrap();
        match lookup_in("web.corp.com", 22, &ed(), std::slice::from_ref(&p)) {
            KeyLookup::UnsupportedCertAuthority => {}
            other => panic!("CA-only 应显式报错, 实际 {other:?}"),
        }
        // 有普通条目时 CA 不干扰
        fs::write(
            &p,
            format!(
                "@cert-authority *.corp.com ssh-rsa {RSA_B64}\nweb.corp.com ssh-ed25519 {ED25519_B64}\n"
            ),
        )
        .unwrap();
        assert!(matches!(
            lookup_in("web.corp.com", 22, &ed(), &[p]),
            KeyLookup::Match
        ));
    }

    #[test]
    fn revoked_beats_plain_trust_entry() {
        // 同一把 key 同时有普通信任条目和 revoked 条目 → 拒绝 (吊销优先)
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("kh");
        fs::write(
            &p,
            format!(
                "host.example ssh-ed25519 {ED25519_B64}\n@revoked host.example ssh-ed25519 {ED25519_B64}\n"
            ),
        )
        .unwrap();
        assert!(matches!(
            lookup_in("host.example", 22, &ed(), &[p]),
            KeyLookup::Revoked
        ));
    }

    #[test]
    fn algorithm_rotation_matches_any_applicable_entry() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("kh");
        fs::write(
            &p,
            format!("example.com ssh-rsa {RSA_B64}\nexample.com ssh-ed25519 {ED25519_B64}\n"),
        )
        .unwrap();
        assert!(matches!(
            lookup_in("example.com", 22, &ed(), std::slice::from_ref(&p)),
            KeyLookup::Match
        ));
        assert!(matches!(
            lookup_in("example.com", 22, &rsa(), std::slice::from_ref(&p)),
            KeyLookup::Match
        ));
    }

    #[test]
    fn mismatch_reports_all_stored_fingerprints() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("kh");
        fs::write(
            &p,
            format!("example.com ssh-rsa {RSA_B64}\nexample.com ecdsa-sha2-nistp256 {ECDSA_B64}\n"),
        )
        .unwrap();
        match lookup_in("example.com", 22, &ed(), &[p]) {
            KeyLookup::Mismatch { stored } => {
                assert_eq!(stored.len(), 2, "应携带全部适用记录指纹");
                let fps: Vec<&str> = stored.iter().map(|s| s.fingerprint.as_str()).collect();
                assert!(fps.contains(&RSA_FP));
                assert!(fps.contains(&ECDSA_FP));
            }
            _ => panic!("应为 Mismatch"),
        }
    }

    #[test]
    fn bad_lines_are_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("kh");
        fs::write(
            &p,
            format!(
                "这不是合法行\ngarbage only-one-field\nexample.com ssh-ed25519 {ED25519_B64}\n"
            ),
        )
        .unwrap();
        assert!(matches!(
            lookup_in("example.com", 22, &ed(), &[p]),
            KeyLookup::Match
        ));
    }

    #[test]
    fn glob_matcher_unit() {
        assert!(glob_match("*.example.com", "a.b.example.com")); // 跨点
        assert!(glob_match("a?c", "abc"));
        assert!(glob_match("abc", "abc"));
        assert!(!glob_match("abc", "abcd"));
        assert!(!glob_match("*.example.com", "[a.example.com]:2222")); // 候选含端口串不匹配
        assert!(glob_match("[a.example.com]:2222", "[a.example.com]:2222"));
        assert!(glob_match("EXAMPLE.com", "example.COM")); // ASCII 大小写不敏感
        assert!(glob_match("*", "anything"));
        assert!(glob_match("a*b*c", "aXbYc"));
        // IPv6 候选: 22 端口为字面量, 非标端口为 [host]:port
        assert_eq!(candidate_of("::1", 22), "::1");
        assert_eq!(candidate_of("::1", 2222), "[::1]:2222");
        assert!(glob_match("::1", "::1"));
        assert!(glob_match("[::1]:2222", "[::1]:2222"));
        assert!(!glob_match("::1", "[::1]:2222"));
    }

    #[test]
    fn lookup_unknown_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        assert!(matches!(
            lookup_in("example.com", 22, &ed(), &[missing]),
            KeyLookup::Unknown
        ));
    }

    #[test]
    fn append_trust_creates_dedupes_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("known_hosts");
        let openssh = ed().to_openssh().unwrap();

        append_trust_to(&path, "example.com", 22, &openssh).unwrap();
        append_trust_to(&path, "example.com", 22, &openssh).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert_eq!(raw.lines().count(), 1, "重复追加应去重");

        assert!(matches!(
            lookup_in("example.com", 22, &ed(), &[path]),
            KeyLookup::Match
        ));
    }

    #[test]
    fn append_trust_appends_without_trailing_newline_and_non22_port() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        fs::write(&path, "other.host ssh-ed25519 AAAA-no-newline").unwrap();

        let openssh = ec().to_openssh().unwrap();
        append_trust_to(&path, "example.com", 2222, &openssh).unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        assert_eq!(raw.lines().count(), 2, "应正确补换行后追加");
        assert!(raw.contains("[example.com]:2222"));
        assert!(matches!(
            lookup_in("example.com", 2222, &ec(), &[path]),
            KeyLookup::Match
        ));
    }

    #[test]
    fn append_trust_concurrent_writers_do_not_lose_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let o1 = ed().to_openssh().unwrap();
        let o2 = ec().to_openssh().unwrap();

        let mut handles = Vec::new();
        for (host, port, o) in [("a.example.com", 22u16, o1), ("b.example.com", 2222u16, o2)] {
            let p = path.clone();
            handles.push(std::thread::spawn(move || {
                append_trust_to(&p, host, port, &o).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let raw = fs::read_to_string(&path).unwrap();
        assert_eq!(raw.lines().count(), 2, "并发写不应丢条目: {raw}");
    }

    // ── 接受/持久化的对抗性测试 ──

    fn fresh_pending() -> (PendingMap, tempfile::TempDir, PathBuf) {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let dir = tempfile::tempdir().unwrap();
        let trust = dir.path().join("known_hosts");
        (pending, dir, trust)
    }

    #[test]
    fn accept_unknown_check_id_fails() {
        let (pending, _dir, trust) = fresh_pending();
        assert!(accept_pending(&pending, "不存在的id", &trust).is_err());
        assert!(!trust.exists(), "失败不应创建信任文件");
    }

    #[test]
    fn accept_persists_exact_pending_entry_not_concurrent_one() {
        // 同一 host:port 两次连接尝试 (例如两个窗口): 各拿各的 check_id,
        // 用户批准哪个就持久化哪个, 互不覆盖
        let (pending, _dir, trust) = fresh_pending();
        let id_ed = register_pending(
            &pending,
            PendingHostKey::new("h1".into(), "example.com".into(), 22, &ed()).unwrap(),
        );
        let id_ec = register_pending(
            &pending,
            PendingHostKey::new("h1".into(), "example.com".into(), 22, &ec()).unwrap(),
        );
        assert_ne!(id_ed, id_ec);

        accept_pending(&pending, &id_ec, &trust).unwrap();
        assert!(matches!(
            lookup_in("example.com", 22, &ec(), std::slice::from_ref(&trust)),
            KeyLookup::Match
        ));
        // 未被批准的那把 key 仍是 mismatch, 不是被连带信任
        assert!(matches!(
            lookup_in("example.com", 22, &ed(), &[trust]),
            KeyLookup::Mismatch { .. }
        ));
    }

    #[test]
    fn accept_is_single_use() {
        let (pending, _dir, trust) = fresh_pending();
        let id = register_pending(
            &pending,
            PendingHostKey::new("h1".into(), "example.com".into(), 22, &ed()).unwrap(),
        );
        accept_pending(&pending, &id, &trust).unwrap();
        assert!(
            accept_pending(&pending, &id, &trust).is_err(),
            "同一 check_id 不可重复使用"
        );
    }

    #[test]
    fn accept_expired_check_id_fails() {
        let (pending, _dir, trust) = fresh_pending();
        let mut e = PendingHostKey::new("h1".into(), "example.com".into(), 22, &ed()).unwrap();
        e.created = Instant::now() - PENDING_TTL - Duration::from_secs(1);
        pending.lock().insert("stale-id".into(), e);
        assert!(accept_pending(&pending, "stale-id", &trust).is_err());
        assert!(!trust.exists(), "过期确认不应写入信任文件");
    }

    #[test]
    fn evaluate_key_all_outcomes() {
        let dir = tempfile::tempdir().unwrap();
        let kh = dir.path().join("kh");
        fs::write(
            &kh,
            format!(
                "good.example.com ssh-ed25519 {ED25519_B64}\n\
                 rotated.example.com ssh-rsa {RSA_B64}\n\
                 @revoked bad.example.com ecdsa-sha2-nistp256 {ECDSA_B64}\n\
                 @cert-authority *.ca.example.com ssh-ed25519 {ED25519_B64}\n"
            ),
        )
        .unwrap();
        let files = std::slice::from_ref(&kh);

        // Match → 放行
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        assert!(evaluate_key("h", "good.example.com", 22, &ed(), &pending, Some(files)).unwrap());
        assert!(pending.lock().is_empty(), "Match 不应登记 pending");

        // Unknown → 拒绝 + pending 登记, check_id 可取回同一把 key
        let Err(KeyRejection::Unknown { check_id, .. }) =
            evaluate_key("h1", "new.example.com", 22, &ed(), &pending, Some(files))
        else {
            panic!("应为 Unknown")
        };
        {
            let map = pending.lock();
            let p = map.get(&check_id).expect("pending 已登记");
            assert_eq!(p.host_id, "h1");
            assert_eq!(p.fingerprint, ED25519_FP);
        }

        // Mismatch → 拒绝, 携带存量指纹
        match evaluate_key("h", "rotated.example.com", 22, &ed(), &pending, Some(files)) {
            Err(KeyRejection::Mismatch {
                stored,
                fingerprint,
                ..
            }) => {
                assert_eq!(fingerprint, ED25519_FP);
                assert_eq!(stored[0].fingerprint, RSA_FP);
            }
            other => panic!("应为 Mismatch: {other:?}"),
        }

        // Revoked → 拒绝
        assert!(matches!(
            evaluate_key("h", "bad.example.com", 22, &ec(), &pending, Some(files)),
            Err(KeyRejection::Revoked { .. })
        ));

        // CA-only → 显式错误
        assert!(matches!(
            evaluate_key("h", "web.ca.example.com", 22, &ed(), &pending, Some(files)),
            Err(KeyRejection::UnsupportedCertAuthority)
        ));

        // 以上所有拒绝路径都返回 Ok(false) 等效物 → russh 会中止握手,
        // 不可能进入 authenticate 阶段
    }

    #[test]
    fn accept_write_failure_keeps_pending_for_retry() {
        // 写入失败 → pending 记录保留; 修复后用同一 check_id 重试成功
        let (pending, dir, trust) = fresh_pending();
        let id = register_pending(
            &pending,
            PendingHostKey::new("h1".into(), "example.com".into(), 22, &ed()).unwrap(),
        );
        // 父路径是一个普通文件 → create_dir_all 必失败
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "x").unwrap();
        let bad_trust = blocker.join("known_hosts");

        assert!(accept_pending(&pending, &id, &bad_trust).is_err());
        assert!(
            pending.lock().contains_key(&id),
            "写失败后 pending 应保留可重试"
        );

        accept_pending(&pending, &id, &trust).unwrap();
        assert!(!pending.lock().contains_key(&id), "成功后记录应移除");
        assert!(matches!(
            lookup_in("example.com", 22, &ed(), &[trust]),
            KeyLookup::Match
        ));
    }

    #[test]
    fn accept_expired_check_id_preserves_existing_trust_file() {
        // 已有信任文件: 过期确认失败且文件内容原封不动
        let (pending, _dir, trust) = fresh_pending();
        let openssh = ec().to_openssh().unwrap();
        append_trust_to(&trust, "other.example.com", 22, &openssh).unwrap();
        let before = fs::read_to_string(&trust).unwrap();

        let mut e = PendingHostKey::new("h1".into(), "example.com".into(), 22, &ed()).unwrap();
        e.created = Instant::now() - PENDING_TTL - Duration::from_secs(1);
        pending.lock().insert("stale-id".into(), e);

        assert!(accept_pending(&pending, "stale-id", &trust).is_err());
        assert!(!pending.lock().contains_key("stale-id"), "过期记录应被移除");
        assert_eq!(
            fs::read_to_string(&trust).unwrap(),
            before,
            "信任文件不应被改动"
        );
    }

    #[test]
    fn retry_with_changed_key_after_accept_is_mismatch() {
        // 批准 key A → 重连时服务器呈现了 key B: 持久化记录与呈现不符 →
        // mismatch 硬失败 (TOFU 间隙 MITM 防线)
        let (pending, _dir, trust) = fresh_pending();
        let id = register_pending(
            &pending,
            PendingHostKey::new("h1".into(), "example.com".into(), 22, &ed()).unwrap(),
        );
        accept_pending(&pending, &id, &trust).unwrap();
        match lookup_in("example.com", 22, &ec(), &[trust]) {
            KeyLookup::Mismatch { stored } => {
                assert_eq!(stored[0].fingerprint, ED25519_FP);
            }
            other => panic!("key 变更应为 Mismatch, 实际 {other:?}"),
        }
    }

    #[test]
    fn pending_registration_and_eviction() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let k = ed();
        let entry = PendingHostKey::new("h1".into(), "example.com".into(), 22, &k).unwrap();
        let id = register_pending(&pending, entry);
        assert!(pending.lock().contains_key(&id));

        // 过期条目在下次登记时被清理
        {
            let mut map = pending.lock();
            map.get_mut(&id).unwrap().created =
                Instant::now() - PENDING_TTL - Duration::from_secs(1);
        }
        let id2 = register_pending(
            &pending,
            PendingHostKey::new("h2".into(), "other".into(), 22, &k).unwrap(),
        );
        let map = pending.lock();
        assert!(!map.contains_key(&id), "过期条目应被清理");
        assert!(map.contains_key(&id2));
    }
}
