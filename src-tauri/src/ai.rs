use std::{fs, path::PathBuf};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::AppState;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AiConfig {
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    /// 终端内唤起命令生成条的快捷键, 默认 "ctrl+shift+k" / macOS "meta+shift+k"
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
    /// 命令非零退出时自动触发 AI 分析, 默认开启
    #[serde(default = "default_auto_analyze")]
    pub auto_analyze: bool,
}

fn default_auto_analyze() -> bool {
    false
}

fn default_hotkey() -> String {
    if cfg!(target_os = "macos") {
        "meta+shift+k".into()
    } else {
        "ctrl+shift+k".into()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMsg {
    pub role: String,
    pub content: String,
}

#[derive(Serialize, Clone)]
struct AiChunk {
    request_id: String,
    delta: String,
    done: bool,
    error: Option<String>,
}

fn config_path() -> Result<PathBuf, String> {
    let dir = dirs::config_dir()
        .ok_or("无法定位配置目录")?
        .join("oneshell");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("ai.json"))
}

#[tauri::command]
pub fn get_ai_config() -> Result<AiConfig, String> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(AiConfig {
            hotkey: default_hotkey(),
            ..Default::default()
        });
    }
    let raw = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut cfg: AiConfig = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    if cfg.hotkey.is_empty() {
        cfg.hotkey = default_hotkey();
    }
    Ok(cfg)
}

#[tauri::command]
pub fn save_ai_config(config: AiConfig) -> Result<(), String> {
    let path = config_path()?;
    let raw = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    fs::write(&path, raw).map_err(|e| e.to_string())
}

/// 流式 <think>…</think> 过滤器: 思考内容不推给前端。
/// 标签可能跨 chunk, 用缓冲 + 尾部保留处理。
struct ThinkFilter {
    in_think: bool,
    buf: String,
}

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

impl ThinkFilter {
    fn new() -> Self {
        Self {
            in_think: false,
            buf: String::new(),
        }
    }

    fn push(&mut self, s: &str) -> String {
        self.buf.push_str(s);
        let mut out = String::new();
        loop {
            if self.in_think {
                match self.buf.find(CLOSE) {
                    Some(i) => {
                        self.buf.drain(..i + CLOSE.len());
                        self.in_think = false;
                    }
                    None => {
                        // 思考内容丢弃, 只保留可能含半个闭合标签的尾巴。
                        // 切口可能落在多字节字符中间 (中文思考内容), 需回退到字符边界。
                        if self.buf.len() > CLOSE.len() * 2 {
                            let mut cut = self.buf.len() - CLOSE.len();
                            while !self.buf.is_char_boundary(cut) {
                                cut -= 1;
                            }
                            self.buf.drain(..cut);
                        }
                        break;
                    }
                }
            } else {
                match self.buf.find(OPEN) {
                    Some(i) => {
                        out.push_str(&self.buf[..i]);
                        self.buf.drain(..i + OPEN.len());
                        self.in_think = true;
                    }
                    None => {
                        // 保留可能是 "<think>" 前缀的尾巴, 其余输出
                        let keep = (1..=(OPEN.len() - 1).min(self.buf.len()))
                            .rev()
                            .find(|&k| {
                                self.buf
                                    .get(self.buf.len() - k..)
                                    .is_some_and(|suffix| OPEN.starts_with(suffix))
                            })
                            .unwrap_or(0);
                        out.push_str(&self.buf[..self.buf.len() - keep]);
                        self.buf.drain(..self.buf.len() - keep);
                        break;
                    }
                }
            }
        }
        out
    }

    fn finish(self) -> String {
        if self.in_think {
            String::new()
        } else {
            self.buf
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 长段中文思考内容: 尾部保留切口必须落在字符边界, 不得 panic
    #[test]
    fn think_filter_multibyte_boundary() {
        let mut f = ThinkFilter::new();
        assert_eq!(f.push("<think>"), "");
        // 分多次推入大量中文, 触发 in_think 分支的尾部裁剪
        for _ in 0..20 {
            assert_eq!(f.push("正在分析用户的问题"), "");
        }
        assert_eq!(f.push("</think>答案在这里"), "答案在这里");
        assert_eq!(f.finish(), "");
    }

    /// 标签跨 chunk 拆分
    #[test]
    fn think_filter_split_tags() {
        let mut f = ThinkFilter::new();
        assert_eq!(f.push("前面<thi"), "前面");
        assert_eq!(f.push("nk>思考</think>后面"), "后面");
        assert_eq!(f.finish(), "");
    }

    /// 未闭合的 think 块整体丢弃
    #[test]
    fn think_filter_unclosed() {
        let mut f = ThinkFilter::new();
        assert_eq!(f.push("可见<think>隐藏的推理"), "可见");
        assert_eq!(f.finish(), "");
    }
}

/// 发起一次 OpenAI 兼容的流式聊天; 增量通过 "ai-chunk" 事件推送
#[tauri::command]
pub async fn ai_chat(
    app: AppHandle,
    state: State<'_, AppState>,
    request_id: String,
    messages: Vec<ChatMsg>,
) -> Result<(), String> {
    let cfg = get_ai_config()?;
    if cfg.base_url.trim().is_empty() || cfg.api_key.trim().is_empty() || cfg.model.trim().is_empty() {
        return Err("请先在 AI 设置中填写 base_url / api_key / model".into());
    }

    let rid = request_id.clone();
    // 启动闸门: 任务先等注册完成, 避免“任务先清理、主线程后插入”的陈旧句柄竞态
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = gate_rx.await;
        stream_chat(&app, &request_id, &cfg, messages).await;
        // 无论成功/失败都清理取消句柄, 避免错误路径上泄漏陈旧句柄
        app.state::<AppState>()
            .ai_requests
            .lock()
            .remove(&request_id);
    });
    state.ai_requests.lock().insert(rid, task.abort_handle());
    let _ = gate_tx.send(());

    Ok(())
}

/// 实际的流式请求体; 出错时通过 ai-chunk 的 error 字段通知前端后提前返回
async fn stream_chat(app: &AppHandle, request_id: &str, cfg: &AiConfig, messages: Vec<ChatMsg>) {
    let emit_chunk = |delta: String, done: bool, error: Option<String>| {
        let _ = app.emit(
            "ai-chunk",
            AiChunk {
                request_id: request_id.to_string(),
                delta,
                done,
                error,
            },
        );
    };

    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": cfg.model,
        "messages": messages,
        "stream": true,
    });

    let resp = match reqwest::Client::new()
        .post(&url)
        .bearer_auth(&cfg.api_key)
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            emit_chunk(String::new(), true, Some(format!("请求失败: {e}")));
            return;
        }
    };
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let mut end = text.len().min(300);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        emit_chunk(String::new(), true, Some(format!("HTTP {status}: {}", &text[..end])));
        return;
    }

    // 解析 SSE: "data: {json}" 行, 以 "data: [DONE]" 结束。
    // 按字节缓冲: 多字节字符可能跨 TCP chunk, 凑够整行再解码避免乱码。
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut filter = ThinkFilter::new();
    'outer: while let Some(chunk) = stream.next().await {
        let bytes = match chunk {
            Ok(b) => b,
            Err(e) => {
                emit_chunk(String::new(), true, Some(format!("请求流中断: {e}")));
                return;
            }
        };
        buf.extend_from_slice(&bytes);
        while let Some(idx) = buf.iter().position(|&b| b == b'\n') {
            let line = String::from_utf8_lossy(&buf[..idx]).trim().to_string();
            buf.drain(..=idx);
            let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if data == "[DONE]" {
                break 'outer;
            }
            let Ok(json) = serde_json::from_str::<serde_json::Value>(data) else {
                continue;
            };
            // 独立 reasoning 字段 (reasoning_content 等) 一律忽略, 只取 content
            let delta = json["choices"][0]["delta"]["content"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let delta = filter.push(&delta);
            if !delta.is_empty() {
                emit_chunk(delta, false, None);
            }
        }
    }
    // 冲刷过滤器尾部 (think 未闭合则丢弃)
    let tail = filter.finish();
    if !tail.is_empty() {
        emit_chunk(tail, false, None);
    }
    emit_chunk(String::new(), true, None);
}

/// 取消进行中的 AI 请求
#[tauri::command]
pub fn ai_cancel(app: AppHandle, request_id: String) {
    let handle = app
        .state::<AppState>()
        .ai_requests
        .lock()
        .remove(&request_id);
    if let Some(handle) = handle {
        handle.abort();
    }
    // 无论任务是否存在, 都补一个 done 让前端退出加载态
    let _ = app.emit(
        "ai-chunk",
        AiChunk {
            request_id,
            delta: String::new(),
            done: true,
            error: None,
        },
    );
}
