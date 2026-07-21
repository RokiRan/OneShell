<script setup lang="ts">
import { X, Plus, FolderTree, Activity, ArrowLeftRight, PanelRightClose, PanelRightOpen, Sparkles } from "@lucide/vue";
import { store, type SidePanelKind } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { api } from "@/lib/api";

function togglePanel(kind: SidePanelKind) {
  store.sidePanel = store.sidePanel === kind ? null : kind;
}

async function newShell() {
  const sessionId = store.activeSessionId();
  if (!sessionId) return;
  const info = store.sessions.get(sessionId);
  const shellId = await api.openShell(sessionId, 120, 32);
  store.tabs.push({
    shellId,
    sessionId,
    title: info?.label.split(" ")[0] ?? "shell",
    alive: true,
  });
  store.activeTab = shellId;
}
</script>

<template>
  <div class="flex h-9 items-center gap-0.5 border-b bg-card px-1">
    <div class="flex min-w-0 flex-1 items-center gap-0.5 overflow-x-auto">
      <div
        v-for="tab in store.tabs"
        :key="tab.shellId"
        class="group flex h-7 cursor-pointer items-center gap-1.5 rounded-md px-2.5 text-xs"
        :class="
          store.activeTab === tab.shellId
            ? 'bg-accent text-accent-foreground'
            : 'text-muted-foreground hover:bg-accent/50'
        "
        @click="store.activeTab = tab.shellId"
      >
        <span
          class="h-1.5 w-1.5 shrink-0 rounded-full"
          :class="tab.alive ? 'bg-emerald-500' : 'bg-zinc-500'"
        />
        <span class="max-w-40 truncate">{{ tab.title }}</span>
        <button
          class="rounded p-0.5 opacity-0 hover:bg-background group-hover:opacity-100"
          @click.stop="store.closeTab(tab.shellId)"
        >
          <X class="h-3 w-3" />
        </button>
      </div>
      <Button
        v-if="store.tabs.length > 0"
        variant="ghost"
        size="icon"
        class="h-6 w-6 shrink-0"
        title="新标签"
        @click="newShell"
      >
        <Plus class="h-3.5 w-3.5" />
      </Button>
    </div>
    <div v-if="store.activeTab" class="flex shrink-0 items-center gap-0.5">
      <Button
        variant="ghost" size="icon" class="h-7 w-7" title="AI 助手"
        :class="store.sidePanel === 'ai' && 'bg-accent'"
        @click="togglePanel('ai')"
      >
        <Sparkles class="h-4 w-4" />
      </Button>
      <Button
        variant="ghost" size="icon" class="h-7 w-7" title="SFTP 文件"
        :class="store.sidePanel === 'sftp' && 'bg-accent'"
        @click="togglePanel('sftp')"
      >
        <FolderTree class="h-4 w-4" />
      </Button>
      <Button
        variant="ghost" size="icon" class="h-7 w-7" title="服务器监控"
        :class="store.sidePanel === 'monitor' && 'bg-accent'"
        @click="togglePanel('monitor')"
      >
        <Activity class="h-4 w-4" />
      </Button>
      <Button
        variant="ghost" size="icon" class="h-7 w-7" title="端口转发"
        :class="store.sidePanel === 'forward' && 'bg-accent'"
        @click="togglePanel('forward')"
      >
        <ArrowLeftRight class="h-4 w-4" />
      </Button>
      <Button
        variant="ghost" size="icon" class="h-7 w-7"
        :title="store.sidePanel ? '收起面板' : '展开面板'"
        @click="store.sidePanel = store.sidePanel ? null : 'sftp'"
      >
        <PanelRightClose v-if="store.sidePanel" class="h-4 w-4" />
        <PanelRightOpen v-else class="h-4 w-4" />
      </Button>
    </div>
  </div>
</template>
