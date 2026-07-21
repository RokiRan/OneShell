use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;

mod ai;
mod forward;
mod models;
mod monitor;
mod sftp;
mod ssh;
mod store;

pub struct AppState {
    pub store: store::Store,
    pub sessions: Mutex<HashMap<String, Arc<ssh::SshSession>>>,
    pub forwards: Mutex<HashMap<String, forward::ForwardEntry>>,
    /// request_id -> 进行中 AI 任务的中止句柄
    pub ai_requests: Mutex<HashMap<String, tokio::task::AbortHandle>>,
}

#[tauri::command]
fn list_hosts(state: tauri::State<'_, AppState>) -> Vec<models::HostConfig> {
    state.store.list()
}

#[tauri::command]
fn save_host(state: tauri::State<'_, AppState>, host: models::HostConfig) -> Result<(), String> {
    state.store.save(host)
}

#[tauri::command]
fn delete_host(state: tauri::State<'_, AppState>, host_id: String) -> Result<(), String> {
    state.store.delete(&host_id)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let store = store::Store::load().expect("加载主机配置失败");
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            store,
            sessions: Mutex::new(HashMap::new()),
            forwards: Mutex::new(HashMap::new()),
            ai_requests: Mutex::new(HashMap::new()),
        })
        .invoke_handler(tauri::generate_handler![
            list_hosts,
            save_host,
            delete_host,
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
