use std::sync::Arc;

use russh_sftp::client::SftpSession;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use crate::{
    models::FileEntry,
    ssh::{get_session, SshSession},
    AppState,
};

async fn open_sftp(session: &Arc<SshSession>) -> Result<SftpSession, String> {
    let channel = {
        let handle = session.handle.lock().await;
        handle
            .channel_open_session()
            .await
            .map_err(|e| format!("打开通道失败: {e}"))?
    };
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| format!("启动 SFTP 子系统失败: {e}"))?;
    SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| format!("SFTP 握手失败: {e}"))
}

#[tauri::command]
pub async fn sftp_home(state: State<'_, AppState>, session_id: String) -> Result<String, String> {
    let session = get_session(&state, &session_id)?;
    let sftp = open_sftp(&session).await?;
    sftp.canonicalize(".")
        .await
        .map_err(|e| format!("获取主目录失败: {e}"))
}

#[tauri::command]
pub async fn sftp_list(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
) -> Result<Vec<FileEntry>, String> {
    let session = get_session(&state, &session_id)?;
    let sftp = open_sftp(&session).await?;
    let dir = sftp
        .read_dir(&path)
        .await
        .map_err(|e| format!("读取目录失败: {e}"))?;
    let base = path.trim_end_matches('/');
    let mut entries: Vec<FileEntry> = dir
        .map(|entry| {
            let meta = entry.metadata();
            let name = entry.file_name();
            FileEntry {
                path: format!("{base}/{name}"),
                is_dir: meta.is_dir(),
                is_symlink: meta.file_type().is_symlink(),
                size: meta.size.unwrap_or(0),
                mtime: meta.mtime.unwrap_or(0) as i64,
                permissions: meta.permissions.unwrap_or(0),
                name,
            }
        })
        .collect();
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
    Ok(entries)
}

#[tauri::command]
pub async fn sftp_mkdir(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
) -> Result<(), String> {
    let session = get_session(&state, &session_id)?;
    let sftp = open_sftp(&session).await?;
    sftp.create_dir(&path)
        .await
        .map_err(|e| format!("创建目录失败: {e}"))
}

#[tauri::command]
pub async fn sftp_remove(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
    is_dir: bool,
) -> Result<(), String> {
    let session = get_session(&state, &session_id)?;
    let sftp = open_sftp(&session).await?;
    if is_dir {
        sftp.remove_dir(&path).await
    } else {
        sftp.remove_file(&path).await
    }
    .map_err(|e| format!("删除失败: {e}"))
}

#[tauri::command]
pub async fn sftp_rename(
    state: State<'_, AppState>,
    session_id: String,
    old_path: String,
    new_path: String,
) -> Result<(), String> {
    let session = get_session(&state, &session_id)?;
    let sftp = open_sftp(&session).await?;
    sftp.rename(&old_path, &new_path)
        .await
        .map_err(|e| format!("重命名失败: {e}"))
}

#[derive(Serialize, Clone)]
struct TransferProgress {
    op_id: String,
    kind: String, // "upload" | "download"
    transferred: u64,
    total: u64,
    done: bool,
    error: Option<String>,
}

/// 上传本地文件到远端 (流式, 带进度事件)
#[tauri::command]
pub async fn sftp_upload(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    local_path: String,
    remote_path: String,
) -> Result<(), String> {
    let session = get_session(&state, &session_id)?;
    let op_id = Uuid::new_v4().to_string();
    tokio::spawn(async move {
        let result = transfer(&app, &op_id, "upload", &session, &local_path, &remote_path).await;
        if let Err(e) = result {
            let _ = app.emit(
                "sftp-progress",
                TransferProgress {
                    op_id,
                    kind: "upload".into(),
                    transferred: 0,
                    total: 0,
                    done: true,
                    error: Some(e),
                },
            );
        }
    });
    Ok(())
}

/// 下载远端文件到本地
#[tauri::command]
pub async fn sftp_download(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    remote_path: String,
    local_path: String,
) -> Result<(), String> {
    let session = get_session(&state, &session_id)?;
    let op_id = Uuid::new_v4().to_string();
    tokio::spawn(async move {
        let result = transfer(
            &app,
            &op_id,
            "download",
            &session,
            &local_path,
            &remote_path,
        )
        .await;
        if let Err(e) = result {
            let _ = app.emit(
                "sftp-progress",
                TransferProgress {
                    op_id,
                    kind: "download".into(),
                    transferred: 0,
                    total: 0,
                    done: true,
                    error: Some(e),
                },
            );
        }
    });
    Ok(())
}

async fn transfer(
    app: &AppHandle,
    op_id: &str,
    kind: &str,
    session: &Arc<SshSession>,
    local_path: &str,
    remote_path: &str,
) -> Result<(), String> {
    let sftp = open_sftp(session).await?;
    if kind == "upload" {
        let total = tokio::fs::metadata(local_path)
            .await
            .map_err(|e| e.to_string())?
            .len();
        let reader = tokio::fs::File::open(local_path)
            .await
            .map_err(|e| e.to_string())?;
        let writer = sftp.create(remote_path).await.map_err(|e| e.to_string())?;
        pump(app, op_id, kind, reader, writer, total).await
    } else {
        let total = sftp
            .metadata(remote_path)
            .await
            .map_err(|e| e.to_string())?
            .size
            .unwrap_or(0);
        let reader = sftp.open(remote_path).await.map_err(|e| e.to_string())?;
        let writer = tokio::fs::File::create(local_path)
            .await
            .map_err(|e| e.to_string())?;
        pump(app, op_id, kind, reader, writer, total).await
    }
}

async fn pump<R, W>(
    app: &AppHandle,
    op_id: &str,
    kind: &str,
    mut reader: R,
    mut writer: W,
    total: u64,
) -> Result<(), String>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let mut transferred: u64 = 0;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .await
            .map_err(|e| format!("读取失败: {e}"))?;
        if n == 0 {
            break;
        }
        writer
            .write_all(&buf[..n])
            .await
            .map_err(|e| format!("写入失败: {e}"))?;
        transferred += n as u64;
        let _ = app.emit(
            "sftp-progress",
            TransferProgress {
                op_id: op_id.to_string(),
                kind: kind.to_string(),
                transferred,
                total,
                done: false,
                error: None,
            },
        );
    }
    writer.shutdown().await.map_err(|e| e.to_string())?;
    let _ = app.emit(
        "sftp-progress",
        TransferProgress {
            op_id: op_id.to_string(),
            kind: kind.to_string(),
            transferred,
            total,
            done: true,
            error: None,
        },
    );
    Ok(())
}
