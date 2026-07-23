<script setup lang="ts">
import { computed, onMounted, reactive, ref } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { Plus, Play, Square, Trash2 } from "@lucide/vue";
import { api, type ForwardRule, type ForwardStatus } from "@/lib/api";
import { store } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/separator";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

const props = defineProps<{ sessionId: string }>();

const active = ref<Set<string>>(new Set());
const error = ref("");
const showForm = ref(false);
let unlisten: UnlistenFn | null = null;

const form = reactive({
  kind: "local" as "local" | "remote" | "dynamic",
  name: "",
  bind_host: "127.0.0.1",
  bind_port: 8080,
  target_host: "127.0.0.1",
  target_port: 80,
});

const host = computed(() => {
  const info = store.sessions.get(props.sessionId);
  return store.hosts.find((h) => h.id === info?.host_id) ?? null;
});

async function refresh() {
  const keys = await api.listForwards(props.sessionId);
  active.value = new Set(keys.map((k) => k.split(":").pop()!));
}

async function toggle(rule: ForwardRule) {
  error.value = "";
  try {
    if (active.value.has(rule.id)) {
      await api.stopForward(props.sessionId, rule.id);
    } else {
      await api.startForward(props.sessionId, rule);
    }
    await refresh();
  } catch (e) {
    error.value = String(e);
  }
}

async function addRule() {
  if (!host.value) return;
  const isDynamic = form.kind === "dynamic";
  const rule: ForwardRule = {
    id: crypto.randomUUID(),
    kind: form.kind,
    name:
      form.name ||
      (isDynamic ? `SOCKS5 :${form.bind_port}` : `${form.bind_port} → ${form.target_host}:${form.target_port}`),
    bind_host: form.bind_host,
    bind_port: Number(form.bind_port),
    // 动态转发无固定目标, 字段置空占位 (后端忽略)
    target_host: isDynamic ? "" : form.target_host,
    target_port: isDynamic ? 0 : Number(form.target_port),
  };
  host.value.forwards.push(rule);
  await api.saveHost(host.value);
  await store.refreshHosts();
  showForm.value = false;
}

async function removeRule(rule: ForwardRule) {
  if (!host.value) return;
  if (active.value.has(rule.id)) {
    await api.stopForward(props.sessionId, rule.id).catch(() => {});
  }
  host.value.forwards = host.value.forwards.filter((r) => r.id !== rule.id);
  await api.saveHost(host.value);
  await store.refreshHosts();
  await refresh();
}

onMounted(async () => {
  await refresh();
  unlisten = await listen<ForwardStatus>("forward-status", (e) => {
    if (e.payload.session_id !== props.sessionId) return;
    if (e.payload.active) active.value.add(e.payload.rule_id);
    else active.value.delete(e.payload.rule_id);
  });
});

defineExpose({ dispose: () => unlisten?.() });
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto p-3 text-xs">
    <div class="flex items-center justify-between">
      <span class="font-medium">端口转发</span>
      <Button variant="ghost" size="icon" class="h-6 w-6" title="新增规则" @click="showForm = !showForm">
        <Plus class="h-4 w-4" />
      </Button>
    </div>
    <div v-if="error" class="rounded bg-destructive/15 px-2 py-1 text-destructive">{{ error }}</div>

    <div v-if="showForm" class="space-y-2 rounded-md border p-2">
      <div class="space-y-1">
        <Label>类型</Label>
        <Select v-model="form.kind">
          <SelectTrigger class="h-7 text-xs"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="local">本地转发 (-L)</SelectItem>
            <SelectItem value="remote">远端转发 (-R)</SelectItem>
            <SelectItem value="dynamic">动态转发 / SOCKS5 (-D)</SelectItem>
          </SelectContent>
        </Select>
      </div>
      <div class="space-y-1">
        <Label>名称</Label>
        <Input v-model="form.name" class="h-7 text-xs" placeholder="可选" />
      </div>
      <div class="grid grid-cols-3 gap-1.5">
        <div class="col-span-2 space-y-1">
          <Label>{{ form.kind === "local" ? "本地监听" : form.kind === "dynamic" ? "SOCKS5 监听" : "远端监听" }}</Label>
          <Input v-model="form.bind_host" class="h-7 text-xs" />
        </div>
        <div class="space-y-1">
          <Label>端口</Label>
          <Input v-model.number="form.bind_port" type="number" class="h-7 text-xs" />
        </div>
        <template v-if="form.kind !== 'dynamic'">
          <div class="col-span-2 space-y-1">
            <Label>目标地址</Label>
            <Input v-model="form.target_host" class="h-7 text-xs" />
          </div>
          <div class="space-y-1">
            <Label>端口</Label>
            <Input v-model.number="form.target_port" type="number" class="h-7 text-xs" />
          </div>
        </template>
      </div>
      <Button size="sm" class="h-7 w-full text-xs" @click="addRule">保存规则</Button>
    </div>

    <Separator />

    <div v-for="rule in host?.forwards ?? []" :key="rule.id" class="rounded-md border p-2">
      <div class="flex items-center justify-between">
        <div class="flex items-center gap-1.5">
          <Badge variant="outline" class="text-[10px]">
            {{ rule.kind === "local" ? "本地" : rule.kind === "dynamic" ? "SOCKS5" : "远端" }}
          </Badge>
          <span class="font-medium">{{ rule.name }}</span>
        </div>
        <div class="flex items-center gap-0.5">
          <Button
            variant="ghost" size="icon" class="h-6 w-6"
            :title="active.has(rule.id) ? '停止' : '启动'"
            @click="toggle(rule)"
          >
            <Square v-if="active.has(rule.id)" class="h-3.5 w-3.5 text-red-400" />
            <Play v-else class="h-3.5 w-3.5 text-emerald-400" />
          </Button>
          <Button variant="ghost" size="icon" class="h-6 w-6" title="删除" @click="removeRule(rule)">
            <Trash2 class="h-3.5 w-3.5" />
          </Button>
        </div>
      </div>
      <div class="mt-1 text-muted-foreground">
        <template v-if="rule.kind === 'dynamic'">
          {{ rule.bind_host }}:{{ rule.bind_port }} → SOCKS5 代理
        </template>
        <template v-else>
          {{ rule.bind_host }}:{{ rule.bind_port }} → {{ rule.target_host }}:{{ rule.target_port }}
        </template>
        <span v-if="active.has(rule.id)" class="ml-1 text-emerald-400">● 运行中</span>
      </div>
    </div>
    <div v-if="(host?.forwards ?? []).length === 0" class="py-4 text-center text-muted-foreground">
      暂无转发规则
    </div>
  </div>
</template>
