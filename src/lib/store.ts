import { reactive } from "vue";
import { api, normalizeConnectError, type ConnectError, type HostConfig, type SessionInfo } from "./api";

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
  /** 右侧面板宽度 (px), 拖动调整并持久化 */
  sidePanelWidth: Number(localStorage.getItem("oneshell:side-panel-width")) || 320,
  /** AI 已配置 (base_url/api_key/model 齐全, 启动时加载) */
  aiConfigured: false,
  /** 非零退出码自动 AI 分析开关 (启动时从 ai.json 加载) */
  aiAutoAnalyze: true,
  /** 右键 "问 AI" 的待预填内容; AiPanel 挂载/监听时消费 */
  aiPendingPrefill: "" as string,
  /** 命令失败自动分析的待处理请求; AiPanel 挂载/监听时消费. followUp=true 表示 AI 跟进自己建议的命令 */
  aiPendingAnalyze: null as { shellId: string; exitCode: number; followUp: boolean } | null,
  /** TOFU 主机密钥确认弹窗 (unknown_host_key); HostKeyDialog 消费 */
  hostKeyPrompt: null as {
    error: Extract<ConnectError, { kind: "unknown_host_key" }>;
    host: HostConfig;
  } | null,
  /** 主机密钥硬告警 (mismatch/revoked/CA); HostKeyDialog 消费 */
  hostKeyAlert: null as Exclude<ConnectError, { kind: "other" }> | null,
  /** 旧版明文凭据待迁入钥匙串 (启动时加载, 横幅 + 重试) */
  migrationPending: false,
  migrationError: "" as string,

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
    // 清掉上一次尝试残留的弹窗状态
    this.hostKeyPrompt = null;
    this.hostKeyAlert = null;
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
    } catch (e) {
      // 主机密钥类错误路由到专用弹窗; 其余原样上抛给调用方展示
      const ce = normalizeConnectError(e);
      if (ce && ce.kind === "unknown_host_key") {
        this.hostKeyPrompt = { error: ce, host };
      } else if (ce && ce.kind !== "other") {
        this.hostKeyAlert = ce;
      } else {
        throw e;
      }
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
