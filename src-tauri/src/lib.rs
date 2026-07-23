use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;

mod ai;
mod forward;
mod known_hosts;
mod models;
mod monitor;
mod secret;
mod sftp;
mod socks5;
mod ssh;
mod store;

pub struct AppState {
    pub store: Arc<store::Store>,
    pub sessions: Mutex<HashMap<String, Arc<ssh::SshSession>>>,
    pub forwards: Mutex<HashMap<String, forward::ForwardEntry>>,
    /// request_id -> 进行中 AI 任务的中止句柄
    pub ai_requests: Mutex<HashMap<String, tokio::task::AbortHandle>>,
    /// check_id -> 待用户确认的主机密钥 (TOFU)
    pub pending_host_keys: known_hosts::PendingMap,
}

#[tauri::command]
fn list_hosts(state: tauri::State<'_, AppState>) -> Vec<models::HostConfig> {
    state.store.list()
}

#[tauri::command]
async fn save_host(
    state: tauri::State<'_, AppState>,
    host: models::HostConfig,
) -> Result<(), String> {
    // 内含钥匙串阻塞 IO, 移出运行时工作线程
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || store.save(host))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn delete_host(state: tauri::State<'_, AppState>, host_id: String) -> Result<(), String> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || store.delete(&host_id))
        .await
        .map_err(|e| e.to_string())?
}

/// 凭据迁移是否待完成 (旧版明文凭据未能迁入钥匙串)
#[tauri::command]
fn credential_migration_pending(state: tauri::State<'_, AppState>) -> bool {
    state.store.migration_pending()
}

/// 用户在前端点击重试迁移 (例如解锁/安装钥匙串后)
#[tauri::command]
async fn retry_credential_migration(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || store.migrate_legacy())
        .await
        .map_err(|e| e.to_string())?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let store = Arc::new(store::Store::load().expect("加载主机配置失败"));
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            store,
            sessions: Mutex::new(HashMap::new()),
            forwards: Mutex::new(HashMap::new()),
            ai_requests: Mutex::new(HashMap::new()),
            pending_host_keys: known_hosts::PendingMap::default(),
        })
        .invoke_handler(tauri::generate_handler![
            list_hosts,
            save_host,
            delete_host,
            credential_migration_pending,
            retry_credential_migration,
            ssh::connect,
            ssh::disconnect,
            ssh::open_shell,
            ssh::write_shell,
            ssh::resize_shell,
            ssh::close_shell,
            ssh::exec_command,
            sftp::sftp_home,
            sftp::sftp_list,
            sftp::sftp_mkdir,
            sftp::sftp_remove,
            sftp::sftp_rename,
            sftp::sftp_upload,
            sftp::sftp_download,
            forward::start_forward,
            forward::stop_forward,
            forward::list_forwards,
            monitor::server_stats,
            ai::get_ai_config,
            ai::save_ai_config,
            ai::ai_chat,
            ai::ai_cancel,
            known_hosts::accept_host_key,
            known_hosts::dismiss_host_key,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
