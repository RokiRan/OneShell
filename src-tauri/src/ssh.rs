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
    models::{AuthMethod, HostConfig, SessionInfo},
    AppState,
};

/// 远端端口转发: 绑定端口 -> (目标主机, 目标端口)
pub type RemoteForwardMap = Arc<Mutex<HashMap<u32, (String, u32)>>>;

pub struct Client {
    pub remote_forwards: RemoteForwardMap,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, _key: &PublicKey) -> Result<bool, Self::Error> {
        // TODO: known_hosts 校验
        Ok(true)
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
        let target = self
            .remote_forwards
            .lock()
            .get(&connected_port)
            .cloned();
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
) -> Result<SessionInfo, String> {
    let host = state.store.get(&host_id)?;
    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(20)),
        ..Default::default()
    });
    let remote_forwards: RemoteForwardMap = Arc::new(Mutex::new(HashMap::new()));
    let handler = Client {
        remote_forwards: remote_forwards.clone(),
    };
    let mut handle =
        tokio::time::timeout(Duration::from_secs(15), async {
            client::connect(config, (host.host.as_str(), host.port), handler).await
        })
        .await
        .map_err(|_| format!("连接超时: {}:{}", host.host, host.port))?
        .map_err(|e| format!("连接失败: {e}"))?;

    authenticate(&mut handle, &host).await?;

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
