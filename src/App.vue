<script setup lang="ts">
import { onMounted, ref } from "vue";
import { listen } from "@tauri-apps/api/event";
import { KeyRound, RefreshCw } from "@lucide/vue";
import HostSidebar from "@/components/HostSidebar.vue";
import TerminalTabs from "@/components/TerminalTabs.vue";
import TerminalView from "@/components/TerminalView.vue";
import SidePanel from "@/components/SidePanel.vue";
import AiCommandBar from "@/components/AiCommandBar.vue";
import AiSettingsDialog from "@/components/AiSettingsDialog.vue";
import HostKeyDialog from "@/components/HostKeyDialog.vue";
import { Button } from "@/components/ui/button";
import { store } from "@/lib/store";
import { api } from "@/lib/api";

const migrationBusy = ref(false);

async function retryMigration() {
  if (migrationBusy.value) return;
  migrationBusy.value = true;
  store.migrationError = "";
  try {
    await api.retryCredentialMigration();
    store.migrationPending = false;
    await store.refreshHosts();
  } catch (e) {
    store.migrationError = String(e);
  } finally {
    migrationBusy.value = false;
  }
}

onMounted(async () => {
  document.documentElement.classList.add("dark");
  await store.refreshHosts();
  store.migrationPending = await api.credentialMigrationPending().catch(() => false);
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
      <div
        v-if="store.migrationPending"
        class="flex items-center gap-2 border-b border-amber-500/40 bg-amber-500/10 px-3 py-1.5 text-xs text-amber-200"
      >
        <KeyRound class="h-3.5 w-3.5 shrink-0" />
        <span class="min-w-0 flex-1 truncate">
          旧版明文凭据尚未迁入系统钥匙串, 携带密码/口令的主机暂不可连接。
          <span v-if="store.migrationError" class="text-amber-400">{{ store.migrationError }}</span>
        </span>
        <Button
          size="sm"
          variant="outline"
          class="h-6 gap-1 text-xs"
          :disabled="migrationBusy"
          @click="retryMigration"
        >
          <RefreshCw class="h-3 w-3" />
          重试迁移
        </Button>
      </div>
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
    <HostKeyDialog />
  </div>
</template>
