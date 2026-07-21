import { invoke } from "@tauri-apps/api/core";

export type AuthMethod =
  | { kind: "password"; password: string }
  | { kind: "key"; key_path: string; passphrase?: string | null };

export interface ForwardRule {
  id: string;
  kind: "local" | "remote" | string;
  name: string;
  bind_host: string;
  bind_port: number;
  target_host: string;
  target_port: number;
}

export interface HostConfig {
  id: string;
  name: string;
  group: string;
  host: string;
  port: number;
  username: string;
  auth: AuthMethod;
  forwards: ForwardRule[];
}

export interface SessionInfo {
  session_id: string;
  host_id: string;
  label: string;
}

export interface FileEntry {
  name: string;
  path: string;
  is_dir: boolean;
  is_symlink: boolean;
  size: number;
  mtime: number;
  permissions: number;
}

export interface ServerStats {
  hostname: string;
  os: string;
  cpu_percent: number;
  cpu_cores: number;
  mem_total: number;
  mem_used: number;
  swap_total: number;
  swap_used: number;
  disk_total: number;
  disk_used: number;
  uptime_secs: number;
  load1: number;
  load5: number;
  load15: number;
  net_rx_bps: number;
  net_tx_bps: number;
}

export interface ForwardStatus {
  key: string;
  session_id: string;
  rule_id: string;
  active: boolean;
  detail: string;
}

export interface TransferProgress {
  op_id: string;
  kind: "upload" | "download";
  transferred: number;
  total: number;
  done: boolean;
  error: string | null;
}

export interface AiConfig {
  base_url: string;
  api_key: string;
  model: string;
  hotkey: string;
  auto_analyze: boolean;
}

export interface ChatMsg {
  role: "system" | "user" | "assistant";
  content: string;
}

export interface AiChunk {
  request_id: string;
  delta: string;
  done: boolean;
  error: string | null;
}

export const api = {
  listHosts: () => invoke<HostConfig[]>("list_hosts"),
  saveHost: (host: HostConfig) => invoke<void>("save_host", { host }),
  deleteHost: (hostId: string) => invoke<void>("delete_host", { hostId }),

  connect: (hostId: string) => invoke<SessionInfo>("connect", { hostId }),
  disconnect: (sessionId: string) => invoke<void>("disconnect", { sessionId }),

  openShell: (sessionId: string, cols: number, rows: number) =>
    invoke<string>("open_shell", { sessionId, cols, rows }),
  writeShell: (sessionId: string, shellId: string, data: string) =>
    invoke<void>("write_shell", { sessionId, shellId, data }),
  resizeShell: (sessionId: string, shellId: string, cols: number, rows: number) =>
    invoke<void>("resize_shell", { sessionId, shellId, cols, rows }),
  closeShell: (sessionId: string, shellId: string) =>
    invoke<void>("close_shell", { sessionId, shellId }),
  execCommand: (sessionId: string, command: string) =>
    invoke<string>("exec_command", { sessionId, command }),

  sftpHome: (sessionId: string) => invoke<string>("sftp_home", { sessionId }),
  sftpList: (sessionId: string, path: string) =>
    invoke<FileEntry[]>("sftp_list", { sessionId, path }),
  sftpMkdir: (sessionId: string, path: string) =>
    invoke<void>("sftp_mkdir", { sessionId, path }),
  sftpRemove: (sessionId: string, path: string, isDir: boolean) =>
    invoke<void>("sftp_remove", { sessionId, path, isDir }),
  sftpRename: (sessionId: string, oldPath: string, newPath: string) =>
    invoke<void>("sftp_rename", { sessionId, oldPath, newPath }),
  sftpUpload: (sessionId: string, localPath: string, remotePath: string) =>
    invoke<void>("sftp_upload", { sessionId, localPath, remotePath }),
  sftpDownload: (sessionId: string, remotePath: string, localPath: string) =>
    invoke<void>("sftp_download", { sessionId, remotePath, localPath }),

  startForward: (sessionId: string, rule: ForwardRule) =>
    invoke<string>("start_forward", { sessionId, rule }),
  stopForward: (sessionId: string, ruleId: string) =>
    invoke<void>("stop_forward", { sessionId, ruleId }),
  listForwards: (sessionId: string) => invoke<string[]>("list_forwards", { sessionId }),

  serverStats: (sessionId: string) => invoke<ServerStats>("server_stats", { sessionId }),

  getAiConfig: () => invoke<AiConfig>("get_ai_config"),
  saveAiConfig: (config: AiConfig) => invoke<void>("save_ai_config", { config }),
  aiChat: (requestId: string, messages: ChatMsg[]) =>
    invoke<void>("ai_chat", { requestId, messages }),
  aiCancel: (requestId: string) => invoke<void>("ai_cancel", { requestId }),
};

export function b64encode(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(s);
}

export function b64decode(b64: string): Uint8Array {
  const s = atob(b64);
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n;
  let u = -1;
  do {
    v /= 1024;
    u++;
  } while (v >= 1024 && u < units.length - 1);
  return `${v.toFixed(1)} ${units[u]}`;
}

export function formatUptime(secs: number): string {
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d > 0) return `${d}天${h}时`;
  if (h > 0) return `${h}时${m}分`;
  return `${m}分`;
}
