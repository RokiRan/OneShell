<script setup lang="ts">
import { nextTick, ref, watch } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { Sparkles, CornerDownLeft, X, LoaderCircle, Square } from "@lucide/vue";
import { api, b64encode, type AiChunk } from "@/lib/api";
import { store } from "@/lib/store";
import { getTermContext } from "@/lib/term-context";

const props = defineProps<{ sessionId: string }>();

const prompt = ref("");
const command = ref("");
const loading = ref(false);
const error = ref("");
const inputEl = ref<HTMLInputElement>();
let unlisten: UnlistenFn | null = null;
let activeRequestId = "";

function cancelRequest() {
  if (activeRequestId) {
    api.aiCancel(activeRequestId);
    activeRequestId = "";
  }
  loading.value = false;
}

watch(
  () => store.commandBarOpen,
  async (open) => {
    if (open) {
      prompt.value = "";
      command.value = "";
      error.value = "";
      await nextTick();
      inputEl.value?.focus();
    } else {
      unlisten?.();
    }
  },
);

function close() {
  cancelRequest();
  store.commandBarOpen = false;
  store.focusTick++;
}

async function refocus() {
  await nextTick();
  inputEl.value?.focus();
}

async function generate() {
  const text = prompt.value.trim();
  if (!text || loading.value) return;
  loading.value = true;
  command.value = "";
  error.value = "";

  const ctx = getTermContext(store.activeTab, 2000);
  const requestId = crypto.randomUUID();
  activeRequestId = requestId;
  unlisten?.();
  unlisten = await listen<AiChunk>("ai-chunk", (e) => {
    if (e.payload.request_id !== requestId) return;
    if (e.payload.error) {
      error.value = e.payload.error;
      loading.value = false;
      refocus();
      return;
    }
    command.value += e.payload.delta;
    if (e.payload.done) {
      // 清理模型可能带的思考块 / markdown 包裹 / 解释
      command.value = command.value
        .replace(/<think>[\s\S]*?(<\/think>|$)/g, "")
        .replace(/```(?:bash|sh|shell)?/g, "")
        .replace(/^\s*(命令|command)[:：]\s*/i, "")
        .trim()
        .split("\n")
        .filter((l) => l.trim() && !l.trim().startsWith("#"))
        .join(" && ");
      loading.value = false;
      refocus();
    }
  });

  try {
    await api.aiChat(requestId, [
      {
        role: "system",
        content: [
          "你是 shell 命令生成器。把用户的自然语言需求转换为一条可在 Linux bash 执行的命令。",
          "只输出命令本身, 不要任何解释、不要使用 markdown 代码块。多步操作用 && 或管道连接为一行。",
          ctx ? `参考: 用户终端最近的输出:\n${ctx}` : "",
        ]
          .filter(Boolean)
          .join("\n"),
      },
      { role: "user", content: text },
    ]);
  } catch (e) {
    error.value = String(e);
    loading.value = false;
    refocus();
  }
}

/** 把生成的命令写入终端 (不自动回车, 交给用户确认) */
async function accept() {
  const cmd = command.value.trim();
  if (!cmd || !store.activeTab) return;
  await api.writeShell(
    props.sessionId,
    store.activeTab,
    b64encode(new TextEncoder().encode(cmd)),
  );
  close();
}

function onKeydown(e: KeyboardEvent) {
  if (e.isComposing) return;
  if (e.key === "Escape") close();
  else if (e.key === "Enter") {
    if (command.value && !loading.value) accept();
    else generate();
  }
}
</script>

<template>
  <div
    v-if="store.commandBarOpen"
    class="absolute inset-x-0 bottom-4 z-10 mx-auto w-[36rem] max-w-[90%] rounded-lg border bg-popover shadow-xl"
    @keydown="onKeydown"
  >
    <div class="flex items-center gap-2 border-b px-3 py-2">
      <Sparkles class="h-4 w-4 shrink-0 text-amber-400" />
      <input
        ref="inputEl"
        v-model="prompt"
        class="h-6 flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground"
        placeholder="描述要执行的命令, 如: 查找大于 100M 的文件"
        :readonly="loading"
      />
      <LoaderCircle v-if="loading" class="h-3.5 w-3.5 shrink-0 animate-spin text-muted-foreground" />
      <button
        v-if="loading"
        class="shrink-0 rounded p-0.5 text-destructive hover:bg-accent"
        title="停止生成"
        @click="cancelRequest"
      >
        <Square class="h-3.5 w-3.5" />
      </button>
      <button class="rounded p-0.5 hover:bg-accent" @click="close">
        <X class="h-3.5 w-3.5" />
      </button>
    </div>
    <div v-if="loading && !command" class="px-3 py-2 text-xs text-muted-foreground">生成中…</div>
    <div v-if="command" class="px-3 py-2">
      <code class="block whitespace-pre-wrap break-all font-mono text-xs text-emerald-400">{{ command }}</code>
      <div class="mt-1 flex items-center gap-1 text-[10px] text-muted-foreground">
        <CornerDownLeft class="h-3 w-3" /> Enter 插入终端 · Esc 取消 · 可继续编辑上方描述重新生成
      </div>
    </div>
    <div v-if="error" class="px-3 py-2 text-xs text-destructive">{{ error }}</div>
  </div>
</template>
