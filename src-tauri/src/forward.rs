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
    /// 远端转发: 停止时调用 cancel_tcpip_forward
    Remote { bind_host: String, bind_port: u32 },
}

fn status_event(app: &AppHandle, key: &str, session_id: &str, rule_id: &str, active: bool, detail: String) {
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
    } else {
        // 远端转发: 注册端口映射, 请求远端监听
        session
            .remote_forwards
            .lock()
            .insert(rule.bind_port as u32, (rule.target_host.clone(), rule.target_port as u32));
        let result = {
            let handle = session.handle.lock().await;
            handle
                .tcpip_forward(rule.bind_host.clone(), rule.bind_port as u32)
                .await
        };
        if let Err(e) = result {
            session.remote_forwards.lock().remove(&(rule.bind_port as u32));
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
    }
    Ok(key)
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
        ForwardEntry::Remote { bind_host, bind_port } => {
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
