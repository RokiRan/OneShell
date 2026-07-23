use std::sync::Arc;

use parking_lot::Mutex;
use tauri::{AppHandle, Emitter, State};
use tokio::net::TcpListener;
use tokio::task::AbortHandle;

use crate::{
    models::{ForwardRule, ForwardStatus},
    ssh::{get_session, pipe_channel_tcp},
    AppState,
};

pub enum ForwardEntry {
    /// 本地转发: 中止监听任务即停止
    Local(AbortHandle),
    /// 动态转发 (SOCKS5): 监听任务 + 已建立连接的子任务句柄;
    /// 停止时两者都中止 (子任务不在监听任务退出时自动结束)
    Dynamic {
        listener: AbortHandle,
        children: Arc<Mutex<Vec<AbortHandle>>>,
    },
    /// 远端转发: 停止时调用 cancel_tcpip_forward
    Remote { bind_host: String, bind_port: u32 },
}

fn status_event(
    app: &AppHandle,
    key: &str,
    session_id: &str,
    rule_id: &str,
    active: bool,
    detail: String,
) {
    let _ = app.emit(
        "forward-status",
        ForwardStatus {
            key: key.to_string(),
            session_id: session_id.to_string(),
            rule_id: rule_id.to_string(),
            active,
            detail,
        },
    );
}

#[tauri::command]
pub async fn start_forward(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    rule: ForwardRule,
) -> Result<String, String> {
    let session = get_session(&state, &session_id)?;
    let key = format!("{session_id}:{}", rule.id);

    if state.forwards.lock().contains_key(&key) {
        return Err("该转发已在运行".into());
    }

    if rule.kind == "local" {
        let listener = TcpListener::bind((rule.bind_host.as_str(), rule.bind_port))
            .await
            .map_err(|e| format!("监听 {}:{} 失败: {e}", rule.bind_host, rule.bind_port))?;
        let session2 = session.clone();
        let app2 = app.clone();
        let key2 = key.clone();
        let sid = session_id.clone();
        let rid = rule.id.clone();
        let target_host = rule.target_host.clone();
        let target_port = rule.target_port;
        let task = tokio::spawn(async move {
            loop {
                let Ok((tcp, peer)) = listener.accept().await else {
                    break;
                };
                let session3 = session2.clone();
                let th = target_host.clone();
                tokio::spawn(async move {
                    let channel = {
                        let handle = session3.handle.lock().await;
                        handle
                            .channel_open_direct_tcpip(
                                th,
                                target_port as u32,
                                peer.ip().to_string(),
                                peer.port() as u32,
                            )
                            .await
                    };
                    match channel {
                        Ok(ch) => pipe_channel_tcp(ch, tcp).await,
                        Err(_) => drop(tcp),
                    }
                });
            }
            status_event(&app2, &key2, &sid, &rid, false, "监听任务结束".into());
        });
        state
            .forwards
            .lock()
            .insert(key.clone(), ForwardEntry::Local(task.abort_handle()));
        status_event(
            &app,
            &key,
            &session_id,
            &rule.id,
            true,
            format!(
                "{}:{} -> {}:{}",
                rule.bind_host, rule.bind_port, rule.target_host, rule.target_port
            ),
        );
    } else if rule.kind == "dynamic" {
        // 动态转发 (SOCKS5): 只允许环回监听 — SOCKS 无认证, 绑定到非环回
        // 地址等于把 SSH 出网能力开放给整个网段 (开放代理)
        let bindable = rule.bind_host == "localhost"
            || rule
                .bind_host
                .parse::<std::net::IpAddr>()
                .map(|ip| ip.is_loopback())
                .unwrap_or(false);
        if !bindable {
            return Err(format!(
                "动态转发 (SOCKS5) 无认证, 只允许环回监听 (127.0.0.1 / ::1), 当前: {}",
                rule.bind_host
            ));
        }
        // 本地监听, 目标由客户端 CONNECT 请求决定
        let listener = TcpListener::bind((rule.bind_host.as_str(), rule.bind_port))
            .await
            .map_err(|e| format!("监听 {}:{} 失败: {e}", rule.bind_host, rule.bind_port))?;
        let session2 = session.clone();
        let app2 = app.clone();
        let key2 = key.clone();
        let sid = session_id.clone();
        let rid = rule.id.clone();
        let children: Arc<Mutex<Vec<AbortHandle>>> = Arc::new(Mutex::new(Vec::new()));
        let children2 = children.clone();
        // 并发连接上限: 防 fd 耗尽 (浏览器开 SOCKS 会同时打几十条)
        let semaphore = Arc::new(tokio::sync::Semaphore::new(128));
        let task = tokio::spawn(async move {
            loop {
                let Ok((tcp, _peer)) = listener.accept().await else {
                    break;
                };
                let Ok(permit) = semaphore.clone().try_acquire_owned() else {
                    drop(tcp); // 超限直接拒连
                    continue;
                };
                let session3 = session2.clone();
                let child = tokio::spawn(async move {
                    let _permit = permit; // 持有至连接结束
                    crate::socks5::handle_client(tcp, session3).await;
                });
                let mut guard = children2.lock();
                guard.retain(|h| !h.is_finished()); // 顺手清理已结束的
                guard.push(child.abort_handle());
            }
            status_event(&app2, &key2, &sid, &rid, false, "监听任务结束".into());
        });
        state.forwards.lock().insert(
            key.clone(),
            ForwardEntry::Dynamic {
                listener: task.abort_handle(),
                children,
            },
        );
        status_event(
            &app,
            &key,
            &session_id,
            &rule.id,
            true,
            format!("SOCKS5 {}:{}", rule.bind_host, rule.bind_port),
        );
    } else if rule.kind == "remote" {
        // 远端转发: 注册端口映射, 请求远端监听
        session.remote_forwards.lock().insert(
            rule.bind_port as u32,
            (rule.target_host.clone(), rule.target_port as u32),
        );
        let result = {
            let handle = session.handle.lock().await;
            handle
                .tcpip_forward(rule.bind_host.clone(), rule.bind_port as u32)
                .await
        };
        if let Err(e) = result {
            session
                .remote_forwards
                .lock()
                .remove(&(rule.bind_port as u32));
            return Err(format!("远端转发请求失败: {e}"));
        }
        state.forwards.lock().insert(
            key.clone(),
            ForwardEntry::Remote {
                bind_host: rule.bind_host.clone(),
                bind_port: rule.bind_port as u32,
            },
        );
        status_event(
            &app,
            &key,
            &session_id,
            &rule.id,
            true,
            format!(
                "远端 {}:{} -> 本地 {}:{}",
                rule.bind_host, rule.bind_port, rule.target_host, rule.target_port
            ),
        );
    } else {
        return Err(format!("不支持的转发类型: {}", rule.kind));
    }
    Ok(key)
}

/// stop_forward 的 Dynamic 分支: 中止监听 + 全部已建立连接
fn abort_dynamic(listener: &AbortHandle, children: &Mutex<Vec<AbortHandle>>) {
    listener.abort();
    for child in children.lock().iter() {
        child.abort();
    }
}

#[tauri::command]
pub async fn stop_forward(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    rule_id: String,
) -> Result<(), String> {
    let key = format!("{session_id}:{rule_id}");
    let entry = state.forwards.lock().remove(&key);
    let Some(entry) = entry else {
        return Err("转发未在运行".into());
    };
    match entry {
        ForwardEntry::Local(abort) => abort.abort(),
        ForwardEntry::Dynamic { listener, children } => abort_dynamic(&listener, &children),
        ForwardEntry::Remote {
            bind_host,
            bind_port,
        } => {
            if let Ok(session) = get_session(&state, &session_id) {
                let handle = session.handle.lock().await;
                let _ = handle
                    .cancel_tcpip_forward(bind_host, bind_port as u32)
                    .await;
                session.remote_forwards.lock().remove(&(bind_port as u32));
            }
        }
    }
    status_event(&app, &key, &session_id, &rule_id, false, "已停止".into());
    Ok(())
}

#[tauri::command]
pub fn list_forwards(state: State<'_, AppState>, session_id: String) -> Vec<String> {
    state
        .forwards
        .lock()
        .keys()
        .filter(|k| k.starts_with(&format!("{session_id}:")))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// stop_forward 的 Dynamic 分支语义: 监听与全部子连接都被中止
    #[tokio::test]
    async fn abort_dynamic_kills_listener_and_children() {
        let children: Arc<Mutex<Vec<AbortHandle>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::spawn(std::future::pending::<()>());
        let child = tokio::spawn(std::future::pending::<()>());
        children.lock().push(child.abort_handle());

        abort_dynamic(&listener.abort_handle(), &children);

        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(3), listener)
                .await
                .unwrap()
                .unwrap_err()
                .is_cancelled(),
            "监听任务应被中止"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(3), child)
                .await
                .unwrap()
                .unwrap_err()
                .is_cancelled(),
            "子连接任务应被中止"
        );
    }
}
