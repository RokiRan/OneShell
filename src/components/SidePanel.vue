<script setup lang="ts">
import { computed } from "vue";
import { store } from "@/lib/store";
import SftpPanel from "./SftpPanel.vue";
import MonitorPanel from "./MonitorPanel.vue";
import ForwardPanel from "./ForwardPanel.vue";
import AiPanel from "./AiPanel.vue";

const sessionId = computed(() => store.activeSessionId());
</script>

<template>
  <div v-if="sessionId" class="flex w-80 shrink-0 flex-col border-l bg-card">
    <SftpPanel v-if="store.sidePanel === 'sftp'" :session-id="sessionId" />
    <MonitorPanel v-else-if="store.sidePanel === 'monitor'" :session-id="sessionId" />
    <ForwardPanel v-else-if="store.sidePanel === 'forward'" :session-id="sessionId" />
    <AiPanel v-else-if="store.sidePanel === 'ai'" :session-id="sessionId" />
  </div>
</template>
