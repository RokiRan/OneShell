<script setup lang="ts">
import { reactive, watch } from "vue";
import { api, type HostConfig } from "@/lib/api";
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

const props = defineProps<{ open: boolean; host: HostConfig | null }>();
const emit = defineEmits<{ "update:open": [boolean] }>();

const blank = () => ({
  name: "",
  group: "",
  host: "",
  port: 22,
  username: "root",
  authKind: "password" as "password" | "key",
  password: "",
  keyPath: "",
  passphrase: "",
});

const form = reactive(blank());
const error = defineModel<string>("error", { default: "" });

watch(
  () => props.open,
  (open) => {
    if (!open) return;
    error.value = "";
    if (props.host) {
      const h = props.host;
      form.name = h.name;
      form.group = h.group;
      form.host = h.host;
      form.port = h.port;
      form.username = h.username;
      form.authKind = h.auth.kind as "password" | "key";
      form.password = h.auth.kind === "password" ? h.auth.password : "";
      form.keyPath = h.auth.kind === "key" ? h.auth.key_path : "";
      form.passphrase = h.auth.kind === "key" ? (h.auth.passphrase ?? "") : "";
    } else {
      Object.assign(form, blank());
    }
  },
);

async function submit() {
  error.value = "";
  const host: HostConfig = {
    id: props.host?.id ?? crypto.randomUUID(),
    name: form.name.trim(),
    group: form.group.trim(),
    host: form.host.trim(),
    port: Number(form.port) || 22,
    username: form.username.trim(),
    auth:
      form.authKind === "password"
        ? { kind: "password", password: form.password }
        : {
            kind: "key",
            key_path: form.keyPath.trim(),
            passphrase: form.passphrase || null,
          },
    forwards: props.host?.forwards ?? [],
  };
  try {
    await api.saveHost(host);
    await store.refreshHosts();
    emit("update:open", false);
  } catch (e) {
    error.value = String(e);
  }
}
</script>

<template>
  <Dialog :open="open" @update:open="emit('update:open', $event)">
    <DialogContent class="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>{{ host ? "编辑主机" : "新增主机" }}</DialogTitle>
      </DialogHeader>
      <div class="grid grid-cols-2 gap-3">
        <div class="space-y-1.5">
          <Label>名称</Label>
          <Input v-model="form.name" placeholder="生产服务器" />
        </div>
        <div class="space-y-1.5">
          <Label>分组</Label>
          <Input v-model="form.group" placeholder="默认分组" />
        </div>
        <div class="space-y-1.5">
          <Label>主机地址 *</Label>
          <Input v-model="form.host" placeholder="192.168.1.10" />
        </div>
        <div class="space-y-1.5">
          <Label>端口</Label>
          <Input v-model.number="form.port" type="number" />
        </div>
        <div class="space-y-1.5">
          <Label>用户名 *</Label>
          <Input v-model="form.username" />
        </div>
        <div class="space-y-1.5">
          <Label>认证方式</Label>
          <Select v-model="form.authKind">
            <SelectTrigger><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="password">密码</SelectItem>
              <SelectItem value="key">私钥</SelectItem>
            </SelectContent>
          </Select>
        </div>
        <template v-if="form.authKind === 'password'">
          <div class="col-span-2 space-y-1.5">
            <Label>密码</Label>
            <Input v-model="form.password" type="password" />
          </div>
        </template>
        <template v-else>
          <div class="col-span-2 space-y-1.5">
            <Label>私钥路径</Label>
            <Input v-model="form.keyPath" placeholder="~/.ssh/id_ed25519" />
          </div>
          <div class="col-span-2 space-y-1.5">
            <Label>私钥口令 (可选)</Label>
            <Input v-model="form.passphrase" type="password" />
          </div>
        </template>
      </div>
      <p v-if="error" class="text-xs text-destructive">{{ error }}</p>
      <DialogFooter>
        <Button variant="outline" @click="emit('update:open', false)">取消</Button>
        <Button :disabled="!form.host || !form.username" @click="submit">保存</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>
