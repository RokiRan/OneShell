<script setup lang="ts">
import { onMounted, onBeforeUnmount, ref, watch } from "vue";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import "@xterm/xterm/css/xterm.css";
import { api, b64decode, b64encode } from "@/lib/api";
import { store, type TermTab } from "@/lib/store";
import {
  appendTermData,
  dropTermContext,
  getTermMeta,
  setTermMeta,
  takeAiCommand,
} from "@/lib/term-context";
import { dropConversation } from "@/lib/ai-conversations";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";

const props = defineProps<{ tab: TermTab; visible: boolean }>();

/**
 * 注入 bash/zsh 钩子, 每次提示符刷新时通过不可见的 OSC 5151 上报:
 *   5151;<退出码>;<cwd 的 base64>;<最后命令的 base64>
 * base64 避免命令里的分号/换行破坏序列结构。return 保留原退出码, 不污染 $?。
 */
const SHELL_INTEGRATION =
  '__os_precmd(){ ec=$?;if [ -n "$ZSH_VERSION" ];then cmd=$__os_last_cmd;else cmd=$(HISTTIMEFORMAT= history 1 | sed "s/^ *[0-9]* *//");fi;b64=$(printf %s "$cmd" | base64 | tr -d "\\n");p64=$(printf %s "$PWD" | base64 | tr -d "\\n");printf \'\\033]5151;%s;%s;%s\\007\' "$ec" "$p64" "$b64";return $ec;};if [ -n "$ZSH_VERSION" ];then __os_preexec(){ __os_last_cmd="$1";};preexec_functions+=(__os_preexec);precmd_functions+=(__os_precmd);else PROMPT_COMMAND="__os_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}";fi\n';

/** 自动分析节流: 同一 shell 30 秒内最多触发一次 */
const lastAutoAnalyze = new Map<string, number>();

/** 解析 "meta+shift+k" 形式的快捷键配置并匹配键盘事件 */
function matchHotkey(e: KeyboardEvent, hotkey: string): boolean {
  const parts = hotkey.toLowerCase().split("+").map((s) => s.trim());
  const key = parts[parts.length - 1];
  const mods = new Set(parts.slice(0, -1));
  return (
    e.key.toLowerCase() === key &&
    e.metaKey === mods.has("meta") &&
    e.ctrlKey === mods.has("ctrl") &&
    e.shiftKey === mods.has("shift") &&
    e.altKey === mods.has("alt")
  );
}

const container = ref<HTMLDivElement>();
const hasSelection = ref(false);
/** 命令失败时的轻量提示气泡 (非自动模式) */
const showChip = ref(false);
const chipExitCode = ref(0);
let term: Terminal | null = null;
let fit: FitAddon | null = null;
let unlisten: UnlistenFn | null = null;
let oscDisposable: { dispose(): void } | null = null;
let resizeObserver: ResizeObserver | null = null;

function triggerAnalyze(code: number, followUp = false) {
  store.activeTab = props.tab.shellId;
  store.sidePanel = "ai";
  store.aiPendingAnalyze = { shellId: props.tab.shellId, exitCode: code, followUp };
}

function onNonZeroExit(code: number) {
  if (!props.tab.alive || !store.aiConfigured) return;
  // Ctrl+C (130) / 管道截断 SIGPIPE (141) 是用户主动行为, 不算错误
  if (code === 130 || code === 141) return;

  // P2 闭环: AI 自己建议的命令失败, 直接在同一对话自动跟进
  const meta = getTermMeta(props.tab.shellId);
  if (meta.lastCmd && takeAiCommand(props.tab.shellId, meta.lastCmd)) {
    triggerAnalyze(code, true);
    return;
  }

  if (store.aiAutoAnalyze) {
    // 全自动模式: 同一 shell 30 秒内最多触发一次
    const now = Date.now();
    if (now - (lastAutoAnalyze.get(props.tab.shellId) ?? 0) < 30_000) return;
    lastAutoAnalyze.set(props.tab.shellId, now);
    triggerAnalyze(code);
    return;
  }

  // 默认: 终端内轻量气泡, 用户点击才分析
  chipExitCode.value = code;
  showChip.value = true;
}

function analyzeFromChip() {
  showChip.value = false;
  triggerAnalyze(chipExitCode.value);
}

function copySelection() {
  const sel = term?.getSelection();
  if (sel) navigator.clipboard.writeText(sel).catch(() => {});
}

function askAi() {
  const sel = term?.getSelection().trim();
  if (!sel) return;
  store.sidePanel = "ai";
  store.aiPendingPrefill = sel;
}

function syncSize() {
  if (!term || !fit || !props.visible) return;
  fit.fit();
  api.resizeShell(props.tab.sessionId, props.tab.shellId, term.cols, term.rows).catch(() => {});
}

onMounted(async () => {
  term = new Terminal({
    cursorBlink: true,
    fontSize: 13,
    fontFamily: "'JetBrains Mono', Menlo, Monaco, 'Courier New', monospace",
    theme: {
      background: "#0a0a0a",
      foreground: "#fafafa",
      cursor: "#fafafa",
      selectionBackground: "#3f3f46",
    },
    allowProposedApi: true,
  });
  fit = new FitAddon();
  term.loadAddon(fit);
  term.open(container.value!);
  fit.fit();

  // 后端 shell 以 120x32 打开, 挂载后立即同步真实尺寸
  syncSize();

  // 注入退出码上报钩子 (bash/zsh), 失败时发 OSC 5151
  api.writeShell(
    props.tab.sessionId,
    props.tab.shellId,
    b64encode(new TextEncoder().encode(SHELL_INTEGRATION)),
  );
  oscDisposable = term.parser.registerOscHandler(5151, (data) => {
    const [ecStr, p64, b64] = data.split(";");
    const dec = (s: string | undefined) => {
      try {
        return s ? new TextDecoder().decode(b64decode(s)) : "";
      } catch {
        return "";
      }
    };
    setTermMeta(props.tab.shellId, { cwd: dec(p64), lastCmd: dec(b64) });
    // 新提示符 = 上一次失败已成为历史, 收起气泡
    showChip.value = false;
    const code = parseInt(ecStr, 10);
    if (Number.isFinite(code) && code !== 0) onNonZeroExit(code);
    return true;
  });
  term.onSelectionChange(() => {
    hasSelection.value = Boolean(term?.getSelection());
  });

  term.onData((data) => {
    api.writeShell(props.tab.sessionId, props.tab.shellId, b64encode(new TextEncoder().encode(data)));
  });
  term.onBinary((data) => {
    const bytes = new Uint8Array(data.length);
    for (let i = 0; i < data.length; i++) bytes[i] = data.charCodeAt(i);
    api.writeShell(props.tab.sessionId, props.tab.shellId, b64encode(bytes));
  });

  unlisten = await listen<{ shell_id: string; data: string }>("shell-data", (e) => {
    if (e.payload.shell_id === props.tab.shellId) {
      const bytes = b64decode(e.payload.data);
      term?.write(bytes);
      appendTermData(props.tab.shellId, new TextDecoder().decode(bytes));
    }
  });

  // AI 命令生成条快捷键 (仅当前可见标签响应)
  term.attachCustomKeyEventHandler((e) => {
    if (
      e.type === "keydown" &&
      !e.isComposing &&
      props.visible &&
      matchHotkey(e, store.aiHotkey)
    ) {
      store.commandBarOpen = !store.commandBarOpen;
      return false;
    }
    return true;
  });

  resizeObserver = new ResizeObserver(() => syncSize());
  resizeObserver.observe(container.value!);
});

watch(
  () => props.visible,
  (v) => {
    if (v) {
      requestAnimationFrame(() => {
        syncSize();
        term?.focus();
      });
    }
  },
);

watch(
  () => store.focusTick,
  () => {
    if (props.visible) term?.focus();
  },
);

onBeforeUnmount(() => {
  unlisten?.();
  oscDisposable?.dispose();
  resizeObserver?.disconnect();
  lastAutoAnalyze.delete(props.tab.shellId);
  dropTermContext(props.tab.shellId);
  dropConversation(props.tab.shellId);
  term?.dispose();
});
</script>

<template>
  <ContextMenu>
    <ContextMenuTrigger as-child>
      <div v-show="visible" class="absolute inset-0 p-1" style="background: #0a0a0a">
        <div ref="container" class="h-full w-full" />
        <div
          v-if="showChip"
          class="absolute bottom-4 right-4 z-10 flex items-center gap-1.5 rounded-md border bg-card px-2 py-1 text-xs shadow-lg"
        >
          <button class="text-primary hover:underline" @click="analyzeFromChip">
            ✨ AI 分析 (退出码 {{ chipExitCode }})
          </button>
          <button class="text-muted-foreground hover:text-foreground" @click="showChip = false">✕</button>
        </div>
      </div>
    </ContextMenuTrigger>
    <ContextMenuContent>
      <ContextMenuItem :disabled="!hasSelection" @click="copySelection">复制</ContextMenuItem>
      <ContextMenuItem :disabled="!hasSelection" @click="askAi">问 AI</ContextMenuItem>
    </ContextMenuContent>
  </ContextMenu>
</template>
