<script setup lang="ts">
import { onMounted, ref } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import {
  ArrowUp,
  RefreshCw,
  FolderPlus,
  Upload,
  Folder,
  File,
  Link,
  Download,
  Pencil,
  Trash2,
} from "@lucide/vue";
import { api, formatBytes, type FileEntry, type TransferProgress } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Progress } from "@/components/ui/progress";
import { ScrollArea } from "@/components/ui/scroll-area";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";

const props = defineProps<{ sessionId: string }>();

const path = ref("/");
const entries = ref<FileEntry[]>([]);
const loading = ref(false);
const error = ref("");
const transfers = ref<TransferProgress[]>([]);
let unlisten: UnlistenFn | null = null;

async function load(p = path.value) {
  loading.value = true;
  error.value = "";
  try {
    entries.value = await api.sftpList(props.sessionId, p);
    path.value = p;
  } catch (e) {
    error.value = String(e);
  } finally {
    loading.value = false;
  }
}

function goUp() {
  if (path.value === "/") return;
  const idx = path.value.lastIndexOf("/");
  load(idx <= 0 ? "/" : path.value.slice(0, idx));
}

function openEntry(e: FileEntry) {
  if (e.is_dir) load(e.path);
}

async function mkdir() {
  const name = prompt("新建文件夹名称:");
  if (!name) return;
  try {
    await api.sftpMkdir(props.sessionId, `${path.value.replace(/\/$/, "")}/${name}`);
    await load();
  } catch (e) {
    error.value = String(e);
  }
}

async function upload() {
  const selected = await openDialog({ multiple: false });
  if (!selected) return;
  const name = selected.split("/").pop()!;
  const remote = `${path.value.replace(/\/$/, "")}/${name}`;
  await api.sftpUpload(props.sessionId, selected, remote).catch((e) => (error.value = String(e)));
}

async function download(e: FileEntry) {
  const dest = await saveDialog({ defaultPath: e.name });
  if (!dest) return;
  await api
    .sftpDownload(props.sessionId, e.path, dest)
    .catch((err) => (error.value = String(err)));
}

async function renameEntry(e: FileEntry) {
  const name = prompt("重命名为:", e.name);
  if (!name || name === e.name) return;
  const base = e.path.slice(0, e.path.length - e.name.length);
  try {
    await api.sftpRename(props.sessionId, e.path, `${base}${name}`);
    await load();
  } catch (err) {
    error.value = String(err);
  }
}

async function removeEntry(e: FileEntry) {
  if (!confirm(`确认删除 ${e.name}?`)) return;
  try {
    await api.sftpRemove(props.sessionId, e.path, e.is_dir);
    await load();
  } catch (err) {
    error.value = String(err);
  }
}

function formatTime(mtime: number): string {
  if (!mtime) return "";
  return new Date(mtime * 1000).toLocaleString("zh-CN", { hour12: false });
}

onMounted(async () => {
  try {
    path.value = await api.sftpHome(props.sessionId);
  } catch {
    path.value = "/";
  }
  await load();
  unlisten = await listen<TransferProgress>("sftp-progress", (e) => {
    const p = e.payload;
    const idx = transfers.value.findIndex((t) => t.op_id === p.op_id);
    if (idx >= 0) transfers.value[idx] = p;
    else transfers.value.push(p);
    if (p.done) {
      if (p.error) error.value = p.error;
      else load();
      setTimeout(() => {
        transfers.value = transfers.value.filter((t) => t.op_id !== p.op_id);
      }, 3000);
    }
  });
});

defineExpose({ dispose: () => unlisten?.() });
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col">
    <div class="flex items-center gap-1 border-b px-2 py-1.5">
      <Button variant="ghost" size="icon" class="h-6 w-6" title="上级目录" @click="goUp">
        <ArrowUp class="h-3.5 w-3.5" />
      </Button>
      <Input
        v-model="path"
        class="h-6 flex-1 border-0 bg-transparent px-1 text-xs shadow-none focus-visible:ring-0"
        @keydown.enter="load()"
      />
      <Button variant="ghost" size="icon" class="h-6 w-6" title="刷新" @click="load()">
        <RefreshCw class="h-3.5 w-3.5" :class="loading && 'animate-spin'" />
      </Button>
      <Button variant="ghost" size="icon" class="h-6 w-6" title="新建文件夹" @click="mkdir">
        <FolderPlus class="h-3.5 w-3.5" />
      </Button>
      <Button variant="ghost" size="icon" class="h-6 w-6" title="上传" @click="upload">
        <Upload class="h-3.5 w-3.5" />
      </Button>
    </div>
    <div v-if="error" class="mx-2 mt-1 rounded bg-destructive/15 px-2 py-1 text-xs text-destructive">
      {{ error }}
    </div>
    <ScrollArea class="min-h-0 flex-1">
      <div class="p-1">
        <ContextMenu v-for="e in entries" :key="e.path">
          <ContextMenuTrigger>
            <div
              class="flex cursor-pointer items-center gap-2 rounded px-2 py-1 text-xs hover:bg-accent"
              @dblclick="openEntry(e)"
            >
              <Folder v-if="e.is_dir" class="h-3.5 w-3.5 shrink-0 text-sky-400" />
              <Link v-else-if="e.is_symlink" class="h-3.5 w-3.5 shrink-0 text-amber-400" />
              <File v-else class="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
              <span class="min-w-0 flex-1 truncate" :title="`${e.path}\n${formatTime(e.mtime)}`">{{ e.name }}</span>
              <span class="shrink-0 text-muted-foreground">{{ e.is_dir ? "" : formatBytes(e.size) }}</span>
            </div>
          </ContextMenuTrigger>
          <ContextMenuContent>
            <ContextMenuItem v-if="!e.is_dir" @click="download(e)">
              <Download class="mr-2 h-4 w-4" /> 下载
            </ContextMenuItem>
            <ContextMenuItem @click="renameEntry(e)">
              <Pencil class="mr-2 h-4 w-4" /> 重命名
            </ContextMenuItem>
            <ContextMenuItem class="text-destructive" @click="removeEntry(e)">
              <Trash2 class="mr-2 h-4 w-4" /> 删除
            </ContextMenuItem>
          </ContextMenuContent>
        </ContextMenu>
        <div v-if="!loading && entries.length === 0" class="py-4 text-center text-xs text-muted-foreground">
          空目录
        </div>
      </div>
    </ScrollArea>
    <div v-if="transfers.length" class="border-t p-2">
      <div v-for="t in transfers" :key="t.op_id" class="mb-1.5">
        <div class="mb-0.5 flex justify-between text-xs text-muted-foreground">
          <span>{{ t.kind === "upload" ? "上传" : "下载" }}</span>
          <span>{{ formatBytes(t.transferred) }} / {{ formatBytes(t.total) }}</span>
        </div>
        <Progress :model-value="t.total ? (t.transferred / t.total) * 100 : 0" class="h-1.5" />
      </div>
    </div>
  </div>
</template>
