<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

// ── Types ─────────────────────────────────────────────────────────────────────

interface Device { name: string; addr: string; status: "Idle" | "Busy" }
interface TransferEvent { bytes_done: number; total_bytes: number; bytes_per_sec: number; done: boolean; error?: string }
interface SendSession { instance_name: string; key_b64: string }
interface ClipEntryView {
  id: number; kind: "text" | "image"; preview: string; stats: string;
  time_str: string; pinned: boolean; char_count: number; image_b64?: string;
}

type Tab   = "send" | "receive" | "devices" | "history";
type Phase = "idle" | "waiting" | "transferring" | "done" | "error";

// ── State ─────────────────────────────────────────────────────────────────────

const tab   = ref<Tab>("send");
const phase = ref<Phase>("idle");
const errorMsg = ref("");

// Send
const dragOver     = ref(false);
const selectedPath = ref("");
const session      = ref<SendSession | null>(null);

// Receive
const receiveInput = ref("");
const outDir       = ref(".");

// Progress
const progress      = ref<TransferEvent>({ bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false });
const connectedPeer = ref("");

// Devices
const devices = ref<Device[]>([]);

// Clipboard history
const historyEntries = ref<ClipEntryView[]>([]);
const historyQuery   = ref("");
const historyPaused  = ref(false);
let   tickTimer: ReturnType<typeof setInterval> | null = null;

// Unlisten handles
const unlisten = ref<UnlistenFn[]>([]);

// ── Computed ──────────────────────────────────────────────────────────────────

const pct = computed(() => {
  if (!progress.value.total_bytes) return null;
  return Math.min(100, Math.round((progress.value.bytes_done / progress.value.total_bytes) * 100));
});
const speed = computed(() => {
  const bps = progress.value.bytes_per_sec;
  if (bps > 1_000_000) return `${(bps / 1_000_000).toFixed(1)} MB/s`;
  if (bps > 1_000)     return `${(bps / 1_000).toFixed(0)} KB/s`;
  return `${bps} B/s`;
});
const shareCode = computed(() =>
  session.value ? `${session.value.instance_name}:${session.value.key_b64}` : ""
);
const pinnedEntries = computed(() => historyEntries.value.filter(e => e.pinned));
const recentEntries = computed(() => historyEntries.value.filter(e => !e.pinned));

// ── Lifecycle ─────────────────────────────────────────────────────────────────

onMounted(async () => {
  unlisten.value.push(
    await listen<TransferEvent>("transfer-progress", (e) => {
      progress.value = e.payload;
      if (phase.value === "waiting") phase.value = "transferring";
    }),
    await listen<string>("transfer-peer-connected", (e) => {
      connectedPeer.value = e.payload;
      phase.value = "transferring";
    }),
    await listen("transfer-done", () => { phase.value = "done"; }),
    await listen<string>("transfer-error", (e) => { errorMsg.value = e.payload; phase.value = "error"; }),
    await listen<Device>("device-found", (e) => {
      const dev = e.payload;
      const idx = devices.value.findIndex(d => d.name === dev.name);
      if (!dev.addr) { if (idx >= 0) devices.value.splice(idx, 1); }
      else if (idx >= 0) { devices.value[idx] = dev; }
      else { devices.value.push(dev); }
    }),
  );

  // Start clipboard history polling
  await tickHistory();
  tickTimer = setInterval(tickHistory, 500);
});

onUnmounted(async () => {
  unlisten.value.forEach(fn => fn());
  if (tickTimer) clearInterval(tickTimer);
  await invoke("flush_history").catch(() => {});
});

// ── Transfer actions ──────────────────────────────────────────────────────────

async function pickFile()   { const r = await open({ multiple: false, directory: false }); if (r) selectedPath.value = r as string; }
async function pickFolder() { const r = await open({ multiple: false, directory: true  }); if (r) selectedPath.value = r as string; }

function onDrop(e: DragEvent) {
  dragOver.value = false;
  const f = e.dataTransfer?.files[0];
  if (f) selectedPath.value = (f as any).path ?? f.name;
}

async function startSend() {
  if (!selectedPath.value) return;
  phase.value = "waiting"; errorMsg.value = "";
  progress.value = { bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false };
  try { session.value = await invoke<SendSession>("start_send", { path: selectedPath.value }); }
  catch (e: any) { errorMsg.value = String(e); phase.value = "error"; }
}

async function startReceive() {
  if (!receiveInput.value.trim()) return;
  const [instance_name, key_b64] = receiveInput.value.trim().split(":");
  if (!key_b64) { errorMsg.value = "Format: instance-name:key"; phase.value = "error"; return; }
  phase.value = "waiting"; errorMsg.value = "";
  progress.value = { bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false };
  try { await invoke("start_receive", { instanceName: instance_name, keyB64: key_b64, outDir: outDir.value }); }
  catch (e: any) { errorMsg.value = String(e); phase.value = "error"; }
}

function reset() {
  invoke("cancel_send").catch(() => {});
  phase.value = "idle"; session.value = null; connectedPeer.value = ""; errorMsg.value = "";
  progress.value = { bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false };
}

async function startScan() { devices.value = []; await invoke("scan_devices"); }

function copyCode() { navigator.clipboard.writeText(shareCode.value); }

// ── Clipboard history actions ─────────────────────────────────────────────────

async function tickHistory() {
  try {
    historyEntries.value = await invoke<ClipEntryView[]>("tick_history", { query: historyQuery.value });
  } catch { /* ignore */ }
}

async function copyEntry(id: number) {
  await invoke("copy_history_entry", { id }).catch(() => {});
  await tickHistory();
}

async function deleteEntry(id: number) {
  await invoke("delete_history_entry", { id });
  historyEntries.value = historyEntries.value.filter(e => e.id !== id);
}

async function togglePin(id: number) {
  await invoke("toggle_pin_entry", { id });
  await tickHistory();
}

async function clearHistory() {
  await invoke("clear_history");
  await tickHistory();
}

async function togglePause() {
  historyPaused.value = !historyPaused.value;
  await invoke("set_history_paused", { paused: historyPaused.value });
}

// ── Helpers ───────────────────────────────────────────────────────────────────

function fmtBytes(n: number) {
  if (n > 1e9) return `${(n / 1e9).toFixed(2)} GB`;
  if (n > 1e6) return `${(n / 1e6).toFixed(1)} MB`;
  if (n > 1e3) return `${(n / 1e3).toFixed(0)} KB`;
  return `${n} B`;
}
</script>

<template>
  <div class="min-h-screen bg-gray-950 text-gray-100 flex flex-col select-none font-sans">

    <!-- Header -->
    <header class="flex items-center gap-3 px-5 py-3 border-b border-gray-800">
      <span class="text-xl">✈️</span>
      <h1 class="text-base font-semibold tracking-tight">rust-air</h1>
      <div class="ml-auto flex gap-1">
        <button v-for="t in (['send','receive','devices','history'] as Tab[])" :key="t"
          @click="tab = t"
          :class="['px-3 py-1 rounded-md text-xs transition-colors',
            tab === t ? 'bg-cyan-600 text-white' : 'text-gray-400 hover:text-white hover:bg-gray-800']">
          {{ t === 'send' ? '📤 发送' : t === 'receive' ? '📥 接收' : t === 'devices' ? '🔍 设备' : '📋 剪贴板' }}
        </button>
      </div>
    </header>

    <!-- Main -->
    <main class="flex-1 flex flex-col p-4 gap-4 overflow-hidden">

      <!-- ── SEND TAB ── -->
      <template v-if="tab === 'send'">
        <div class="flex-1 flex flex-col items-center justify-center gap-4">
          <template v-if="phase === 'idle'">
            <div @dragover.prevent="dragOver = true" @dragleave="dragOver = false" @drop.prevent="onDrop"
              :class="['w-full max-w-lg border-2 border-dashed rounded-2xl p-10 text-center transition-colors cursor-pointer',
                dragOver ? 'border-cyan-400 bg-cyan-950/30' : 'border-gray-700 hover:border-gray-500']"
              @click="pickFile">
              <div class="text-5xl mb-3">📦</div>
              <p class="text-gray-300 font-medium">拖拽文件或文件夹到此处</p>
              <p class="text-gray-500 text-sm mt-1">或点击选择文件</p>
            </div>
            <div class="flex gap-3">
              <button @click="pickFile"   class="px-4 py-2 bg-gray-800 hover:bg-gray-700 rounded-lg text-sm transition-colors">📄 文件</button>
              <button @click="pickFolder" class="px-4 py-2 bg-gray-800 hover:bg-gray-700 rounded-lg text-sm transition-colors">📁 文件夹</button>
            </div>
            <div v-if="selectedPath" class="w-full max-w-lg bg-gray-900 rounded-xl p-4 flex items-center gap-3">
              <span class="text-cyan-400 text-xl">📎</span>
              <span class="text-sm text-gray-300 truncate flex-1">{{ selectedPath }}</span>
              <button @click="startSend" class="px-4 py-2 bg-cyan-600 hover:bg-cyan-500 rounded-lg text-sm font-medium transition-colors">发送 →</button>
            </div>
          </template>

          <template v-else-if="phase === 'waiting'">
            <div class="text-center space-y-4">
              <div class="text-4xl animate-pulse">⏳</div>
              <p class="text-gray-300 font-medium">等待接收方…</p>
              <div v-if="session" class="bg-gray-900 rounded-xl p-4 space-y-2 text-left w-80">
                <p class="text-xs text-gray-500 uppercase tracking-wider">分享码</p>
                <div class="flex items-center gap-2">
                  <code class="text-cyan-300 text-xs break-all flex-1">{{ shareCode }}</code>
                  <button @click="copyCode" class="text-gray-400 hover:text-white text-lg" title="复制">⎘</button>
                </div>
                <p class="text-xs text-gray-600">🔒 E2EE · SHA-256 校验</p>
              </div>
              <button @click="reset" class="text-sm text-gray-500 hover:text-gray-300">取消</button>
            </div>
          </template>

          <template v-else-if="phase === 'transferring'">
            <ProgressRing :pct="pct" :speed="speed" :done-bytes="progress.bytes_done" :total-bytes="progress.total_bytes" :peer="connectedPeer" />
          </template>

          <template v-else-if="phase === 'done'">
            <div class="text-center space-y-3">
              <div class="text-6xl">✅</div>
              <p class="text-gray-200 font-semibold text-lg">传输完成！</p>
              <p class="text-gray-500 text-sm">{{ fmtBytes(progress.bytes_done) }} 已发送</p>
              <button @click="reset" class="px-5 py-2 bg-cyan-600 hover:bg-cyan-500 rounded-lg text-sm font-medium transition-colors">再发一个</button>
            </div>
          </template>

          <template v-else-if="phase === 'error'">
            <div class="text-center space-y-3">
              <div class="text-5xl">❌</div>
              <p class="text-red-400 font-medium">{{ errorMsg }}</p>
              <button @click="reset" class="px-5 py-2 bg-gray-800 hover:bg-gray-700 rounded-lg text-sm transition-colors">重试</button>
            </div>
          </template>
        </div>
      </template>

      <!-- ── RECEIVE TAB ── -->
      <template v-else-if="tab === 'receive'">
        <div class="flex-1 flex flex-col items-center justify-center gap-4">
          <template v-if="phase === 'idle' || phase === 'error'">
            <div class="w-full max-w-md space-y-4">
              <h2 class="text-gray-300 font-medium">输入发送方的分享码</h2>
              <textarea v-model="receiveInput" rows="3" placeholder="rust-air-abc12345:base64key..."
                class="w-full bg-gray-900 border border-gray-700 rounded-xl p-3 text-sm text-cyan-300 font-mono resize-none focus:outline-none focus:border-cyan-500 transition-colors" />
              <div class="flex gap-3 items-center">
                <input v-model="outDir" placeholder="保存目录"
                  class="flex-1 bg-gray-900 border border-gray-700 rounded-lg px-3 py-2 text-sm focus:outline-none focus:border-cyan-500 transition-colors" />
                <button @click="startReceive" class="px-5 py-2 bg-cyan-600 hover:bg-cyan-500 rounded-lg text-sm font-medium transition-colors">接收 →</button>
              </div>
              <p v-if="phase === 'error'" class="text-red-400 text-sm">{{ errorMsg }}</p>
            </div>
          </template>
          <template v-else-if="phase === 'waiting'">
            <div class="text-center space-y-3"><div class="text-4xl animate-pulse">🔍</div><p class="text-gray-300">正在通过 mDNS 查找发送方…</p></div>
          </template>
          <template v-else-if="phase === 'transferring'">
            <ProgressRing :pct="pct" :speed="speed" :done-bytes="progress.bytes_done" :total-bytes="progress.total_bytes" :peer="connectedPeer" />
          </template>
          <template v-else-if="phase === 'done'">
            <div class="text-center space-y-3">
              <div class="text-6xl">✅</div>
              <p class="text-gray-200 font-semibold text-lg">接收完成！</p>
              <p class="text-gray-500 text-sm">{{ fmtBytes(progress.bytes_done) }} · SHA-256 已验证</p>
              <button @click="reset" class="px-5 py-2 bg-cyan-600 hover:bg-cyan-500 rounded-lg text-sm font-medium transition-colors">再接收一个</button>
            </div>
          </template>
        </div>
      </template>

      <!-- ── DEVICES TAB ── -->
      <template v-else-if="tab === 'devices'">
        <div class="flex-1 flex flex-col gap-4 max-w-lg mx-auto w-full">
          <div class="flex items-center justify-between">
            <h2 class="text-gray-300 font-medium">局域网设备</h2>
            <button @click="startScan" class="px-3 py-1.5 bg-gray-800 hover:bg-gray-700 rounded-lg text-sm transition-colors">🔄 扫描</button>
          </div>
          <div v-if="devices.length === 0" class="text-center text-gray-600 py-12">未发现设备 — 点击扫描</div>
          <div v-for="dev in devices" :key="dev.name"
            class="bg-gray-900 rounded-xl p-4 flex items-center gap-4 hover:bg-gray-800 transition-colors">
            <div :class="['w-3 h-3 rounded-full flex-shrink-0', dev.status === 'Idle' ? 'bg-green-400' : 'bg-yellow-400']" />
            <div class="flex-1 min-w-0">
              <p class="text-sm font-medium text-gray-200 truncate">{{ dev.name }}</p>
              <p class="text-xs text-gray-500">{{ dev.addr }}</p>
            </div>
            <span :class="['text-xs px-2 py-0.5 rounded-full', dev.status === 'Idle' ? 'bg-green-900/50 text-green-400' : 'bg-yellow-900/50 text-yellow-400']">
              {{ dev.status === 'Idle' ? '空闲' : '忙碌' }}
            </span>
          </div>
        </div>
      </template>

      <!-- ── CLIPBOARD HISTORY TAB ── -->
      <template v-else-if="tab === 'history'">
        <div class="flex-1 flex flex-col gap-3 min-h-0">

          <!-- Toolbar -->
          <div class="flex items-center gap-2 flex-shrink-0">
            <span class="text-gray-500">🔍</span>
            <input v-model="historyQuery" @input="tickHistory" placeholder="搜索历史…"
              class="flex-1 bg-gray-900 border border-gray-700 rounded-lg px-3 py-1.5 text-sm focus:outline-none focus:border-cyan-500 transition-colors" />
            <button v-if="historyQuery" @click="historyQuery = ''; tickHistory()"
              class="text-gray-500 hover:text-white text-sm px-1">✕</button>
            <button @click="togglePause"
              :class="['px-2 py-1 rounded text-xs transition-colors', historyPaused ? 'bg-yellow-700 text-yellow-200' : 'bg-gray-800 text-gray-400 hover:bg-gray-700']"
              :title="historyPaused ? '恢复记录' : '暂停记录'">
              {{ historyPaused ? '▶' : '⏸' }}
            </button>
            <button @click="clearHistory" class="px-2 py-1 bg-gray-800 hover:bg-gray-700 rounded text-xs text-gray-400 transition-colors" title="清除未固定条目">🗑</button>
          </div>

          <!-- Status bar -->
          <p :class="['text-xs flex-shrink-0', historyPaused ? 'text-yellow-500' : 'text-gray-600']">
            {{ historyEntries.length }} 条记录{{ historyPaused ? ' · 已暂停' : '' }}
          </p>

          <!-- Entry list -->
          <div class="flex-1 overflow-y-auto space-y-2 pr-1">

            <!-- Pinned section -->
            <template v-if="pinnedEntries.length > 0">
              <p class="text-xs text-yellow-500 font-medium">📌 已固定</p>
              <ClipCard v-for="e in pinnedEntries" :key="e.id" :entry="e"
                @copy="copyEntry(e.id)" @pin="togglePin(e.id)" @delete="deleteEntry(e.id)" />
              <hr class="border-gray-800 my-2" />
            </template>

            <!-- Recent section -->
            <template v-if="recentEntries.length > 0">
              <p v-if="pinnedEntries.length > 0" class="text-xs text-gray-600 font-medium">最近</p>
              <ClipCard v-for="e in recentEntries" :key="e.id" :entry="e"
                @copy="copyEntry(e.id)" @pin="togglePin(e.id)" @delete="deleteEntry(e.id)" />
            </template>

            <!-- Empty state -->
            <div v-if="historyEntries.length === 0" class="text-center text-gray-600 py-16">
              {{ historyQuery ? '未找到匹配项' : '复制任意内容即可记录' }}
            </div>
          </div>
        </div>
      </template>

    </main>

    <!-- Footer -->
    <footer class="text-center text-xs text-gray-800 py-2">
      rust-air v0.2 · E2EE · mDNS · SHA-256
    </footer>
  </div>
</template>

<!-- ── ProgressRing ────────────────────────────────────────────────────────── -->
<script lang="ts">
import { defineComponent } from "vue";

export const ProgressRing = defineComponent({
  props: {
    pct:        { type: Number, default: null },
    speed:      { type: String, default: "" },
    doneBytes:  { type: Number, default: 0 },
    totalBytes: { type: Number, default: 0 },
    peer:       { type: String, default: "" },
  },
  setup(props) {
    const R = 54, C = 2 * Math.PI * R;
    const dash = () => props.pct !== null ? `${(props.pct / 100) * C} ${C}` : `0 ${C}`;
    const fmt  = (n: number) => n > 1e9 ? `${(n/1e9).toFixed(2)} GB`
      : n > 1e6 ? `${(n/1e6).toFixed(1)} MB` : n > 1e3 ? `${(n/1e3).toFixed(0)} KB` : `${n} B`;
    return { R, C, dash, fmt };
  },
  template: `
    <div class="flex flex-col items-center gap-4">
      <svg width="140" height="140" class="-rotate-90">
        <circle :cx="70" :cy="70" :r="R" fill="none" stroke="#1f2937" stroke-width="12"/>
        <circle :cx="70" :cy="70" :r="R" fill="none" stroke="#06b6d4" stroke-width="12"
          stroke-linecap="round" :stroke-dasharray="dash()" style="transition:stroke-dasharray 0.3s ease"/>
      </svg>
      <div class="text-center -mt-20 mb-16 pointer-events-none">
        <p class="text-2xl font-bold text-cyan-300">{{ pct !== null ? pct + '%' : '…' }}</p>
        <p class="text-sm text-gray-400">{{ speed }}</p>
      </div>
      <div class="text-center space-y-1">
        <p class="text-gray-300 text-sm">{{ fmt(doneBytes) }}<span v-if="totalBytes"> / {{ fmt(totalBytes) }}</span></p>
        <p v-if="peer" class="text-xs text-gray-600">{{ peer }}</p>
      </div>
    </div>
  `,
});

// ── ClipCard ──────────────────────────────────────────────────────────────────
export const ClipCard = defineComponent({
  props: {
    entry: { type: Object as () => import("./App.vue").ClipEntryView, required: true },
  },
  emits: ["copy", "pin", "delete"],
  template: `
    <div :class="['rounded-xl p-3 transition-colors cursor-pointer group',
      entry.pinned ? 'bg-gray-900 border border-yellow-900/40' : 'bg-gray-900 hover:bg-gray-800']"
      @click="$emit('copy')">
      <!-- Header row -->
      <div class="flex items-center gap-2 mb-1">
        <span class="text-xs text-gray-600">{{ entry.time_str }}</span>
        <span class="text-xs text-gray-700">{{ entry.stats }}</span>
        <div class="ml-auto flex items-center gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
          <button @click.stop="$emit('pin')"
            :class="['text-xs px-1 rounded hover:bg-gray-700 transition-colors',
              entry.pinned ? 'text-yellow-400' : 'text-gray-600']"
            title="固定/取消固定">📌</button>
          <button @click.stop="$emit('delete')"
            class="text-xs px-1 rounded text-gray-600 hover:text-red-400 hover:bg-gray-700 transition-colors"
            title="删除">✕</button>
        </div>
      </div>
      <!-- Content -->
      <template v-if="entry.kind === 'text'">
        <p class="text-sm text-gray-200 font-mono leading-snug line-clamp-3 break-all">{{ entry.preview }}</p>
      </template>
      <template v-else-if="entry.kind === 'image' && entry.image_b64">
        <img :src="'data:image/png;base64,' + entry.image_b64"
          class="max-h-24 rounded object-contain" :alt="entry.stats" />
      </template>
    </div>
  `,
});
</script>
