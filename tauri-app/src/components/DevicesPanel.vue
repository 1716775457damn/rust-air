<script setup lang="ts">
import type { Device } from "../types/app"

defineProps<{
  scanning: boolean
  clipSyncEnabled: boolean
  devices: Device[]
  shortName: (name: string) => string
  fmtLastSeen: (ts?: number) => string
  isPeerInSyncGroup: (deviceName: string) => boolean
}>()

const emit = defineEmits<{
  scan: []
  toggleClipSync: []
  toggleSyncPeer: [device: Device]
}>()
</script>

<template>
  <div class="flex-1 flex flex-col gap-4 max-w-lg mx-auto w-full">
    <div class="flex items-center justify-between">
      <h2 class="font-medium text-sm" style="color:var(--text-secondary)">局域网设备</h2>
      <button @click="emit('scan')"
        :style="scanning ? `color:var(--accent);background:var(--bg-muted)` : `background:var(--bg-muted);color:var(--text-secondary)`"
        :class="['px-3 py-1.5 rounded-lg text-xs transition-colors', scanning ? 'animate-pulse' : '']">
        {{ scanning ? '扫描中…' : '🔄 扫描' }}
      </button>
    </div>

    <div class="rounded-xl p-3 flex items-center justify-between"
      style="background:var(--bg-card);box-shadow:0 0 0 1px var(--border)">
      <div class="flex items-center gap-2">
        <span class="text-sm">📋</span>
        <span class="text-xs" style="color:var(--text-secondary)">共享剪贴板</span>
      </div>
      <label class="relative inline-flex items-center cursor-pointer">
        <input type="checkbox" :checked="clipSyncEnabled" @change="emit('toggleClipSync')" class="sr-only peer" />
        <div class="w-9 h-5 rounded-full peer transition-colors duration-200"
          :style="clipSyncEnabled ? 'background:var(--accent)' : 'background:var(--border-input)'">
          <div class="absolute top-0.5 left-0.5 w-4 h-4 rounded-full transition-transform duration-200 bg-white"
            :style="clipSyncEnabled ? 'transform:translateX(16px)' : ''"></div>
        </div>
      </label>
    </div>

    <div v-if="devices.length === 0" class="text-center py-12 text-sm" style="color:var(--text-faint)">未发现设备 — 点击扫描</div>
    <div v-for="dev in devices" :key="dev.name"
      class="rounded-xl p-3.5 flex items-center gap-4"
      style="background:var(--bg-card);box-shadow:0 0 0 1px var(--border)">
      <div class="w-3 h-3 rounded-full flex-shrink-0" style="background:#4ade80"></div>
      <div class="flex-1 min-w-0">
        <div class="flex items-center gap-1.5">
          <p class="text-sm font-medium truncate" style="color:var(--text-primary)">{{ shortName(dev.name) }}</p>
          <span v-if="isPeerInSyncGroup(dev.name)" class="text-xs" title="已共享剪贴板">📋</span>
        </div>
        <p class="text-xs" style="color:var(--text-muted)">{{ dev.addr }}
          <span v-if="dev.lastSeen" class="ml-1" style="color:var(--text-faint)">· {{ fmtLastSeen(dev.lastSeen) }}</span>
        </p>
      </div>
      <button @click.stop="emit('toggleSyncPeer', dev)"
        :title="isPeerInSyncGroup(dev.name) ? '取消共享剪贴板' : '共享剪贴板'"
        :style="isPeerInSyncGroup(dev.name)
          ? 'background:var(--accent-bg);color:var(--accent);box-shadow:0 0 0 1px var(--accent-ring)'
          : 'background:var(--bg-muted);color:var(--text-muted)'"
        class="px-2.5 py-1.5 rounded-lg text-xs transition-all flex-shrink-0">
        {{ isPeerInSyncGroup(dev.name) ? '📋 已共享' : '📋 共享' }}
      </button>
    </div>
  </div>
</template>
