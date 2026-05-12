<script setup lang="ts">
import type { SyncConfig, SyncStatus } from "../types/app"

defineProps<{
  syncConfig: SyncConfig
  syncStatus: SyncStatus
  syncExcludeInput: string
  syncLog: string[]
}>()

const emit = defineEmits<{
  pickSrc: []
  pickDst: []
  saveAndSync: []
  toggleWatch: []
  addExclude: []
  removeExclude: [index: number]
  updateConfig: [field: string, value: string | boolean]
  updateExcludeInput: [value: string]
}>()
</script>

<template>
  <div class="flex-1 flex flex-col gap-4 min-h-0 max-w-xl mx-auto w-full">
    <div class="flex items-center gap-3 rounded-xl px-4 py-2 text-xs flex-shrink-0"
      style="background:var(--bg-card)">
      <span :style="syncStatus.is_running ? 'color:#facc15' : 'color:#4ade80'"
        :class="syncStatus.is_running ? 'animate-pulse' : ''">
        {{ syncStatus.is_running ? '⏳ 同步中…' : '✅ 空闲' }}
      </span>
      <span style="color:var(--text-faint)">上次: {{ syncStatus.last_sync ?? '从未同步' }}</span>
      <span style="color:var(--text-faint)">共 {{ syncStatus.total_files }} 个文件 / {{ syncStatus.total_bytes }}</span>
      <span v-if="syncStatus.is_watching" class="ml-auto" style="color:var(--accent)">👁 监听中</span>
    </div>

    <div class="space-y-3 flex-shrink-0">
      <div class="flex gap-2 items-center">
        <span class="text-xs w-8 flex-shrink-0" style="color:var(--text-muted)">源</span>
        <input :value="syncConfig.src" @input="emit('updateConfig', 'src', ($event.target as HTMLInputElement).value)" placeholder="源目录路径" :title="syncConfig.src"
          class="flex-1 rounded-lg px-3 py-1.5 text-sm focus:outline-none transition-colors"
          style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)" />
        <button @click="emit('pickSrc')"
          class="px-2 py-1.5 rounded-lg text-xs transition-colors flex-shrink-0"
          style="background:var(--bg-muted);color:var(--text-secondary)">📂</button>
      </div>
      <div class="flex gap-2 items-center">
        <span class="text-xs w-8 flex-shrink-0" style="color:var(--text-muted)">目标</span>
        <input :value="syncConfig.dst" @input="emit('updateConfig', 'dst', ($event.target as HTMLInputElement).value)" placeholder="目标目录路径" :title="syncConfig.dst"
          class="flex-1 rounded-lg px-3 py-1.5 text-sm focus:outline-none transition-colors"
          style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)" />
        <button @click="emit('pickDst')"
          class="px-2 py-1.5 rounded-lg text-xs transition-colors flex-shrink-0"
          style="background:var(--bg-muted);color:var(--text-secondary)">📂</button>
      </div>
      <label class="flex items-center gap-2 text-xs cursor-pointer" style="color:var(--text-secondary)">
        <input type="checkbox" :checked="syncConfig.delete_removed" @change="emit('updateConfig', 'delete_removed', ($event.target as HTMLInputElement).checked)" class="accent-cyan-500" />删除已移除的文件
      </label>
      <div class="space-y-1">
        <p class="text-xs" style="color:var(--text-muted)">排除规则</p>
        <div class="flex gap-2">
          <input :value="syncExcludeInput" @input="emit('updateExcludeInput', ($event.target as HTMLInputElement).value)" @keyup.enter="emit('addExclude')" placeholder="*.tmp 或 node_modules"
            class="flex-1 rounded-lg px-3 py-1 text-xs focus:outline-none transition-colors"
            style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)" />
          <button @click="emit('addExclude')"
            class="px-2 py-1 rounded text-xs transition-colors"
            style="background:var(--bg-muted);color:var(--text-secondary)">+</button>
        </div>
        <div class="flex flex-wrap gap-1 mt-1">
          <span v-for="(ex, i) in syncConfig.excludes" :key="i"
            class="flex items-center gap-1 text-xs px-2 py-0.5 rounded-full"
            style="background:var(--bg-muted);color:var(--text-secondary)">
            {{ ex }}
            <button @click="emit('removeExclude', i)" class="leading-none" style="color:var(--text-muted)">×</button>
          </span>
        </div>
      </div>
    </div>

    <div class="flex gap-2 flex-shrink-0">
      <button @click="emit('saveAndSync')" :disabled="syncStatus.is_running"
        :style="syncStatus.is_running
          ? `background:var(--bg-muted);color:var(--text-faint);cursor:not-allowed`
          : `background:var(--accent);color:#fff`"
        class="flex-1 py-2 rounded-lg text-sm font-medium transition-colors">
        {{ syncStatus.is_running ? '同步中…' : '🔄 立即同步' }}
      </button>
      <button @click="emit('toggleWatch')"
        :style="syncStatus.is_watching
          ? 'background:#92400e;color:#fff'
          : `background:var(--bg-muted);color:var(--text-secondary)`"
        class="px-4 py-2 rounded-lg text-sm transition-colors">
        {{ syncStatus.is_watching ? '⏹ 停止监听' : '👁 实时监听' }}
      </button>
    </div>

    <div class="flex-1 overflow-y-auto rounded-xl p-3 font-mono text-xs space-y-0.5 min-h-0"
      style="background:var(--bg-card)">
      <div v-if="syncLog.length === 0" class="text-center py-4" style="color:var(--text-faint)">日志将在此显示</div>
      <div v-for="(line, i) in syncLog" :key="i"
        :style="line.startsWith('❌') ? 'color:#f87171' : line.startsWith('🗑') ? 'color:#facc15' : `color:var(--text-secondary)`"
        class="leading-5">
        {{ line }}
      </div>
    </div>
  </div>
</template>
