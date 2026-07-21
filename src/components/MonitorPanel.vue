<script setup lang="ts">
import { onMounted, onBeforeUnmount, ref } from "vue";
import { api, formatBytes, formatUptime, type ServerStats } from "@/lib/api";
import { Progress } from "@/components/ui/progress";
import { Separator } from "@/components/ui/separator";

const props = defineProps<{ sessionId: string }>();

const stats = ref<ServerStats | null>(null);
const error = ref("");
let timer: ReturnType<typeof setInterval> | null = null;

async function poll() {
  try {
    stats.value = await api.serverStats(props.sessionId);
    error.value = "";
  } catch (e) {
    error.value = String(e);
  }
}

function pct(used: number, total: number): number {
  return total > 0 ? (used / total) * 100 : 0;
}

onMounted(() => {
  poll();
  timer = setInterval(poll, 3000);
});

onBeforeUnmount(() => {
  if (timer) clearInterval(timer);
});
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-3 text-xs">
    <div v-if="error" class="rounded bg-destructive/15 px-2 py-1 text-destructive">{{ error }}</div>
    <template v-if="stats">
      <div>
        <div class="font-medium">{{ stats.hostname }}</div>
        <div class="text-muted-foreground">{{ stats.os }} · {{ stats.cpu_cores }} 核 · 运行 {{ formatUptime(stats.uptime_secs) }}</div>
      </div>
      <Separator />
      <div>
        <div class="mb-1 flex justify-between">
          <span>CPU</span>
          <span>{{ stats.cpu_percent.toFixed(1) }}%</span>
        </div>
        <Progress :model-value="stats.cpu_percent" class="h-2" />
        <div class="mt-1 text-muted-foreground">
          负载 {{ stats.load1.toFixed(2) }} / {{ stats.load5.toFixed(2) }} / {{ stats.load15.toFixed(2) }}
        </div>
      </div>
      <div>
        <div class="mb-1 flex justify-between">
          <span>内存</span>
          <span>{{ formatBytes(stats.mem_used) }} / {{ formatBytes(stats.mem_total) }}</span>
        </div>
        <Progress :model-value="pct(stats.mem_used, stats.mem_total)" class="h-2" />
      </div>
      <div v-if="stats.swap_total > 0">
        <div class="mb-1 flex justify-between">
          <span>Swap</span>
          <span>{{ formatBytes(stats.swap_used) }} / {{ formatBytes(stats.swap_total) }}</span>
        </div>
        <Progress :model-value="pct(stats.swap_used, stats.swap_total)" class="h-2" />
      </div>
      <div>
        <div class="mb-1 flex justify-between">
          <span>磁盘 /</span>
          <span>{{ formatBytes(stats.disk_used) }} / {{ formatBytes(stats.disk_total) }}</span>
        </div>
        <Progress :model-value="pct(stats.disk_used, stats.disk_total)" class="h-2" />
      </div>
      <Separator />
      <div class="flex justify-between">
        <span class="text-muted-foreground">网络 ↓</span>
        <span>{{ formatBytes(stats.net_rx_bps) }}/s</span>
      </div>
      <div class="flex justify-between">
        <span class="text-muted-foreground">网络 ↑</span>
        <span>{{ formatBytes(stats.net_tx_bps) }}/s</span>
      </div>
    </template>
    <div v-else-if="!error" class="py-8 text-center text-muted-foreground">采集中…</div>
  </div>
</template>
