<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

// ── Types ─────────────────────────────────────────────────────────────────────

interface Device {
  name: string;
  addr: string;
  status: "Idle" | "Busy";
}

interface TransferEvent {
  bytes_done: number;
  total_bytes: number;
  bytes_per_sec: number;
  done: boolean;
  error?: string;
}

interface SendSession {
  instance_name: string;
  key_b64: string;
}

type Tab = "send" | "receive" | "devices";
type Phase = "idle" | "waiting" | "transferring" | "done" | "error";

// ── State ─────────────────────────────────────────────────────────────────────

const tab = ref<Tab>("send");
const phase = ref<Phase>("idle");
const errorMsg = ref("");

// Send
const dragOver = ref(false);
const selectedPath = ref("");
const session = ref<SendSession | null>(null);
const receiveCode = ref("");   // "instance_name:key_b64" to share

// Receive
const receiveInput = ref("");
const outDir = ref(".");

// Progress
const progress = ref<TransferEvent>({ bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false });
const connectedPeer = ref("");

// Devices
const devices = ref<Device[]>([]);

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

// ── Ring SVG helper ───────────────────────────────────────────────────────────

const RING_R = 54;
const RING_C = 2 * Math.PI * RING_R;
const ringDash = computed(() =>
  pct.value !== null ? `${(pct.value / 100) * RING_C} ${RING_C}` : `0 ${RING_C}`
);

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
    await listen<string>("transfer-error", (e) => {
      errorMsg.value = e.payload;
      phase.value = "error";
    }),
    await listen<Device>("device-found", (e) => {
      const dev = e.payload;
      const idx = devices.value.findIndex((d) => d.name === dev.name);
      if (!dev.addr) {
        if (idx >= 0) devices.value.splice(idx, 1);
      } else if (idx >= 0) {
        devices.value[idx] = dev;
      } else {
        devices.value.push(dev);
      }
    }),
  );
});

onUnmounted(() => unlisten.value.forEach((fn) => fn()));

// ── Actions ───────────────────────────────────────────────────────────────────

async function pickFile() {
  const result = await open({ multiple: false, directory: false });
  if (result) selectedPath.value = result as string;
}

async function pickFolder() {
  const result = await open({ multiple: false, directory: true });
  if (result) selectedPath.value = result as string;
}

function onDrop(e: DragEvent) {
  dragOver.value = false;
  const f = e.dataTransfer?.files[0];
  if (f) selectedPath.value = (f as any).path ?? f.name;
}

async function startSend() {
  if (!selectedPath.value) return;
  phase.value = "waiting";
  errorMsg.value = "";
  progress.value = { bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false };
  try {
    session.value = await invoke<SendSession>("start_send", { path: selectedPath.value });
  } catch (e: any) {
    errorMsg.value = String(e);
    phase.value = "error";
  }
}

async function startReceive() {
  if (!receiveInput.value.trim()) return;
  const [instance_name, key_b64] = receiveInput.value.trim().split(":");
  if (!key_b64) { errorMsg.value = "Format: instance-name:key"; phase.value = "error"; return; }
  phase.value = "waiting";
  errorMsg.value = "";
  progress.value = { bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false };
  try {
    await invoke("start_receive", { instanceName: instance_name, keyB64: key_b64, outDir: outDir.value });
  } catch (e: any) {
    errorMsg.value = String(e);
    phase.value = "error";
  }
}

function reset() {
  invoke("cancel_send").catch(() => {});
  phase.value = "idle";
  session.value = null;
  connectedPeer.value = "";
  errorMsg.value = "";
  progress.value = { bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false };
}

async function startScan() {
  devices.value = [];
  await invoke("scan_devices");
}

async function sendToDevice(dev: Device) {
  if (!selectedPath.value) { tab.value = "send"; return; }
  // For direct device-click send, we still use the normal flow
  // (mDNS resolves by name; the device card shows the instance name)
  tab.value = "send";
}

function copyCode() {
  navigator.clipboard.writeText(shareCode.value);
}

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
    <header class="flex items-center gap-3 px-6 py-4 border-b border-gray-800">
      <span class="text-2xl">✈️</span>
      <h1 class="text-lg font-semibold tracking-tight">rust-air</h1>
      <span class="text-xs text-gray-500 ml-1">LAN file transfer</span>
      <div class="ml-auto flex gap-1">
        <button v-for="t in (['send','receive','devices'] as Tab[])" :key="t"
          @click="tab = t"
          :class="['px-3 py-1 rounded-md text-sm transition-colors',
            tab === t ? 'bg-cyan-600 text-white' : 'text-gray-400 hover:text-white hover:bg-gray-800']">
          {{ t === 'send' ? '📤 Send' : t === 'receive' ? '📥 Receive' : '🔍 Devices' }}
        </button>
      </div>
    </header>

    <!-- Main -->
    <main class="flex-1 flex flex-col items-center justify-center p-6 gap-6">

      <!-- ── SEND TAB ── -->
      <template v-if="tab === 'send'">

        <!-- Idle: drop zone -->
        <template v-if="phase === 'idle'">
          <div
            @dragover.prevent="dragOver = true"
            @dragleave="dragOver = false"
            @drop.prevent="onDrop"
            :class="['w-full max-w-lg border-2 border-dashed rounded-2xl p-10 text-center transition-colors cursor-pointer',
              dragOver ? 'border-cyan-400 bg-cyan-950/30' : 'border-gray-700 hover:border-gray-500']"
            @click="pickFile">
            <div class="text-5xl mb-3">📦</div>
            <p class="text-gray-300 font-medium">Drop a file or folder here</p>
            <p class="text-gray-500 text-sm mt-1">or click to browse</p>
          </div>

          <div class="flex gap-3">
            <button @click="pickFile"
              class="px-4 py-2 bg-gray-800 hover:bg-gray-700 rounded-lg text-sm transition-colors">
              📄 File
            </button>
            <button @click="pickFolder"
              class="px-4 py-2 bg-gray-800 hover:bg-gray-700 rounded-lg text-sm transition-colors">
              📁 Folder
            </button>
          </div>

          <div v-if="selectedPath"
            class="w-full max-w-lg bg-gray-900 rounded-xl p-4 flex items-center gap-3">
            <span class="text-cyan-400 text-xl">📎</span>
            <span class="text-sm text-gray-300 truncate flex-1">{{ selectedPath }}</span>
            <button @click="startSend"
              class="px-4 py-2 bg-cyan-600 hover:bg-cyan-500 rounded-lg text-sm font-medium transition-colors">
              Send →
            </button>
          </div>
        </template>

        <!-- Waiting for receiver -->
        <template v-else-if="phase === 'waiting'">
          <div class="text-center space-y-4">
            <div class="text-4xl animate-pulse">⏳</div>
            <p class="text-gray-300 font-medium">Waiting for receiver…</p>
            <div v-if="session" class="bg-gray-900 rounded-xl p-4 space-y-2 text-left w-80">
              <p class="text-xs text-gray-500 uppercase tracking-wider">Share this code</p>
              <div class="flex items-center gap-2">
                <code class="text-cyan-300 text-xs break-all flex-1">{{ shareCode }}</code>
                <button @click="copyCode"
                  class="text-gray-400 hover:text-white text-lg transition-colors" title="Copy">⎘</button>
              </div>
              <p class="text-xs text-gray-600">🔒 E2EE · SHA-256 verified</p>
            </div>
            <button @click="reset" class="text-sm text-gray-500 hover:text-gray-300 transition-colors">
              Cancel
            </button>
          </div>
        </template>

        <!-- Transferring -->
        <template v-else-if="phase === 'transferring'">
          <ProgressRing :pct="pct" :speed="speed" :done-bytes="progress.bytes_done"
            :total-bytes="progress.total_bytes" :peer="connectedPeer" />
        </template>

        <!-- Done -->
        <template v-else-if="phase === 'done'">
          <div class="text-center space-y-3">
            <div class="text-6xl">✅</div>
            <p class="text-gray-200 font-semibold text-lg">Transfer complete!</p>
            <p class="text-gray-500 text-sm">{{ fmtBytes(progress.bytes_done) }} sent</p>
            <button @click="reset"
              class="px-5 py-2 bg-cyan-600 hover:bg-cyan-500 rounded-lg text-sm font-medium transition-colors">
              Send another
            </button>
          </div>
        </template>

        <!-- Error -->
        <template v-else-if="phase === 'error'">
          <div class="text-center space-y-3">
            <div class="text-5xl">❌</div>
            <p class="text-red-400 font-medium">{{ errorMsg }}</p>
            <button @click="reset"
              class="px-5 py-2 bg-gray-800 hover:bg-gray-700 rounded-lg text-sm transition-colors">
              Try again
            </button>
          </div>
        </template>

      </template>

      <!-- ── RECEIVE TAB ── -->
      <template v-else-if="tab === 'receive'">

        <template v-if="phase === 'idle' || phase === 'error'">
          <div class="w-full max-w-md space-y-4">
            <h2 class="text-gray-300 font-medium">Enter the code from the sender</h2>
            <textarea v-model="receiveInput" rows="3"
              placeholder="rust-air-abc12345:base64key..."
              class="w-full bg-gray-900 border border-gray-700 rounded-xl p-3 text-sm text-cyan-300
                     font-mono resize-none focus:outline-none focus:border-cyan-500 transition-colors" />
            <div class="flex gap-3 items-center">
              <input v-model="outDir" placeholder="Output directory"
                class="flex-1 bg-gray-900 border border-gray-700 rounded-lg px-3 py-2 text-sm
                       focus:outline-none focus:border-cyan-500 transition-colors" />
              <button @click="startReceive"
                class="px-5 py-2 bg-cyan-600 hover:bg-cyan-500 rounded-lg text-sm font-medium transition-colors">
                Receive →
              </button>
            </div>
            <p v-if="phase === 'error'" class="text-red-400 text-sm">{{ errorMsg }}</p>
          </div>
        </template>

        <template v-else-if="phase === 'waiting'">
          <div class="text-center space-y-3">
            <div class="text-4xl animate-pulse">🔍</div>
            <p class="text-gray-300">Resolving sender via mDNS…</p>
          </div>
        </template>

        <template v-else-if="phase === 'transferring'">
          <ProgressRing :pct="pct" :speed="speed" :done-bytes="progress.bytes_done"
            :total-bytes="progress.total_bytes" :peer="connectedPeer" />
        </template>

        <template v-else-if="phase === 'done'">
          <div class="text-center space-y-3">
            <div class="text-6xl">✅</div>
            <p class="text-gray-200 font-semibold text-lg">Received!</p>
            <p class="text-gray-500 text-sm">{{ fmtBytes(progress.bytes_done) }} · SHA-256 verified</p>
            <button @click="reset"
              class="px-5 py-2 bg-cyan-600 hover:bg-cyan-500 rounded-lg text-sm font-medium transition-colors">
              Receive another
            </button>
          </div>
        </template>

      </template>

      <!-- ── DEVICES TAB ── -->
      <template v-else-if="tab === 'devices'">
        <div class="w-full max-w-lg space-y-4">
          <div class="flex items-center justify-between">
            <h2 class="text-gray-300 font-medium">LAN Devices</h2>
            <button @click="startScan"
              class="px-3 py-1.5 bg-gray-800 hover:bg-gray-700 rounded-lg text-sm transition-colors">
              🔄 Scan
            </button>
          </div>

          <div v-if="devices.length === 0"
            class="text-center text-gray-600 py-12">
            No devices found — click Scan to discover senders
          </div>

          <div v-for="dev in devices" :key="dev.name"
            class="bg-gray-900 rounded-xl p-4 flex items-center gap-4 hover:bg-gray-800 transition-colors cursor-pointer"
            @click="sendToDevice(dev)">
            <div :class="['w-3 h-3 rounded-full flex-shrink-0',
              dev.status === 'Idle' ? 'bg-green-400' : 'bg-yellow-400']" />
            <div class="flex-1 min-w-0">
              <p class="text-sm font-medium text-gray-200 truncate">{{ dev.name }}</p>
              <p class="text-xs text-gray-500">{{ dev.addr }}</p>
            </div>
            <span :class="['text-xs px-2 py-0.5 rounded-full',
              dev.status === 'Idle' ? 'bg-green-900/50 text-green-400' : 'bg-yellow-900/50 text-yellow-400']">
              {{ dev.status }}
            </span>
          </div>
        </div>
      </template>

    </main>

    <!-- Footer -->
    <footer class="text-center text-xs text-gray-700 py-3">
      rust-air v0.2 · E2EE · mDNS · SHA-256
    </footer>
  </div>
</template>

<!-- ── Progress Ring component (inline) ──────────────────────────────────────── -->
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
    const fmt = (n: number) => n > 1e9 ? `${(n/1e9).toFixed(2)} GB`
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
        <p class="text-gray-300 text-sm">{{ fmt(doneBytes) }}
          <span v-if="totalBytes"> / {{ fmt(totalBytes) }}</span>
        </p>
        <p v-if="peer" class="text-xs text-gray-600">{{ peer }}</p>
      </div>
    </div>
  `,
});
</script>
