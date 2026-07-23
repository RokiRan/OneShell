<script setup lang="ts">
import { computed, ref } from "vue";
import { ShieldAlert, ShieldQuestion, OctagonX } from "@lucide/vue";
import { api } from "@/lib/api";
import { store } from "@/lib/store";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

const busy = ref(false);
const error = ref("");

// ── TOFU 首次确认 ──
const prompt = computed(() => store.hostKeyPrompt);

async function trust() {
  const p = prompt.value;
  if (!p || busy.value) return;
  busy.value = true;
  error.value = "";
  try {
    await api.acceptHostKey(p.error.check_id);
    const host = p.host;
    store.hostKeyPrompt = null;
    await store.connect(host); // 重连: check_server_key 用持久化记录精确再校验
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}

async function cancelPrompt() {
  const p = prompt.value;
  store.hostKeyPrompt = null;
  if (p) await api.dismissHostKey(p.error.check_id).catch(() => {});
}

// ── 硬告警 (mismatch / revoked / CA) — 无“仍然连接”出口 ──
const alert = computed(() => store.hostKeyAlert);
const alertTitle = computed(() => {
  switch (alert.value?.kind) {
    case "host_key_mismatch":
      return "主机密钥已变更";
    case "host_key_revoked":
      return "主机密钥已被吊销";
    case "unsupported_cert_authority":
      return "不支持证书机构密钥";
    default:
      return "连接被拒绝";
  }
});
</script>

<template>
  <!-- TOFU: 首次连接未知主机 -->
  <Dialog :open="!!prompt" @update:open="(v) => !v && cancelPrompt()">
    <DialogContent v-if="prompt" class="sm:max-w-md">
      <DialogHeader>
        <DialogTitle class="flex items-center gap-2">
          <ShieldQuestion class="h-5 w-5 text-amber-400" />
          首次连接 {{ prompt.error.host }}:{{ prompt.error.port }}
        </DialogTitle>
        <DialogDescription>
          无法验证该主机的真实性。确认指纹无误后再信任, 信任记录将保存在本机。
        </DialogDescription>
      </DialogHeader>
      <div class="space-y-2 rounded-md border bg-muted/40 p-3 font-mono text-xs">
        <div class="flex justify-between gap-2">
          <span class="text-muted-foreground">密钥类型</span>
          <span>{{ prompt.error.key_type }}</span>
        </div>
        <div class="flex justify-between gap-2">
          <span class="text-muted-foreground">SHA256 指纹</span>
          <span class="break-all text-right">{{ prompt.error.fingerprint }}</span>
        </div>
      </div>
      <p class="text-xs text-muted-foreground">
        请带外核对指纹 (如服务器上执行 <code>ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub</code>)。
      </p>
      <p v-if="error" class="text-xs text-destructive">{{ error }}</p>
      <DialogFooter>
        <Button variant="outline" :disabled="busy" @click="cancelPrompt">取消</Button>
        <Button :disabled="busy" @click="trust">信任并连接</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>

  <!-- 硬告警: 密钥变更 / 吊销 / CA — 只展示, 不提供绕过 -->
  <Dialog :open="!!alert" @update:open="(v) => !v && (store.hostKeyAlert = null)">
    <DialogContent v-if="alert" class="sm:max-w-md">
      <DialogHeader>
        <DialogTitle class="flex items-center gap-2 text-destructive">
          <ShieldAlert v-if="alert.kind === 'host_key_mismatch'" class="h-5 w-5" />
          <OctagonX v-else class="h-5 w-5" />
          {{ alertTitle }}
        </DialogTitle>
        <DialogDescription>{{ alert.message }}</DialogDescription>
      </DialogHeader>
      <div class="space-y-2 rounded-md border border-destructive/40 bg-destructive/10 p-3 font-mono text-xs">
        <div class="flex justify-between gap-2">
          <span class="text-muted-foreground">主机</span>
          <span>{{ alert.host }}:{{ alert.port }}</span>
        </div>
        <div v-if="alert.kind !== 'unsupported_cert_authority'" class="flex justify-between gap-2">
          <span class="text-muted-foreground">实际呈现</span>
          <span class="break-all text-right">{{ alert.fingerprint }}</span>
        </div>
        <template v-if="alert.kind === 'host_key_mismatch'">
          <div v-for="s in alert.stored" :key="s.fingerprint" class="flex justify-between gap-2">
            <span class="text-muted-foreground">已记录 ({{ s.key_type }})</span>
            <span class="break-all text-right">{{ s.fingerprint }}</span>
          </div>
        </template>
      </div>
      <p class="text-xs text-muted-foreground">
        <template v-if="alert.kind === 'host_key_mismatch'">
          可能遭受中间人攻击, 也可能是服务器重装了系统。确认无误后,
          手工删除 <code>~/.config/oneshell/known_hosts</code> 中该主机的条目再重连。
        </template>
        <template v-else-if="alert.kind === 'host_key_revoked'">
          该密钥在 known_hosts 中被标记为 @revoked。移除该条目前请确认吊销原因已排除。
        </template>
        <template v-else>
          该主机使用证书机构 (CA) 签发的密钥, 当前版本尚不支持证书校验, 因此拒绝连接而不是降级为手动信任。
        </template>
      </p>
      <DialogFooter>
        <Button variant="outline" @click="store.hostKeyAlert = null">知道了</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>
