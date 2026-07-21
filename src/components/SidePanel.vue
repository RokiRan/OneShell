<script setup lang="ts">
import { computed } from "vue";
import { store } from "@/lib/store";
import SftpPanel from "./SftpPanel.vue";
import MonitorPanel from "./MonitorPanel.vue";
import ForwardPanel from "./ForwardPanel.vue";
import AiPanel from "./AiPanel.vue";

const sessionId = computed(() => store.activeSessionId());

const MIN_W = 260;

/** 拖左边缘调宽: 向左拖变宽, 松手后持久化 */
function startDrag(e: MouseEvent) {
  e.preventDefault();
  const startX = e.clientX;
  const startW = store.sidePanelWidth;
  // 拖动过程中禁止文本选择, 避免掠过终端时 xterm 进入选择态
  document.body.style.userSelect = "none";
  const onMove = (ev: MouseEvent) => {
    // 上限随窗口宽度收紧, 给中间终端区至少留 320px
    const maxW = Math.min(720, window.innerWidth - 320);
    store.sidePanelWidth = Math.min(maxW, Math.max(MIN_W, startW + (startX - ev.clientX)));
  };
  const onUp = () => {
    window.removeEventListener("mousemove", onMove);
    window.removeEventListener("mouseup", onUp);
    document.body.style.userSelect = "";
    localStorage.setItem("oneshell:side-panel-width", String(store.sidePanelWidth));
  };
  window.addEventListener("mousemove", onMove);
  window.addEventListener("mouseup", onUp);
}
</script>

<template>
  <div
    v-if="sessionId"
    class="relative flex shrink-0 flex-col border-l bg-card"
    :style="{ width: store.sidePanelWidth + 'px' }"
  >
    <div
      class="absolute inset-y-0 left-0 z-20 w-1 cursor-col-resize hover:bg-primary/40 active:bg-primary/60"
      @mousedown="startDrag"
    />
    <SftpPanel v-if="store.sidePanel === 'sftp'" :session-id="sessionId" />
    <MonitorPanel v-else-if="store.sidePanel === 'monitor'" :session-id="sessionId" />
    <ForwardPanel v-else-if="store.sidePanel === 'forward'" :session-id="sessionId" />
    <AiPanel v-else-if="store.sidePanel === 'ai'" :session-id="sessionId" />
  </div>
</template>
