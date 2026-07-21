<script setup lang="ts">
import { onMounted } from "vue";
import { listen } from "@tauri-apps/api/event";
import HostSidebar from "@/components/HostSidebar.vue";
import TerminalTabs from "@/components/TerminalTabs.vue";
import TerminalView from "@/components/TerminalView.vue";
import SidePanel from "@/components/SidePanel.vue";
import AiCommandBar from "@/components/AiCommandBar.vue";
import AiSettingsDialog from "@/components/AiSettingsDialog.vue";
import { store } from "@/lib/store";
import { api } from "@/lib/api";

onMounted(async () => {
  document.documentElement.classList.add("dark");
  await store.refreshHosts();
  const cfg = await api.getAiConfig().catch(() => null);
  if (cfg) {
    store.aiHotkey = cfg.hotkey || store.aiHotkey;
    store.aiAutoAnalyze = cfg.auto_analyze;
    store.aiConfigured = Boolean(cfg.base_url && cfg.api_key && cfg.model);
  }
  await listen<{ shell_id: string }>("shell-exit", (e) => {
    store.onShellExit(e.payload.shell_id);
  });
});
</script>

<template>
  <div class="flex h-screen w-screen overflow-hidden bg-background text-foreground">
    <HostSidebar class="w-64 shrink-0 border-r" />
    <div class="flex min-w-0 flex-1 flex-col">
      <TerminalTabs />
      <div class="flex min-h-0 flex-1">
        <div class="relative min-w-0 flex-1">
          <TerminalView
            v-for="tab in store.tabs"
            :key="tab.shellId"
            :tab="tab"
            :visible="store.activeTab === tab.shellId"
          />
          <AiCommandBar v-if="store.activeSessionId()" :session-id="store.activeSessionId()!" />
          <div
            v-if="store.tabs.length === 0"
            class="flex h-full flex-col items-center justify-center gap-2 text-muted-foreground"
          >
            <div class="text-5xl">🐚</div>
            <p class="text-sm">从左侧选择主机开始连接</p>
          </div>
        </div>
        <SidePanel v-if="store.sidePanel && store.activeTab" />
      </div>
    </div>
    <AiSettingsDialog />
  </div>
</template>
