import { reactive } from "vue";
import { api, type HostConfig, type SessionInfo } from "./api";

export interface TermTab {
  shellId: string;
  sessionId: string;
  title: string;
  alive: boolean;
}

export type SidePanelKind = "sftp" | "monitor" | "forward" | "ai" | null;

export const store = reactive({
  hosts: [] as HostConfig[],
  sessions: new Map<string, SessionInfo>(),
  tabs: [] as TermTab[],
  activeTab: "" as string,
  sidePanel: "sftp" as SidePanelKind,
  connecting: new Set<string>(),
  commandBarOpen: false,
  /** 递增触发终端重新聚焦 */
  focusTick: 0,
  /** AI 设置对话框 */
  aiSettingsOpen: false,
  /** 命令生成条快捷键 (启动时从 ai.json 加载) */
  aiHotkey: "meta+shift+k",

  /** 当前激活终端对应的 sessionId */
  activeSessionId(): string | null {
    const tab = this.tabs.find((t) => t.shellId === this.activeTab);
    return tab ? tab.sessionId : null;
  },

  async refreshHosts() {
    this.hosts = await api.listHosts();
  },

  async connect(host: HostConfig) {
    if (this.connecting.has(host.id)) return;
    this.connecting.add(host.id);
    try {
      const info = await api.connect(host.id);
      this.sessions.set(info.session_id, info);
      const shellId = await api.openShell(info.session_id, 120, 32);
      this.tabs.push({
        shellId,
        sessionId: info.session_id,
        title: host.name || host.host,
        alive: true,
      });
      this.activeTab = shellId;
    } finally {
      this.connecting.delete(host.id);
    }
  },

  async closeTab(shellId: string) {
    const idx = this.tabs.findIndex((t) => t.shellId === shellId);
    if (idx < 0) return;
    const tab = this.tabs[idx];
    await api.closeShell(tab.sessionId, shellId).catch(() => {});
    this.tabs.splice(idx, 1);

    // 会话无剩余 shell 时断开
    if (!this.tabs.some((t) => t.sessionId === tab.sessionId)) {
      await api.disconnect(tab.sessionId).catch(() => {});
      this.sessions.delete(tab.sessionId);
    }
    if (this.activeTab === shellId) {
      this.activeTab = this.tabs[Math.min(idx, this.tabs.length - 1)]?.shellId ?? "";
    }
  },

  onShellExit(shellId: string) {
    const tab = this.tabs.find((t) => t.shellId === shellId);
    if (tab) tab.alive = false;
  },
});
