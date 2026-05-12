<script setup lang="ts">
import { computed } from "vue"
import type { Device, SyncConfig, SyncProgressView, SyncStatus } from "../types/app"

const props = defineProps<{
  syncConfig: SyncConfig
  syncStatus: SyncStatus
  syncProgress: SyncProgressView
  syncExcludeInput: string
  syncLog: string[]
  syncErrors: string[]
  devices: Device[]
  shortName: (name: string) => string
}>()

const progressPct = computed(() => {
  if (!props.syncProgress.total) return 0
  return Math.max(0, Math.min(100, Math.round((props.syncProgress.current / props.syncProgress.total) * 100)))
})

const toneStyle = computed(() => {
  if (props.syncProgress.tone === "error") return "color:#f87171"
  if (props.syncProgress.tone === "done") return "color:#4ade80"
  if (props.syncProgress.tone === "running") return "color:#facc15"
  return "color:var(--accent)"
})

const emit = defineEmits<{
  pickSrc: []
  pickDst: []
  saveAndSync: []
  remoteSync: []
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
      <span v-if="syncStatus.is_watching" class="ml-auto" style="color:var(--accent)">⟳ 自动同步已开启</span>
    </div>

    <div class="rounded-xl px-4 py-3 space-y-2 text-xs flex-shrink-0"
      style="background:var(--bg-card)">
      <div class="flex items-center justify-between gap-3">
        <span style="color:var(--text-muted)">当前阶段</span>
        <span class="font-medium" :style="toneStyle">{{ syncProgress.detail }}</span>
      </div>
      <div class="flex items-center justify-between gap-3">
        <span style="color:var(--text-muted)">阶段标识</span>
        <span class="uppercase tracking-wide" style="color:var(--accent)">{{ syncProgress.phase }}</span>
      </div>
      <div class="space-y-1 pt-1">
        <div class="flex items-center justify-between">
          <span style="color:var(--text-muted)">动作进度</span>
          <span style="color:var(--text-primary)">{{ syncProgress.current }} / {{ syncProgress.total }}</span>
        </div>
        <div class="w-full h-2 rounded-full overflow-hidden" style="background:var(--bg-input)">
          <div class="h-2 rounded-full transition-all duration-200" :style="`width:${progressPct}%;background:var(--accent)`"></div>
        </div>
      </div>
      <div class="grid grid-cols-3 gap-2 pt-1">
        <div class="rounded-lg px-3 py-2 text-center" style="background:var(--bg-input);border:1px solid var(--border-input)">
          <div class="text-[10px]" style="color:var(--text-faint)">推送</div>
          <div class="font-medium" style="color:var(--text-primary)">{{ syncProgress.push_count }}</div>
        </div>
        <div class="rounded-lg px-3 py-2 text-center" style="background:var(--bg-input);border:1px solid var(--border-input)">
          <div class="text-[10px]" style="color:var(--text-faint)">拉取</div>
          <div class="font-medium" style="color:var(--text-primary)">{{ syncProgress.pull_count }}</div>
        </div>
        <div class="rounded-lg px-3 py-2 text-center" style="background:var(--bg-input);border:1px solid var(--border-input)">
          <div class="text-[10px]" style="color:var(--text-faint)">删除</div>
          <div class="font-medium" style="color:var(--text-primary)">{{ syncProgress.delete_count }}</div>
        </div>
      </div>
      <div v-if="syncProgress.stats.length > 0" class="grid grid-cols-1 sm:grid-cols-2 gap-2 pt-1">
        <div v-for="stat in syncProgress.stats" :key="stat.label"
          class="rounded-lg px-3 py-2"
          style="background:var(--bg-input);border:1px solid var(--border-input)">
          <div class="font-medium mb-1" style="color:var(--text-primary)">{{ stat.label }}</div>
          <div style="color:var(--text-faint)">扫描 {{ stat.scanned_files }}</div>
          <div style="color:var(--text-faint)">复用缓存 {{ stat.cached_files }}</div>
          <div style="color:var(--text-faint)">重算哈希 {{ stat.hashed_files }}</div>
        </div>
      </div>
      <div v-if="syncErrors.length > 0" class="rounded-lg px-3 py-2 space-y-1"
        style="background:rgba(248,113,113,0.08);border:1px solid rgba(248,113,113,0.2)">
        <div class="font-medium" style="color:#f87171">最近错误</div>
        <div v-for="(line, i) in syncErrors.slice(0, 3)" :key="i" class="leading-5" style="color:#fca5a5">{{ line }}</div>
      </div>
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
        <span class="text-xs w-8 flex-shrink-0" style="color:var(--text-muted)">本地</span>
        <input :value="syncConfig.dst" @input="emit('updateConfig', 'dst', ($event.target as HTMLInputElement).value)" placeholder="本地镜像目标目录" :title="syncConfig.dst"
          class="flex-1 rounded-lg px-3 py-1.5 text-sm focus:outline-none transition-colors"
          style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)" />
        <button @click="emit('pickDst')"
          class="px-2 py-1.5 rounded-lg text-xs transition-colors flex-shrink-0"
          style="background:var(--bg-muted);color:var(--text-secondary)">📂</button>
      </div>
      <div class="flex gap-2 items-center">
        <span class="text-xs w-8 flex-shrink-0" style="color:var(--text-muted)">远端</span>
        <input :value="syncConfig.remote_addr" @input="emit('updateConfig', 'remote_addr', ($event.target as HTMLInputElement).value)" placeholder="远端设备地址，例如 192.168.1.5:49821" :title="syncConfig.remote_addr"
          class="flex-1 rounded-lg px-3 py-1.5 text-sm focus:outline-none transition-colors"
          style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)" />
      </div>
      <div v-if="devices.length > 0" class="flex gap-2 items-center">
        <span class="text-xs w-8 flex-shrink-0" style="color:var(--text-muted)">设备</span>
        <select @change="emit('updateConfig', 'remote_addr', ($event.target as HTMLSelectElement).value)"
          class="flex-1 rounded-lg px-3 py-1.5 text-sm focus:outline-none transition-colors"
          style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)">
          <option value="">选择已发现设备</option>
          <option v-for="dev in devices.filter(d => !!d.addr)" :key="dev.name" :value="dev.addr">
            {{ shortName(dev.name) }} · {{ dev.addr }}
          </option>
        </select>
      </div>
      <label class="flex items-center gap-2 text-xs cursor-pointer" style="color:var(--text-secondary)">
        <input type="checkbox" :checked="syncConfig.delete_removed" @change="emit('updateConfig', 'delete_removed', ($event.target as HTMLInputElement).checked)" class="accent-cyan-500" />删除已移除的文件
      </label>
      <p class="text-[11px]" style="color:var(--text-faint)">
        “开启自动同步”仅用于本机把“源目录”实时镜像到“本地镜像目标目录”，不负责双机持续同步。
      </p>
      <p class="text-[11px]" style="color:var(--text-faint)">
        双机目录同步：两台机器都先选择各自的“源目录”，再在任意一台填写对端地址并点击“首次完全同步”。
      </p>
      <p class="text-[11px]" style="color:var(--text-faint)">
        完全同步默认以最新修改为准。开启删除同步后，较新的删除操作也会同步到另一台设备。
      </p>
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
        {{ syncStatus.is_running ? '同步中…' : '🔄 同步到本地镜像' }}
      </button>
      <button @click="emit('remoteSync')" :disabled="syncStatus.is_running || !syncConfig.remote_addr"
        :style="syncStatus.is_running || !syncConfig.remote_addr
          ? 'background:var(--bg-muted);color:var(--text-faint);cursor:not-allowed'
          : 'background:#2563eb;color:#fff'"
        class="px-4 py-2 rounded-lg text-sm transition-colors">
        首次完全同步
      </button>
      <button @click="emit('toggleWatch')"
        :style="syncStatus.is_watching
          ? 'background:#92400e;color:#fff'
          : `background:var(--bg-muted);color:var(--text-secondary)`"
        class="px-4 py-2 rounded-lg text-sm transition-colors">
        {{ syncStatus.is_watching ? '⏹ 关闭本地自动镜像' : '⟳ 开启本地自动镜像' }}
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
