<script setup lang="ts">
import { reactive, ref, watch } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api, type AiChunk, type AiConfig } from "@/lib/api";
import { store } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Switch } from "@/components/ui/switch";

const form = reactive<AiConfig>({ base_url: "", api_key: "", model: "", hotkey: "", auto_analyze: true });
const testing = ref(false);
const testResult = ref("");
const error = ref("");
let unlisten: UnlistenFn | null = null;

const TEMPLATES = [
  { name: "OpenAI", base_url: "https://api.openai.com/v1", model: "gpt-4o-mini" },
  { name: "DeepSeek", base_url: "https://api.deepseek.com/v1", model: "deepseek-chat" },
  { name: "MiniMax", base_url: "https://api.minimaxi.com/v1", model: "MiniMax-M2" },
  { name: "通义千问", base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1", model: "qwen-plus" },
];

watch(
  () => store.aiSettingsOpen,
  async (open) => {
    if (!open) return;
    testResult.value = "";
    error.value = "";
    Object.assign(form, await api.getAiConfig());
  },
);

function applyTemplate(t: (typeof TEMPLATES)[number]) {
  form.base_url = t.base_url;
  form.model = t.model;
}

async function save() {
  error.value = "";
  try {
    await api.saveAiConfig({ ...form });
    if (form.hotkey) store.aiHotkey = form.hotkey;
    store.aiAutoAnalyze = form.auto_analyze;
    store.aiConfigured = Boolean(form.base_url && form.api_key && form.model);
    store.aiSettingsOpen = false;
  } catch (e) {
    error.value = String(e);
  }
}

async function test() {
  testResult.value = "";
  error.value = "";
  testing.value = true;
  const requestId = crypto.randomUUID();
  let acc = "";
  try {
    await api.saveAiConfig({ ...form });
    unlisten?.();
    unlisten = await listen<AiChunk>("ai-chunk", (e) => {
      if (e.payload.request_id !== requestId) return;
      if (e.payload.error) testResult.value = `❌ ${e.payload.error}`;
      else {
        acc += e.payload.delta;
        if (e.payload.done) testResult.value = `✅ 连接成功: ${acc.slice(0, 80)}`;
      }
    });
    await api.aiChat(requestId, [{ role: "user", content: "ping, 回复 pong" }]);
  } catch (e) {
    testResult.value = `❌ ${String(e)}`;
  } finally {
    testing.value = false;
  }
}
</script>

<template>
  <Dialog :open="store.aiSettingsOpen" @update:open="store.aiSettingsOpen = $event">
    <DialogContent class="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>AI 设置</DialogTitle>
      </DialogHeader>
      <div class="space-y-3">
        <div class="flex flex-wrap gap-1.5">
          <Button
            v-for="t in TEMPLATES"
            :key="t.name"
            variant="outline"
            size="sm"
            class="h-6 text-xs"
            @click="applyTemplate(t)"
          >
            {{ t.name }}
          </Button>
        </div>
        <div class="space-y-1.5">
          <Label>Base URL (OpenAI 兼容)</Label>
          <Input v-model="form.base_url" placeholder="https://api.openai.com/v1" />
        </div>
        <div class="space-y-1.5">
          <Label>API Key</Label>
          <Input v-model="form.api_key" type="password" placeholder="sk-..." />
        </div>
        <div class="space-y-1.5">
          <Label>模型</Label>
          <Input v-model="form.model" placeholder="gpt-4o-mini" />
        </div>
        <div class="space-y-1.5">
          <Label>命令生成快捷键</Label>
          <Input v-model="form.hotkey" placeholder="meta+shift+k" />
          <p class="text-xs text-muted-foreground">修饰键: meta / ctrl / shift / alt, 用 + 连接</p>
        </div>
        <div class="flex items-center justify-between">
          <Label>命令失败时自动发送 AI 分析</Label>
          <Switch v-model="form.auto_analyze" />
        </div>
        <p class="-mt-1.5 text-xs text-muted-foreground">关闭时仅在终端显示提示气泡, 点击才分析</p>
        <p v-if="testResult" class="text-xs">{{ testResult }}</p>
        <p v-if="error" class="text-xs text-destructive">{{ error }}</p>
      </div>
      <DialogFooter>
        <Button variant="outline" :disabled="testing" @click="test">
          {{ testing ? "测试中…" : "测试连接" }}
        </Button>
        <Button @click="save">保存</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>
