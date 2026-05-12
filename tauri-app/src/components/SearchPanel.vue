<script setup lang="ts">
import type { FileResult } from "../types/app"

defineProps<{
  searchPath: string
  searchMode: "filename" | "text"
  searchPattern: string
  searchIgnoreCase: boolean
  searchFixed: boolean
  searchRunning: boolean
  searchStatus: string
  searchResults: FileResult[]
  searchFilter: string
  filteredResults: FileResult[]
  cachedHighlight: (path: string, lineNum: number, line: string, ranges: [number, number][]) => { text: string; hl: boolean }[]
}>()

const emit = defineEmits<{
  pickPath: []
  search: []
  stop: []
  filterInput: [value: string]
  reveal: [path: string]
  update: [field: string, value: string | boolean]
}>()
</script>

<template>
  <div class="flex-1 flex flex-col gap-3 min-h-0">
    <div class="flex items-center gap-2 flex-shrink-0 flex-wrap">
      <div class="flex items-center gap-1 flex-shrink-0">
        <input :value="searchPath" @input="emit('update', 'searchPath', ($event.target as HTMLInputElement).value)" placeholder="搜索路径" :title="searchPath"
          class="w-44 rounded-lg px-2 py-1.5 text-xs focus:outline-none transition-colors"
          style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)" />
        <button @click="emit('pickPath')"
          class="px-2 py-1.5 rounded-lg text-xs transition-colors"
          style="background:var(--bg-muted);color:var(--text-secondary)" title="选择目录">📂</button>
      </div>
      <select :value="searchMode" @change="emit('update', 'searchMode', ($event.target as HTMLSelectElement).value)"
        class="rounded-lg px-2 py-1.5 text-xs focus:outline-none flex-shrink-0"
        style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)">
        <option value="filename">🗂 文件名</option>
        <option value="text">📄 文本</option>
      </select>
      <input :value="searchPattern" @input="emit('update', 'searchPattern', ($event.target as HTMLInputElement).value)" @keyup.enter="emit('search')" placeholder="搜索内容…"
        class="flex-1 min-w-[120px] rounded-lg px-3 py-1.5 text-sm focus:outline-none transition-colors"
        style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)" />
      <label class="flex items-center gap-1 text-xs cursor-pointer flex-shrink-0" style="color:var(--text-secondary)">
        <input type="checkbox" :checked="searchIgnoreCase" @change="emit('update', 'searchIgnoreCase', ($event.target as HTMLInputElement).checked)" class="accent-cyan-500" />忽略大小写
      </label>
      <label class="flex items-center gap-1 text-xs cursor-pointer flex-shrink-0" style="color:var(--text-secondary)">
        <input type="checkbox" :checked="searchFixed" @change="emit('update', 'searchFixed', ($event.target as HTMLInputElement).checked)" class="accent-cyan-500" />纯文本
      </label>
      <button v-if="!searchRunning" @click="emit('search')"
        class="px-3 py-1.5 rounded-lg text-xs font-medium transition-colors flex-shrink-0 text-white"
        style="background:var(--accent)">🔍 搜索</button>
      <button v-else @click="emit('stop')"
        class="px-3 py-1.5 rounded-lg text-xs transition-colors flex-shrink-0 text-white"
        style="background:#b91c1c">⏹ 取消</button>
    </div>

    <div class="flex items-center gap-2 flex-shrink-0">
      <span class="text-xs flex-1" style="color:var(--text-muted)">{{ searchStatus }}</span>
      <input v-if="searchResults.length > 0" :value="searchFilter"
        @input="emit('filterInput', ($event.target as HTMLInputElement).value)"
        placeholder="过滤结果…"
        class="w-36 rounded-lg px-2 py-1 text-xs focus:outline-none transition-colors"
        style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)" />
    </div>

    <div class="flex-1 overflow-y-auto space-y-1 pr-1 min-h-0">
      <div v-if="filteredResults.length === 0 && !searchRunning"
        class="text-center py-16 text-sm" style="color:var(--text-faint)">
        {{ searchResults.length === 0 ? '输入内容后按回车搜索' : '无匹配结果' }}
      </div>
      <div v-for="r in filteredResults" :key="r.path"
        class="rounded-xl p-3 group"
        style="background:var(--bg-card);box-shadow:0 0 0 1px var(--border)">
        <div class="flex items-center gap-2 mb-1">
          <span class="flex-shrink-0">{{ r.icon }}</span>
          <button @click="emit('reveal', r.path)"
            class="text-xs font-mono truncate flex-1 text-left hover:underline transition-colors"
            style="color:var(--accent)" :title="r.path">{{ r.path }}</button>
          <span class="text-xs flex-shrink-0" style="color:var(--text-faint)">{{ r.matches.length }} 处</span>
          <button @click="emit('reveal', r.path)"
            class="opacity-0 group-hover:opacity-100 transition-opacity flex-shrink-0 text-xs"
            style="color:var(--accent)" title="打开所在目录">📂</button>
        </div>
        <div v-if="searchMode === 'filename'" class="font-mono text-xs mt-0.5">
          <template v-for="seg in cachedHighlight(r.path, r.matches[0].line_num, r.matches[0].line, r.matches[0].ranges)" :key="seg.text">
            <span :style="seg.hl ? 'background:rgba(250,204,21,0.25);color:#fde68a;border-radius:2px;padding:0 2px' : `color:var(--text-secondary)`">{{ seg.text }}</span>
          </template>
        </div>
        <div v-else class="space-y-0.5 mt-1">
          <div v-for="(m, mi) in r.matches.slice(0, 5)" :key="mi" class="flex gap-2 font-mono text-xs">
            <span class="w-8 text-right flex-shrink-0" style="color:#4ade80">{{ m.line_num }}:</span>
            <span class="truncate">
              <template v-for="seg in cachedHighlight(r.path, m.line_num, m.line, m.ranges)" :key="seg.text">
                <span :style="seg.hl ? 'background:rgba(250,204,21,0.25);color:#fde68a;border-radius:2px;padding:0 2px' : `color:var(--text-secondary)`">{{ seg.text }}</span>
              </template>
            </span>
          </div>
          <div v-if="r.matches.length > 5" class="text-xs pl-10" style="color:var(--text-faint)">…另外 {{ r.matches.length - 5 }} 处</div>
        </div>
      </div>
    </div>
  </div>
</template>
