<script setup lang="ts">
import type { WhiteboardItem } from "../types/app"

defineProps<{
  whiteboardItems: WhiteboardItem[]
  wbTextInput: string
  wbClearConfirm: boolean
  wbEditingId: string | null
  wbEditingText: string
  fmtWbTime: (ts: number) => string
}>()

const emit = defineEmits<{
  addText: []
  clearConfirm: [value: boolean]
  clear: []
  deleteItem: [id: string]
  pasteImage: [event: ClipboardEvent]
  editStart: [item: WhiteboardItem]
  editCancel: []
  editSave: []
  editInput: [value: string]
  textInput: [value: string]
  copyText: [text: string]
}>()
</script>

<template>
  <div class="flex-1 flex flex-col gap-4 min-h-0 max-w-xl mx-auto w-full">
    <div class="flex gap-2 flex-shrink-0">
      <input :value="wbTextInput" @input="emit('textInput', ($event.target as HTMLInputElement).value)" @keyup.enter="emit('addText')" @paste="emit('pasteImage', $event as ClipboardEvent)"
        placeholder="输入文字或粘贴图片…"
        class="flex-1 rounded-lg px-3 py-2 text-sm focus:outline-none transition-colors"
        style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)" />
      <button @click="emit('addText')"
        :disabled="!wbTextInput.trim()"
        :style="wbTextInput.trim()
          ? 'background:var(--accent);color:#fff'
          : 'background:var(--bg-muted);color:var(--text-faint);cursor:not-allowed'"
        class="px-4 py-2 rounded-lg text-sm font-medium transition-colors flex-shrink-0">
        添加
      </button>
    </div>

    <div class="flex items-center justify-between flex-shrink-0">
      <span class="text-xs" style="color:var(--text-muted)">共 {{ whiteboardItems.length }} 条</span>
      <div v-if="whiteboardItems.length > 0" class="flex items-center gap-2">
        <template v-if="!wbClearConfirm">
          <button @click="emit('clearConfirm', true)"
            class="px-3 py-1 rounded-lg text-xs transition-colors"
            style="background:rgba(239,68,68,0.1);color:#f87171">
            🗑 清空白板
          </button>
        </template>
        <template v-else>
          <span class="text-xs" style="color:#f87171">确定清空？此操作将同步到所有设备</span>
          <button @click="emit('clear')"
            class="px-3 py-1 rounded-lg text-xs font-medium text-white"
            style="background:#dc2626">确定</button>
          <button @click="emit('clearConfirm', false)"
            class="px-2 py-1 rounded-lg text-xs"
            style="color:var(--text-muted)">取消</button>
        </template>
      </div>
    </div>

    <div class="flex-1 min-h-0 overflow-y-auto space-y-2">
      <div v-if="whiteboardItems.length === 0" class="text-center py-12 text-sm" style="color:var(--text-faint)">
        <div class="text-3xl mb-2">🖊️</div>
        <p>白板为空</p>
        <p class="text-xs mt-1">输入文字或粘贴图片开始使用</p>
      </div>
      <div v-for="item in whiteboardItems" :key="item.id"
        class="rounded-xl p-3.5 group"
        style="background:var(--bg-card);box-shadow:0 0 0 1px var(--border)">
        <div class="flex items-start gap-3">
          <span class="flex-shrink-0 text-sm mt-0.5">{{ item.content_type === 'Image' ? '🖼️' : '📝' }}</span>
          <div class="flex-1 min-w-0">
            <div v-if="item.content_type === 'Text' && item.text" class="space-y-2">
              <textarea v-if="wbEditingId === item.id"
                :value="wbEditingText"
                @input="emit('editInput', ($event.target as HTMLTextAreaElement).value)"
                @keydown.ctrl.enter.prevent="emit('editSave')"
                @keydown.esc.prevent="emit('editCancel')"
                class="w-full min-h-24 rounded-lg px-3 py-2 text-sm focus:outline-none resize-y"
                style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)"></textarea>
              <div v-else class="space-y-2">
                <p @dblclick="emit('editStart', item)"
                  class="text-sm whitespace-pre-wrap break-words cursor-text select-text"
                  title="双击可编辑，文本可直接选择复制"
                  style="color:var(--text-primary);user-select:text">{{ item.text }}</p>
                <div class="flex items-center gap-2">
                  <button @click="emit('copyText', item.text)"
                    class="px-3 py-1 rounded-lg text-xs transition-colors"
                    style="background:var(--bg-muted);color:var(--text-secondary)">复制</button>
                  <button @click="emit('editStart', item)"
                    class="px-3 py-1 rounded-lg text-xs transition-colors"
                    style="background:var(--accent-bg);color:var(--accent)">编辑</button>
                </div>
              </div>
              <div v-if="wbEditingId === item.id" class="flex items-center gap-2">
                <button @click="emit('editSave')"
                  :disabled="!wbEditingText.trim()"
                  :style="wbEditingText.trim()
                    ? 'background:var(--accent);color:#fff'
                    : 'background:var(--bg-muted);color:var(--text-faint);cursor:not-allowed'"
                  class="px-3 py-1 rounded-lg text-xs font-medium transition-colors">保存</button>
                <button @click="emit('editCancel')"
                  class="px-3 py-1 rounded-lg text-xs transition-colors"
                  style="background:var(--bg-muted);color:var(--text-secondary)">取消</button>
                <span class="text-[11px]" style="color:var(--text-faint)">Ctrl+Enter 保存，Esc 取消</span>
              </div>
            </div>
            <img v-if="item.content_type === 'Image' && item.image_b64"
              :src="'data:image/png;base64,' + item.image_b64"
              class="max-w-full max-h-48 rounded-lg object-contain"
              style="background:var(--bg-muted)" />
            <div class="flex items-center gap-2 mt-1.5 text-[11px]" style="color:var(--text-faint)">
              <span>{{ fmtWbTime(item.timestamp) }}</span>
              <span v-if="item.source_device" class="px-1.5 py-0.5 rounded-full"
                style="background:var(--accent-bg);color:var(--accent)">
                {{ item.source_device }}
              </span>
            </div>
          </div>
          <button @click="emit('deleteItem', item.id)"
            class="text-xs flex-shrink-0 opacity-0 group-hover:opacity-100 transition-opacity p-1 rounded-lg"
            style="color:var(--text-muted)"
            title="删除">🗑</button>
        </div>
      </div>
    </div>
  </div>
</template>
