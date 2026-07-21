<script setup lang="ts">
import { onMounted, onBeforeUnmount, ref, watch } from "vue";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import "@xterm/xterm/css/xterm.css";
import { api, b64decode, b64encode } from "@/lib/api";
import { store, type TermTab } from "@/lib/store";
import { appendTermData, dropTermContext } from "@/lib/term-context";

const props = defineProps<{ tab: TermTab; visible: boolean }>();

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
let term: Terminal | null = null;
let fit: FitAddon | null = null;
let unlisten: UnlistenFn | null = null;
let resizeObserver: ResizeObserver | null = null;

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
  resizeObserver?.disconnect();
  dropTermContext(props.tab.shellId);
  term?.dispose();
});
</script>

<template>
  <div v-show="visible" class="absolute inset-0 p-1" style="background: #0a0a0a">
    <div ref="container" class="h-full w-full" />
  </div>
</template>
