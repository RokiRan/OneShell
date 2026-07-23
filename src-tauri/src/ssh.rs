use std::{collections::HashMap, sync::Arc, time::Duration};

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use parking_lot::Mutex;
use russh::{
    client::{self, ChannelOpenHandle, Handle, Msg, Session},
    keys::{PrivateKeyWithHashAlg, PublicKey},
    Channel, ChannelId, ChannelMsg, ChannelOpenFailure, CryptoVec,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{
    known_hosts::{self, KeyRejection, PendingMap, StoredKeyInfo},
    models::{AuthMethod, HostConfig, SessionInfo},
    AppState,
};

/// 远端端口转发: 绑定端口 -> (目标主机, 目标端口)
pub type RemoteForwardMap = Arc<Mutex<HashMap<u32, (String, u32)>>>;

pub struct Client {
    pub remote_forwards: RemoteForwardMap,
    /// 连接目标 (主机密钥查验用)
    pub host_id: String,
    pub host: String,
    pub port: u16,
    /// check_server_key 拒绝时记录原因, connect 据此构造类型化错误
    pub key_rejection: Arc<Mutex<Option<KeyRejection>>>,
    pub pending_host_keys: PendingMap,
    /// 信任文件覆盖 (测试用; None = 标准文件集)
    pub trust_files: Option<Vec<std::path::PathBuf>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    /// 主机密钥查验。永不阻塞等用户决定: 未知 key 拒绝并登记 pending,
    /// 由前端弹窗确认后重连; mismatch/revoked 硬失败。
    async fn check_server_key(&mut self, key: &PublicKey) -> Result<bool, Self::Error> {
        match known_hosts::evaluate_key(
            &self.host_id,
            &self.host,
            self.port,
            key,
            &self.pending_host_keys,
            self.trust_files.as_deref(),
        ) {
            Ok(accepted) => Ok(accepted),
            Err(rejection) => {
                *self.key_rejection.lock() = Some(rejection);
                Ok(false) // russh 中止握手, 不会进入认证阶段
            }
        }
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<Msg>,
        _connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let target = self.remote_forwards.lock().get(&connected_port).cloned();
        let Some((host, port)) = target else {
            reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        };
        reply.accept().await;
        tokio::spawn(async move {
            match tokio::net::TcpStream::connect((host.as_str(), port as u16)).await {
                Ok(tcp) => pipe_channel_tcp(channel, tcp).await,
                Err(_) => {
                    let _ = channel.close().await;
                }
            }
        });
        Ok(())
    }
}

/// 把 SSH channel 与本地 TCP 流双向桥接 (端口转发用)
pub async fn pipe_channel_tcp(mut channel: Channel<Msg>, tcp: tokio::net::TcpStream) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (mut rd, mut wr) = tcp.into_split();
    let mut buf = vec![0u8; 32 * 1024];
    loop {
        tokio::select! {
            msg = channel.wait() => match msg {
                Some(ChannelMsg::Data { data }) => {
                    if wr.write_all(&data).await.is_err() { break; }
                }
                Some(ChannelMsg::Eof) => { let _ = wr.shutdown().await; }
                Some(ChannelMsg::Close) | None => break,
                _ => {}
            },
            n = rd.read(&mut buf) => match n {
                Ok(0) => { let _ = channel.eof().await; break; }
                Ok(n) => {
                    if channel.data(&buf[..n]).await.is_err() { break; }
                }
                Err(_) => { let _ = channel.eof().await; break; }
            },
        }
    }
    let _ = channel.close().await;
}

pub enum ShellCtl {
    Data(Vec<u8>),
    Resize(u32, u32),
    Close,
}

pub struct SshSession {
    pub id: String,
    pub host_id: String,
    pub label: String,
    pub handle: tokio::sync::Mutex<Handle<Client>>,
    pub shells: Mutex<HashMap<String, mpsc::UnboundedSender<ShellCtl>>>,
    pub remote_forwards: RemoteForwardMap,
}

#[derive(Serialize, Clone)]
struct ShellDataPayload {
    session_id: String,
    shell_id: String,
    data: String, // base64
}

#[derive(Serialize, Clone)]
struct ShellExitPayload {
    session_id: String,
    shell_id: String,
}

/// 连接错误 (类型化, serde tag 直达前端, 不做 JSON-in-String 解析)。
/// unknown_host_key 可经 TOFU 弹窗确认; host_key_mismatch / host_key_revoked
/// 是硬失败路径, 前端不提供“仍然连接”。
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConnectError {
    UnknownHostKey {
        check_id: String,
        host_id: String,
        host: String,
        port: u16,
        key_type: String,
        fingerprint: String,
        message: String,
    },
    HostKeyMismatch {
        host_id: String,
        host: String,
        port: u16,
        key_type: String,
        fingerprint: String,
        stored: Vec<StoredKeyInfo>,
        message: String,
    },
    HostKeyRevoked {
        host_id: String,
        host: String,
        port: u16,
        key_type: String,
        fingerprint: String,
        message: String,
    },
    UnsupportedCertAuthority {
        host_id: String,
        host: String,
        port: u16,
        message: String,
    },
    Other {
        message: String,
    },
}

/// KeyRejection → ConnectError (独立函数以便单测)
fn rejection_to_connect_error(rejection: &KeyRejection, host: &HostConfig) -> ConnectError {
    let (host_id, hostname, port) = (host.id.clone(), host.host.clone(), host.port);
    match rejection {
        KeyRejection::Unknown {
            check_id,
            key_type,
            fingerprint,
        } => ConnectError::UnknownHostKey {
            check_id: check_id.clone(),
            host_id,
            host: hostname.clone(),
            port,
            key_type: key_type.clone(),
            fingerprint: fingerprint.clone(),
            message: format!("首次连接 {hostname}, 主机密钥未知"),
        },
        KeyRejection::Mismatch {
            key_type,
            fingerprint,
            stored,
        } => ConnectError::HostKeyMismatch {
            host_id,
            host: hostname.clone(),
            port,
            key_type: key_type.clone(),
            fingerprint: fingerprint.clone(),
            stored: stored.clone(),
            message: format!("警告: {hostname} 的主机密钥与已记录的不一致, 可能遭受中间人攻击"),
        },
        KeyRejection::Revoked {
            key_type,
            fingerprint,
        } => ConnectError::HostKeyRevoked {
            host_id,
            host: hostname.clone(),
            port,
            key_type: key_type.clone(),
            fingerprint: fingerprint.clone(),
            message: format!("{hostname} 的主机密钥已被吊销 (known_hosts @revoked)"),
        },
        KeyRejection::UnsupportedCertAuthority => ConnectError::UnsupportedCertAuthority {
            host_id,
            host: hostname.clone(),
            port,
            message: format!("{hostname} 使用证书机构签发的密钥, 当前版本不支持证书校验, 拒绝连接"),
        },
        KeyRejection::Internal(msg) => ConnectError::Other {
            message: format!("主机密钥查验内部错误: {msg}"),
        },
    }
}

async fn authenticate(handle: &mut Handle<Client>, host: &HostConfig) -> Result<(), String> {
    let result = match &host.auth {
        AuthMethod::Password { password } => handle
            .authenticate_password(&host.username, password)
            .await
            .map_err(|e| format!("认证错误: {e}"))?,
        AuthMethod::Key {
            key_path,
            passphrase,
        } => {
            let key = russh::keys::load_secret_key(key_path, passphrase.as_deref())
                .map_err(|e| format!("私钥加载失败 ({key_path}): {e}"))?;
            let hash = handle
                .best_supported_rsa_hash()
                .await
                .ok()
                .flatten()
                .flatten();
            handle
                .authenticate_publickey(
                    &host.username,
                    PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await
                .map_err(|e| format!("认证错误: {e}"))?
        }
    };
    if result.success() {
        Ok(())
    } else {
        Err("SSH 认证失败: 用户名或凭据不正确".into())
    }
}

#[tauri::command]
pub async fn connect(
    state: State<'_, AppState>,
    host_id: String,
) -> Result<SessionInfo, ConnectError> {
    let host = state
        .store
        .get(&host_id)
        .map_err(|message| ConnectError::Other { message })?;
    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(20)),
        ..Default::default()
    });
    let remote_forwards: RemoteForwardMap = Arc::new(Mutex::new(HashMap::new()));
    // 每次连接新建拒绝记录, 不可能读到上一次尝试的残留状态
    let key_rejection: Arc<Mutex<Option<KeyRejection>>> = Arc::new(Mutex::new(None));
    let handler = Client {
        remote_forwards: remote_forwards.clone(),
        host_id: host.id.clone(),
        host: host.host.clone(),
        port: host.port,
        key_rejection: key_rejection.clone(),
        pending_host_keys: state.pending_host_keys.clone(),
        trust_files: None,
    };
    let mut handle = match tokio::time::timeout(Duration::from_secs(15), async {
        client::connect(config, (host.host.as_str(), host.port), handler).await
    })
    .await
    {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => {
            // check_server_key 拒绝时 russh 中止握手并报错;
            // 优先返回类型化的主机密钥错误, 而不是笼统的连接失败
            if let Some(rejection) = key_rejection.lock().take() {
                return Err(rejection_to_connect_error(&rejection, &host));
            }
            return Err(ConnectError::Other {
                message: format!("连接失败: {e}"),
            });
        }
        Err(_) => {
            // 超时也可能发生在 check_server_key 记录拒绝之后
            // (russh 尚未返回); 同样优先映射为主机密钥错误
            if let Some(rejection) = key_rejection.lock().take() {
                return Err(rejection_to_connect_error(&rejection, &host));
            }
            return Err(ConnectError::Other {
                message: format!("连接超时: {}:{}", host.host, host.port),
            });
        }
    };

    // 主机密钥查验通过后才取凭据: 不信任的主机不触发钥匙串读取/解锁提示。
    // 阻塞 IO 移出运行时线程; 不做明文回退。
    let auth = {
        let store = state.store.clone();
        let hid = host_id.clone();
        tokio::task::spawn_blocking(move || store.resolve_auth(&hid))
            .await
            .map_err(|e| ConnectError::Other {
                message: e.to_string(),
            })?
            .map_err(|message| ConnectError::Other { message })?
    };
    let host = HostConfig { auth, ..host };

    authenticate(&mut handle, &host)
        .await
        .map_err(|message| ConnectError::Other { message })?;

    let session_id = Uuid::new_v4().to_string();
    let label = format!("{}@{}:{}", host.username, host.host, host.port);
    let session = Arc::new(SshSession {
        id: session_id.clone(),
        host_id: host.id.clone(),
        label: if host.name.is_empty() {
            label.clone()
        } else {
            format!("{} ({})", host.name, label)
        },
        handle: tokio::sync::Mutex::new(handle),
        shells: Mutex::new(HashMap::new()),
        remote_forwards,
    });
    state
        .sessions
        .lock()
        .insert(session_id.clone(), session.clone());

    Ok(SessionInfo {
        session_id,
        host_id: host.id,
        label: session.label.clone(),
    })
}

pub(crate) fn get_session(state: &AppState, session_id: &str) -> Result<Arc<SshSession>, String> {
    state
        .sessions
        .lock()
        .get(session_id)
        .cloned()
        .ok_or_else(|| format!("会话不存在: {session_id}"))
}

#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let session = state.sessions.lock().remove(&session_id);
    if let Some(session) = session {
        let handle = session.handle.lock().await;
        let _ = handle
            .disconnect(russh::Disconnect::ByApplication, "", "")
            .await;
    }
    Ok(())
}

#[tauri::command]
pub async fn open_shell(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    cols: u32,
    rows: u32,
) -> Result<String, String> {
    let session = get_session(&state, &session_id)?;
    let mut channel = {
        let handle = session.handle.lock().await;
        handle
            .channel_open_session()
            .await
            .map_err(|e| format!("打开通道失败: {e}"))?
    };
    channel
        .request_pty(true, "xterm-256color", cols, rows, 0, 0, &[])
        .await
        .map_err(|e| format!("申请 PTY 失败: {e}"))?;
    channel
        .request_shell(true)
        .await
        .map_err(|e| format!("启动 shell 失败: {e}"))?;

    let shell_id = Uuid::new_v4().to_string();
    let (tx, mut rx) = mpsc::unbounded_channel::<ShellCtl>();
    session.shells.lock().insert(shell_id.clone(), tx);

    let sid = session_id.clone();
    let shid = shell_id.clone();
    let app2 = app.clone();
    let session2 = session.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = channel.wait() => match msg {
                    Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                        let _ = app2.emit("shell-data", ShellDataPayload {
                            session_id: sid.clone(),
                            shell_id: shid.clone(),
                            data: B64.encode(&data[..]),
                        });
                    }
                    Some(ChannelMsg::ExitStatus { .. }) | Some(ChannelMsg::Close) | None => break,
                    _ => {}
                },
                ctl = rx.recv() => match ctl {
                    Some(ShellCtl::Data(d)) => {
                        if channel.data(&d[..]).await.is_err() { break; }
                    }
                    Some(ShellCtl::Resize(c, r)) => {
                        let _ = channel.window_change(c, r, 0, 0).await;
                    }
                    Some(ShellCtl::Close) | None => {
                        let _ = channel.close().await;
                        break;
                    }
                },
            }
        }
        session2.shells.lock().remove(&shid);
        let _ = app2.emit(
            "shell-exit",
            ShellExitPayload {
                session_id: sid,
                shell_id: shid,
            },
        );
    });

    Ok(shell_id)
}

#[tauri::command]
pub fn write_shell(
    state: State<'_, AppState>,
    session_id: String,
    shell_id: String,
    data: String, // base64
) -> Result<(), String> {
    let session = get_session(&state, &session_id)?;
    let bytes = B64.decode(&data).map_err(|e| e.to_string())?;
    let shells = session.shells.lock();
    let tx = shells
        .get(&shell_id)
        .ok_or_else(|| format!("shell 不存在: {shell_id}"))?;
    tx.send(ShellCtl::Data(bytes)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn resize_shell(
    state: State<'_, AppState>,
    session_id: String,
    shell_id: String,
    cols: u32,
    rows: u32,
) -> Result<(), String> {
    let session = get_session(&state, &session_id)?;
    let shells = session.shells.lock();
    if let Some(tx) = shells.get(&shell_id) {
        let _ = tx.send(ShellCtl::Resize(cols, rows));
    }
    Ok(())
}

#[tauri::command]
pub fn close_shell(
    state: State<'_, AppState>,
    session_id: String,
    shell_id: String,
) -> Result<(), String> {
    let session = get_session(&state, &session_id)?;
    let tx = session.shells.lock().remove(&shell_id);
    if let Some(tx) = tx {
        let _ = tx.send(ShellCtl::Close);
    }
    Ok(())
}

/// 执行一条命令并收集全部输出 (监控采集用)
pub async fn exec_collect(
    handle: &tokio::sync::Mutex<Handle<Client>>,
    command: &str,
) -> Result<String, String> {
    let mut channel = {
        let h = handle.lock().await;
        h.channel_open_session()
            .await
            .map_err(|e| format!("打开通道失败: {e}"))?
    };
    channel
        .exec(true, command)
        .await
        .map_err(|e| format!("执行命令失败: {e}"))?;
    let mut out = Vec::new();
    while let Some(msg) = channel.wait().await {
        match msg {
            ChannelMsg::Data { data } => out.extend_from_slice(&data),
            ChannelMsg::Eof | ChannelMsg::Close => break,
            _ => {}
        }
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

#[tauri::command]
pub async fn exec_command(
    state: State<'_, AppState>,
    session_id: String,
    command: String,
) -> Result<String, String> {
    let session = get_session(&state, &session_id)?;
    exec_collect(&session.handle, &command).await
}

// 让未使用的导入在某些构建配置下不报警
#[allow(unused)]
fn _unused(_: ChannelId, _: ChannelOpenFailure, _: CryptoVec, _: AppHandle) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::known_hosts::KeyRejection;
    use russh::client;
    use std::collections::HashMap;

    fn test_host() -> HostConfig {
        HostConfig {
            id: "h1".into(),
            name: "测试".into(),
            group: String::new(),
            host: "example.com".into(),
            port: 2222,
            username: "root".into(),
            auth: AuthMethod::Password {
                password: String::new(),
            },
            forwards: Vec::new(),
        }
    }

    /// serde tag 契约测试: 前端依赖精确的 kind 名与字段名,
    /// 任何改动都会在这里炸出来而不是在运行时
    #[test]
    fn connect_error_serde_contract() {
        let host = test_host();

        let e = rejection_to_connect_error(
            &KeyRejection::Unknown {
                check_id: "cid".into(),
                key_type: "ssh-ed25519".into(),
                fingerprint: "SHA256:abc".into(),
            },
            &host,
        );
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "unknown_host_key");
        assert_eq!(v["check_id"], "cid");
        assert_eq!(v["host_id"], "h1");
        assert_eq!(v["port"], 2222);

        let e = rejection_to_connect_error(
            &KeyRejection::Mismatch {
                key_type: "ssh-ed25519".into(),
                fingerprint: "SHA256:new".into(),
                stored: vec![StoredKeyInfo {
                    key_type: "ssh-rsa".into(),
                    fingerprint: "SHA256:old".into(),
                }],
            },
            &host,
        );
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "host_key_mismatch");
        assert_eq!(v["stored"][0]["fingerprint"], "SHA256:old");

        let e = rejection_to_connect_error(
            &KeyRejection::Revoked {
                key_type: "ssh-ed25519".into(),
                fingerprint: "SHA256:rev".into(),
            },
            &host,
        );
        assert_eq!(
            serde_json::to_value(&e).unwrap()["kind"],
            "host_key_revoked"
        );

        let e = rejection_to_connect_error(&KeyRejection::UnsupportedCertAuthority, &host);
        assert_eq!(
            serde_json::to_value(&e).unwrap()["kind"],
            "unsupported_cert_authority"
        );

        let e = rejection_to_connect_error(&KeyRejection::Internal("boom".into()), &host);
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "other");
        assert!(v["message"].as_str().unwrap().contains("boom"));
    }

    // ── TOFU 端到端: 真实 russh 服务器对握 ──

    /// 测试专用一次性私钥 (公开测试数据, 非任何真实主机)
    const TEST_SERVER_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW\nQyNTUxOQAAACAL+y5Fr2AqmfAjm4YL7mM0Qj2aioKdr+TMuszzpe41FwAAAJBh9jRdYfY0\nXQAAAAtzc2gtZWQyNTUxOQAAACAL+y5Fr2AqmfAjm4YL7mM0Qj2aioKdr+TMuszzpe41Fw\nAAAEDb/IhWDNA8gMjV0l35Nz5/mybwaeV4Z2qN5t2HNQlWywv7LkWvYCqZ8CObhgvuYzRC\nPZqKgp2v5My6zPOl7jUXAAAADHRlc3QtZWQyNTUxOQE=\n-----END OPENSSH PRIVATE KEY-----\n";

    struct TestServer;

    impl russh::server::Handler for TestServer {
        type Error = russh::Error;

        async fn auth_password(
            &mut self,
            user: &str,
            password: &str,
        ) -> Result<russh::server::Auth, Self::Error> {
            if user == "u" && password == "p" {
                Ok(russh::server::Auth::Accept)
            } else {
                Ok(russh::server::Auth::reject())
            }
        }
    }

    async fn start_test_server() -> u16 {
        let key = russh::keys::PrivateKey::from_openssh(TEST_SERVER_KEY).unwrap();
        let config = Arc::new(russh::server::Config {
            keys: vec![key],
            ..Default::default()
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            // 服务两次连接 (第一次被拒绝, 第二次放行)
            for _ in 0..2 {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let cfg = config.clone();
                tokio::spawn(async move {
                    let _ = russh::server::run_stream(cfg, stream, TestServer).await;
                });
            }
        });
        port
    }

    fn test_client(
        port: u16,
        pending: &crate::known_hosts::PendingMap,
        trust: Option<Vec<std::path::PathBuf>>,
    ) -> (Client, Arc<Mutex<Option<KeyRejection>>>) {
        let key_rejection = Arc::new(Mutex::new(None));
        (
            Client {
                remote_forwards: Arc::new(Mutex::new(HashMap::new())),
                host_id: "t1".into(),
                host: "127.0.0.1".into(),
                port,
                key_rejection: key_rejection.clone(),
                pending_host_keys: pending.clone(),
                trust_files: trust,
            },
            key_rejection,
        )
    }

    /// 完整 TOFU 流程: 未知 key → 拒绝且不能认证 → 用户批准 →
    /// 重连查验通过 → 密码认证成功。这是 P0 的连接路径证明。
    #[tokio::test]
    async fn tofu_unknown_reject_then_accept_then_authenticate() {
        let port = start_test_server().await;
        let pending: crate::known_hosts::PendingMap = Default::default();
        let dir = tempfile::tempdir().unwrap();
        let trust = dir.path().join("known_hosts");

        // ── 第一次: 未知主机密钥 → 握手被中止, 到不了认证 ──
        let (client, rejection) = test_client(port, &pending, Some(vec![trust.clone()]));
        let config = Arc::new(client::Config::default());
        let result = client::connect(config, ("127.0.0.1", port), client).await;
        assert!(result.is_err(), "未知 key 必须中止连接");
        let check_id = match rejection.lock().take() {
            Some(KeyRejection::Unknown {
                check_id,
                fingerprint,
                ..
            }) => {
                assert!(!fingerprint.is_empty());
                check_id
            }
            other => panic!("应记录 Unknown 拒绝: {other:?}"),
        };

        // ── 用户批准 → 持久化到信任文件 ──
        crate::known_hosts::accept_pending(&pending, &check_id, &trust).unwrap();

        // ── 第二次: 查验通过 → 密码认证成功 ──
        let (client, rejection) = test_client(port, &pending, Some(vec![trust]));
        let config = Arc::new(client::Config::default());
        let mut handle = client::connect(config, ("127.0.0.1", port), client)
            .await
            .expect("信任后应能完成握手");
        assert!(rejection.lock().is_none());
        let auth = handle.authenticate_password("u", "p").await.unwrap();
        assert!(auth.success(), "认证应成功");
    }

    /// 批准 key A 后服务器换 key B → mismatch 硬失败, 不能认证
    #[tokio::test]
    async fn changed_key_after_trust_is_hard_mismatch() {
        let port = start_test_server().await;
        let pending: crate::known_hosts::PendingMap = Default::default();
        let dir = tempfile::tempdir().unwrap();
        let trust = dir.path().join("known_hosts");

        // 信任一把与服务器实际不同的 key (用 fixture 的 RSA key 冒充)
        let wrong_key = russh::keys::parse_public_key_base64(
            "AAAAB3NzaC1yc2EAAAADAQABAAABgQC/CIkggOIIthWLQ9l682i6kVsLX27pp5qo4mMfHI+AMhMzjpRRyUY8tzFkWi2RJH5IeduvGCk+bOq1Ae+J9LCDMkmodsyBJ5eoX8jZhbPCkF9CeftiAG0gUhMhf208dR5dDXckGJYbWlOYolEFimGtHj3BnDX9pE7z5o2M2S3bU26dMMrVwu0GAO3mgdocqepb5Cx1yPFrhxFQpRVgOCR05ctTm8BPaQmW920wQIhFmi7UUbjrGAFBy42H4rAxLDLoicQDIBnhecV3DRzuQbEsCMpnAETuWPYotXdE3MdT31YttnCwv7y+g4gknRiTXxuR0sZl2JJBgJAVTLd4sUuRo4Wtk1pndtjnQP9+Zz6a42BH0pXgfvFBlX/CjrfRd6EUTmQpRhzuS4vu451Tz4ops++Jj93pg1vTAE9kpSzynvI6xm9Fbj/GF8TUIT5TeiTgnzuSwljW9VRzT7OxSmcGSWX1nHjjirCOXH+ShlXAmqamx3PnYGjhRo/s+JjU2jM=",
        )
        .unwrap();
        let entry = crate::known_hosts::PendingHostKey::new(
            "t1".into(),
            "127.0.0.1".into(),
            port,
            &wrong_key,
        )
        .unwrap();
        let check_id = crate::known_hosts::register_pending(&pending, entry);
        crate::known_hosts::accept_pending(&pending, &check_id, &trust).unwrap();

        let (client, rejection) = test_client(port, &pending, Some(vec![trust]));
        let config = Arc::new(client::Config::default());
        let result = client::connect(config, ("127.0.0.1", port), client).await;
        assert!(result.is_err(), "key 不符必须中止连接");
        let outcome = rejection.lock().take();
        match outcome {
            Some(KeyRejection::Mismatch { stored, .. }) => {
                assert_eq!(stored.len(), 1, "应携带已记录指纹");
            }
            other => panic!("应记录 Mismatch 拒绝: {other:?}"),
        }
    }
}
