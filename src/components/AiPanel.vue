<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { marked } from "marked";
import DOMPurify from "dompurify";
import { Send, SquareTerminal, Play, Stethoscope, Settings2, Square, Trash2 } from "@lucide/vue";
import { api, b64encode, type AiChunk, type ChatMsg } from "@/lib/api";
import { store } from "@/lib/store";
import { getTermContext, getTermMeta, markAiCommand } from "@/lib/term-context";
import { activeRequests, getConversation, type UiMsg } from "@/lib/ai-conversations";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

const props = defineProps<{ sessionId: string }>();

/** 当前标签的对话; 数组按引用共享, 后台流式直接写入所属标签的数组 */
const messages = ref<UiMsg[]>(getConversation(store.activeTab));
const input = ref("");
const sending = computed(() => activeRequests.has(store.activeTab));
const chatEl = ref<HTMLElement>();
/** 进行中的请求监听器; 组件卸载不销毁, 由请求结束时自清理 */
const unlistens = new Set<UnlistenFn>();

/** 消费右键 "问 AI" 预填内容 (面板未挂载时触发的事件靠挂载时消费) */
function consumePrefill() {
  const sel = store.aiPendingPrefill;
  if (!sel) return;
  store.aiPendingPrefill = "";
  input.value = `分析以下内容:\n\`\`\`\n${sel}\n\`\`\`\n`;
}

/** 消费命令失败的自动分析请求 */
function consumeAnalyze() {
  const p = store.aiPendingAnalyze;
  if (!p) return;
  const tab = store.tabs.find((t) => t.shellId === p.shellId);
  if (!tab || tab.sessionId !== props.sessionId) return;
  store.aiPendingAnalyze = null;
  if (activeRequests.has(p.shellId)) return;
  const meta = getTermMeta(p.shellId);
  const prompt = [
    p.followUp
      ? `我之前建议的命令执行失败了 (退出码 ${p.exitCode}), 我来跟进分析。`
      : `刚才命令执行失败 (退出码 ${p.exitCode})。`,
    meta.lastCmd ? `失败命令: \`${meta.lastCmd}\`` : "",
    meta.cwd ? `工作目录: ${meta.cwd}` : "",
    "请分析当前终端最近的输出, 解释失败原因并给出修复命令。",
  ]
    .filter(Boolean)
    .join("\n");
  getConversation(p.shellId).push({ role: "user", content: prompt });
  request(p.shellId);
}

onMounted(() => {
  consumePrefill();
  consumeAnalyze();
  scrollBottom();
});

watch(() => store.aiPendingPrefill, consumePrefill);
watch(() => store.aiPendingAnalyze, consumeAnalyze);
// 切标签 = 切对话上下文; 后台流式输出不受影响, 切回时内容完整
watch(
  () => store.activeTab,
  (tab) => {
    messages.value = getConversation(tab);
    scrollBottom();
  },
);

function buildSystem(tabId: string): string {
  const tab = store.tabs.find((t) => t.shellId === tabId);
  const info = tab ? store.sessions.get(tab.sessionId) : undefined;
  const ctx = getTermContext(tabId, 4000);
  const meta = getTermMeta(tabId);
  return [
    "你是 OneShell 内置的 SSH 运维助手, 用户通过你操作远程 Linux 服务器。",
    `当前会话: ${info?.label ?? "未知"}`,
    meta.cwd ? `当前目录: ${meta.cwd}` : "",
    meta.lastCmd ? `最近执行的命令: ${meta.lastCmd}` : "",
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

async function request(tabId: string) {
  const target = getConversation(tabId);
  target.push({ role: "assistant", content: "" });
  if (store.activeTab === tabId) await scrollBottom();

  const requestId = crypto.randomUUID();
  activeRequests.set(tabId, requestId);
  const reqMsgs: ChatMsg[] = [
    { role: "system", content: buildSystem(tabId) },
    ...target.slice(0, -1).slice(-20),
  ];

  let finished = false;
  let un: UnlistenFn | null = null;
  function finish() {
    if (finished) return;
    finished = true;
    if (activeRequests.get(tabId) === requestId) activeRequests.delete(tabId);
    if (un) {
      unlistens.delete(un);
      un();
    }
  }

  un = await listen<AiChunk>("ai-chunk", (e) => {
    if (e.payload.request_id !== requestId) return;
    const last = target[target.length - 1];
    if (e.payload.error) {
      last.content = `⚠️ ${e.payload.error}`;
      finish();
    } else {
      last.content += e.payload.delta;
      if (e.payload.done) finish();
    }
    // 组件可能已卸载或已切走, chatEl 为空时静默跳过
    if (store.activeTab === tabId) scrollBottom();
  });
  unlistens.add(un);

  try {
    await api.aiChat(requestId, reqMsgs);
  } catch (e) {
    target[target.length - 1].content = `⚠️ ${String(e)}`;
    finish();
  }
}

async function send(text?: string) {
  const content = (text ?? input.value).trim();
  if (!content || sending.value) return;
  input.value = "";
  getConversation(store.activeTab).push({ role: "user", content });
  await request(store.activeTab);
}

function stop() {
  const rid = activeRequests.get(store.activeTab);
  if (rid) api.aiCancel(rid);
  activeRequests.delete(store.activeTab);
}

function clearConversation() {
  stop();
  messages.value.length = 0;
}

function diagnose() {
  send("诊断当前终端: 解释最近的输出, 如有错误请分析原因并给出修复命令。");
}

type Segment = { type: "text" | "cmd"; text: string };

marked.use({ gfm: true, breaks: true });

/** AI 输出不可信: markdown 渲染后必须消毒再 v-html */
function renderMd(text: string): string {
  return DOMPurify.sanitize(marked.parse(text, { async: false }));
}

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
  markAiCommand(store.activeTab, cmd);
  await api.writeShell(props.sessionId, store.activeTab, b64encode(new TextEncoder().encode(cmd)));
  store.focusTick++;
}

/** 通过独立 exec 通道执行并把结果回灌给 AI */
async function runCommand(cmd: string) {
  const tabId = store.activeTab;
  if (activeRequests.has(tabId)) return;
  const target = getConversation(tabId);
  target.push({ role: "user", content: `执行命令: \`${cmd}\`` });
  await scrollBottom();
  let out: string;
  try {
    out = await api.execCommand(props.sessionId, cmd);
  } catch (e) {
    out = `执行失败: ${String(e)}`;
  }
  const trimmed = out.length > 6000 ? out.slice(0, 6000) + "\n…(输出截断)" : out;
  target.push({
    role: "user",
    content: `命令输出:\n\`\`\`\n${trimmed}\n\`\`\`\n请解读结果。`,
  });
  await request(tabId);
}
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col">
    <div class="flex items-center justify-between border-b px-2 py-1.5">
      <span class="text-xs font-medium">AI 助手</span>
      <div class="flex gap-0.5">
        <Button variant="ghost" size="icon" class="h-6 w-6" title="诊断当前终端" @click="diagnose">
          <Stethoscope class="h-3.5 w-3.5" />
        </Button>
        <Button variant="ghost" size="icon" class="h-6 w-6" title="清空当前标签的对话" @click="clearConversation">
          <Trash2 class="h-3.5 w-3.5" />
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
            <div v-if="seg.type === 'cmd'" class="my-1 flex items-start gap-1 rounded border bg-background p-1.5">
              <code class="min-w-0 flex-1 whitespace-pre-wrap break-all font-mono text-[11px]">{{ seg.text }}</code>
              <div class="flex shrink-0 gap-0.5">
                <Button
                  variant="ghost" size="icon" class="h-5 w-5" title="插入终端"
                  @click="insertCommand(seg.text)"
                >
                  <SquareTerminal class="h-3 w-3" />
                </Button>
                <Button
                  v-if="msg.role === 'assistant'"
                  variant="ghost" size="icon" class="h-5 w-5" title="执行"
                  @click="runCommand(seg.text)"
                >
                  <Play class="h-3 w-3" />
                </Button>
              </div>
            </div>
            <div v-else class="md-body min-w-0 text-xs leading-relaxed" v-html="renderMd(seg.text)" />
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

<style scoped>
/* v-html 渲染的 markdown 内容, 用 :deep 渗透 scoped 边界 */
.md-body {
  /* 长 token (request_id 等无空格字符串) 强制换行, 不撑破气泡 */
  overflow-wrap: anywhere;
  word-break: break-word;
}
.md-body :deep(p) {
  margin: 0.3rem 0;
}
.md-body :deep(h1),
.md-body :deep(h2),
.md-body :deep(h3),
.md-body :deep(h4) {
  margin: 0.5rem 0 0.25rem;
  font-weight: 600;
}
.md-body :deep(h1) { font-size: 0.95rem; }
.md-body :deep(h2) { font-size: 0.9rem; }
.md-body :deep(h3),
.md-body :deep(h4) { font-size: 0.8rem; }
.md-body :deep(ul),
.md-body :deep(ol) {
  margin: 0.25rem 0;
  padding-left: 1.1rem;
}
.md-body :deep(ul) { list-style: disc; }
.md-body :deep(ol) { list-style: decimal; }
.md-body :deep(li) { margin: 0.1rem 0; }
.md-body :deep(code) {
  border-radius: 4px;
  background: var(--color-muted);
  padding: 0.05rem 0.3rem;
  font-family: ui-monospace, monospace;
  font-size: 11px;
  word-break: break-all;
}
.md-body :deep(pre) {
  margin: 0.3rem 0;
  overflow-x: auto;
  border-radius: 6px;
  border: 1px solid var(--color-border);
  background: var(--color-background);
  padding: 0.5rem;
}
.md-body :deep(pre code) {
  background: transparent;
  padding: 0;
  word-break: normal;
}
.md-body :deep(a) {
  color: var(--color-primary);
  text-decoration: underline;
}
.md-body :deep(strong) { font-weight: 600; }
.md-body :deep(blockquote) {
  margin: 0.3rem 0;
  border-left: 2px solid var(--color-border);
  padding-left: 0.6rem;
  color: var(--color-muted-foreground);
}
.md-body :deep(hr) {
  margin: 0.5rem 0;
  border-color: var(--color-border);
}
.md-body :deep(table) {
  margin: 0.3rem 0;
  border-collapse: collapse;
}
.md-body :deep(th),
.md-body :deep(td) {
  border: 1px solid var(--color-border);
  padding: 0.15rem 0.5rem;
}
</style>
