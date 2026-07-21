<script setup lang="ts">
import { computed, ref } from "vue";
import { Plus, Server, TerminalSquare, Pencil, Trash2, FolderClosed, Settings } from "@lucide/vue";
import { store } from "@/lib/store";
import type { HostConfig } from "@/lib/api";
import { api } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import HostFormDialog from "./HostFormDialog.vue";

const dialogOpen = ref(false);
const editing = ref<HostConfig | null>(null);
const error = ref("");

const groups = computed(() => {
  const map = new Map<string, HostConfig[]>();
  for (const h of store.hosts) {
    const g = h.group || "默认分组";
    if (!map.has(g)) map.set(g, []);
    map.get(g)!.push(h);
  }
  return [...map.entries()];
});

async function connect(host: HostConfig) {
  error.value = "";
  try {
    await store.connect(host);
  } catch (e) {
    error.value = String(e);
  }
}

function edit(host: HostConfig) {
  editing.value = host;
  dialogOpen.value = true;
}

async function remove(host: HostConfig) {
  await api.deleteHost(host.id);
  await store.refreshHosts();
}

function openNew() {
  editing.value = null;
  dialogOpen.value = true;
}
</script>

<template>
  <aside class="flex flex-col bg-card">
    <div class="flex items-center justify-between px-3 py-2.5">
      <span class="text-sm font-semibold tracking-wide">OneShell</span>
      <Button variant="ghost" size="icon" class="h-7 w-7" title="新增主机" @click="openNew">
        <Plus class="h-4 w-4" />
      </Button>
    </div>
    <div v-if="error" class="mx-2 mb-1 rounded bg-destructive/15 px-2 py-1 text-xs text-destructive">
      {{ error }}
    </div>
    <ScrollArea class="min-h-0 flex-1">
      <div class="px-2 pb-2">
        <div v-for="[group, hosts] in groups" :key="group" class="mt-1">
          <div class="flex items-center gap-1 px-1 py-1 text-xs text-muted-foreground">
            <FolderClosed class="h-3.5 w-3.5" />
            {{ group }}
          </div>
          <ContextMenu v-for="host in hosts" :key="host.id">
            <ContextMenuTrigger>
              <div
                class="group flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-sm hover:bg-accent"
                @dblclick="connect(host)"
              >
                <Server class="h-4 w-4 shrink-0 text-muted-foreground" />
                <div class="min-w-0 flex-1">
                  <div class="truncate">{{ host.name || host.host }}</div>
                  <div class="truncate text-xs text-muted-foreground">
                    {{ host.username }}@{{ host.host }}:{{ host.port }}
                  </div>
                </div>
                <Button
                  variant="ghost"
                  size="icon"
                  class="h-6 w-6 opacity-0 group-hover:opacity-100"
                  :disabled="store.connecting.has(host.id)"
                  title="连接"
                  @click.stop="connect(host)"
                >
                  <TerminalSquare class="h-4 w-4" />
                </Button>
              </div>
            </ContextMenuTrigger>
            <ContextMenuContent>
              <ContextMenuItem @click="connect(host)">
                <TerminalSquare class="mr-2 h-4 w-4" /> 连接
              </ContextMenuItem>
              <ContextMenuItem @click="edit(host)">
                <Pencil class="mr-2 h-4 w-4" /> 编辑
              </ContextMenuItem>
              <ContextMenuItem class="text-destructive" @click="remove(host)">
                <Trash2 class="mr-2 h-4 w-4" /> 删除
              </ContextMenuItem>
            </ContextMenuContent>
          </ContextMenu>
        </div>
        <div v-if="store.hosts.length === 0" class="px-2 py-6 text-center text-xs text-muted-foreground">
          还没有主机,点击右上角 + 添加
        </div>
      </div>
    </ScrollArea>
    <div class="flex items-center justify-between border-t px-3 py-1.5">
      <span class="text-xs text-muted-foreground">{{ store.hosts.length }} 台主机</span>
      <Button variant="ghost" size="icon" class="h-6 w-6" title="AI 设置" @click="store.aiSettingsOpen = true">
        <Settings class="h-3.5 w-3.5" />
      </Button>
    </div>
    <HostFormDialog v-model:open="dialogOpen" :host="editing" />
  </aside>
</template>
