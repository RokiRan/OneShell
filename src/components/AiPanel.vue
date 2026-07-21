<script setup lang="ts">
import { nextTick, onBeforeUnmount, ref } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { Send, SquareTerminal, Play, Stethoscope, Settings2, Square } from "@lucide/vue";
import { api, b64encode, type AiChunk, type ChatMsg } from "@/lib/api";
import { store } from "@/lib/store";
import { getTermContext } from "@/lib/term-context";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

const props = defineProps<{ sessionId: string }>();

interface UiMsg {
  role: "user" | "assistant";
  content: string;
}

const messages = ref<UiMsg[]>([]);
const input = ref("");
const sending = ref(false);
const chatEl = ref<HTMLElement>();
let unlisten: UnlistenFn | null = null;
let activeRequestId = "";

function buildSystem(): string {
  const info = store.sessions.get(props.sessionId);
  const ctx = getTermContext(store.activeTab, 4000);
  return [
    "你是 OneShell 内置的 SSH 运维助手, 用户通过你操作远程 Linux 服务器。",
    `当前会话: ${info?.label ?? "未知"}`,
    ctx ? `用户终端最近的输出:\n\`\`\`\n${ctx}\n\`\`\`` : "",
    "规则:",
    "1. 用简体中文回答, 简洁直接;",
    "2. 需要建议命令时, 用 ```bash 代码块给出, 每块一条完整命令;",
    "3. 涉及 rm -rf、dd、mkfs、shutdown 等破坏性命令时必须先警告风险;",
    "4. 不确定时说明, 不要编造命令输出。",
  ]
    .filter(Boolean)
    .join("\n");
}

async function scrollBottom() {
  await nextTick();
  chatEl.value?.scrollTo({ top: chatEl.value.scrollHeight });
}

async function request() {
  messages.value.push({ role: "assistant", content: "" });
  sending.value = true;
  await scrollBottom();

  const requestId = crypto.randomUUID();
  activeRequestId = requestId;
  const reqMsgs: ChatMsg[] = [
    { role: "system", content: buildSystem() },
    ...messages.value.slice(0, -1).slice(-20),
  ];

  unlisten?.();
  unlisten = await listen<AiChunk>("ai-chunk", (e) => {
    if (e.payload.request_id !== requestId) return;
    const last = messages.value[messages.value.length - 1];
    if (e.payload.error) {
      last.content = `⚠️ ${e.payload.error}`;
      sending.value = false;
    } else {
      last.content += e.payload.delta;
      if (e.payload.done) sending.value = false;
    }
    scrollBottom();
  });

  try {
    await api.aiChat(requestId, reqMsgs);
  } catch (e) {
    messages.value[messages.value.length - 1].content = `⚠️ ${String(e)}`;
    sending.value = false;
  }
}

async function send(text?: string) {
  const content = (text ?? input.value).trim();
  if (!content || sending.value) return;
  input.value = "";
  messages.value.push({ role: "user", content });
  await request();
}

function stop() {
  if (activeRequestId) api.aiCancel(activeRequestId);
  sending.value = false;
}

function diagnose() {
  send("诊断当前终端: 解释最近的输出, 如有错误请分析原因并给出修复命令。");
}

type Segment = { type: "text" | "cmd"; text: string };

function segments(content: string): Segment[] {
  const out: Segment[] = [];
  const re = /```(?:bash|sh|shell)?\s*\n([\s\S]*?)```/g;
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(content))) {
    if (m.index > last) out.push({ type: "text", text: content.slice(last, m.index) });
    out.push({ type: "cmd", text: m[1].trim() });
    last = m.index + m[0].length;
  }
  if (last < content.length) out.push({ type: "text", text: content.slice(last) });
  return out;
}

/** 把命令写入当前终端 (不自动回车) */
async function insertCommand(cmd: string) {
  if (!store.activeTab) return;
  await api.writeShell(props.sessionId, store.activeTab, b64encode(new TextEncoder().encode(cmd)));
  store.focusTick++;
}

/** 通过独立 exec 通道执行并把结果回灌给 AI */
async function runCommand(cmd: string) {
  if (sending.value) return;
  messages.value.push({ role: "user", content: `执行命令: \`${cmd}\`` });
  await scrollBottom();
  let out: string;
  try {
    out = await api.execCommand(props.sessionId, cmd);
  } catch (e) {
    out = `执行失败: ${String(e)}`;
  }
  const trimmed = out.length > 6000 ? out.slice(0, 6000) + "\n…(输出截断)" : out;
  messages.value.push({
    role: "user",
    content: `命令输出:\n\`\`\`\n${trimmed}\n\`\`\`\n请解读结果。`,
  });
  await request();
}

onBeforeUnmount(() => unlisten?.());
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col">
    <div class="flex items-center justify-between border-b px-2 py-1.5">
      <span class="text-xs font-medium">AI 助手</span>
      <div class="flex gap-0.5">
        <Button variant="ghost" size="icon" class="h-6 w-6" title="诊断当前终端" @click="diagnose">
          <Stethoscope class="h-3.5 w-3.5" />
        </Button>
        <Button variant="ghost" size="icon" class="h-6 w-6" title="AI 设置" @click="store.aiSettingsOpen = true">
          <Settings2 class="h-3.5 w-3.5" />
        </Button>
      </div>
    </div>

    <div ref="chatEl" class="min-h-0 flex-1 overflow-y-auto p-2">
      <div v-if="messages.length === 0" class="py-8 text-center text-xs text-muted-foreground">
        用自然语言描述你想做的事,<br />例如 "查看占用内存最高的 5 个进程"
      </div>
      <div v-for="(msg, i) in messages" :key="i" class="mb-2">
        <div
          class="rounded-md px-2 py-1.5 text-xs"
          :class="msg.role === 'user' ? 'ml-6 bg-primary/15' : 'mr-2 bg-muted'"
        >
          <template v-for="(seg, j) in segments(msg.content)" :key="j">
            <div v-if="seg.type === 'cmd'" class="my-1 rounded border bg-background p-1.5">
              <code class="block whitespace-pre-wrap break-all font-mono text-[11px]">{{ seg.text }}</code>
              <div class="mt-1 flex gap-1">
                <Button variant="outline" size="sm" class="h-5 px-1.5 text-[10px]" @click="insertCommand(seg.text)">
                  <SquareTerminal class="mr-1 h-3 w-3" />插入终端
                </Button>
                <Button
                  v-if="msg.role === 'assistant'"
                  variant="outline" size="sm" class="h-5 px-1.5 text-[10px]"
                  @click="runCommand(seg.text)"
                >
                  <Play class="mr-1 h-3 w-3" />执行
                </Button>
              </div>
            </div>
            <span v-else class="whitespace-pre-wrap break-words">{{ seg.text }}</span>
          </template>
        </div>
      </div>
      <div v-if="sending" class="text-xs text-muted-foreground">思考中…</div>
    </div>

    <div class="flex items-center gap-1 border-t p-2">
      <Input
        v-model="input"
        class="h-7 flex-1 text-xs"
        placeholder="描述你要做的事…"
        :disabled="sending"
        @keydown.enter="send()"
      />
      <Button
        v-if="sending"
        size="icon"
        variant="destructive"
        class="h-7 w-7"
        title="停止生成"
        @click="stop"
      >
        <Square class="h-3.5 w-3.5" />
      </Button>
      <Button v-else size="icon" class="h-7 w-7" :disabled="!input.trim()" @click="send()">
        <Send class="h-3.5 w-3.5" />
      </Button>
    </div>
  </div>
</template>
