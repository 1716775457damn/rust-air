<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

// ── Types ─────────────────────────────────────────────────────────────────────

interface Device { name: string; addr: string; status: "Idle" | "Busy" }
interface TransferEvent { bytes_done: number; total_bytes: number; bytes_per_sec: number; done: boolean }
interface MatchLine { line_num: number; line: string; ranges: [number,number][] }
interface FileResult { path: string; icon: string; matches: MatchLine[] }
interface SearchEvent { kind: string; path?: string; icon?: string; matches?: MatchLine[]; ms?: number; total?: number; msg?: string }
interface SyncConfig { src: string; dst: string; delete_removed: boolean; excludes: string[]; auto_watch: boolean }
interface SyncStatus { last_sync: string | null; total_files: number; total_bytes: string; is_running: boolean; is_watching: boolean }
interface SyncEventPayload { kind: string; rel?: string; bytes?: number; err?: string; scanned?: number; total?: number; total_files?: number; total_bytes?: number }

type Tab   = "send" | "receive" | "devices" | "search" | "sync";
type Phase = "idle" | "transferring" | "done" | "error";

// ── State ─────────────────────────────────────────────────────────────────────

const tab = ref<Tab>("send");

// Send
const sendPhase    = ref<Phase>("idle");
const sendError    = ref("");
const sendProgress = ref<TransferEvent>({ bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false });
const sendPeer     = ref("");
const selectedPath = ref("");
const dragOver     = ref(false);

// Receive (auto, no user input needed)
const recvPhase    = ref<Phase>("idle");
const recvError    = ref("");
const recvProgress = ref<TransferEvent>({ bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false });
const recvPeer     = ref("");
const savedPath    = ref("");
const recvHistory  = ref<{peer: string; path: string; bytes: number}[]>([]);

// Devices
const devices     = ref<Device[]>([]);
const scanning    = ref(false);
const myPort      = ref(0);

// Search
const searchPattern    = ref("");
const searchPath       = ref(localStorage.getItem('searchPath') || "C:/");
const searchMode       = ref<"filename"|"text">("filename");
const searchIgnoreCase = ref(true);
const searchFixed      = ref(false);
const searchResults    = ref<FileResult[]>([]);
const searchStatus     = ref("就绪");
const searchRunning    = ref(false);
const searchFilter     = ref("");

// Sync
const syncConfig       = ref<SyncConfig>({ src: "", dst: "", delete_removed: false, excludes: [], auto_watch: false });
const syncStatus       = ref<SyncStatus>({ last_sync: null, total_files: 0, total_bytes: "0 B", is_running: false, is_watching: false });
const syncLog          = ref<string[]>([]);
const syncExcludeInput = ref("");

// IP
const localIps  = ref<string[]>([]);
const primaryIp = computed(() => localIps.value[0] ?? "");
const ipCopied  = ref(false);

const unlisten = ref<UnlistenFn[]>([]);

// ── Computed ──────────────────────────────────────────────────────────────────

function makePct(p: TransferEvent) {
  if (!p.total_bytes) return null;
  return Math.min(100, Math.round((p.bytes_done / p.total_bytes) * 100));
}
function makeSpeed(p: TransferEvent) {
  const bps = p.bytes_per_sec;
  if (bps > 1_000_000) return `${(bps/1_000_000).toFixed(1)} MB/s`;
  if (bps > 1_000)     return `${(bps/1_000).toFixed(0)} KB/s`;
  return `${bps} B/s`;
}
const sendPct   = computed(() => makePct(sendProgress.value));
const sendSpeed = computed(() => makeSpeed(sendProgress.value));
const recvPct   = computed(() => makePct(recvProgress.value));
const recvSpeed = computed(() => makeSpeed(recvProgress.value));

// Devices excluding self
const peersOnly = computed(() =>
  devices.value.filter(d => d.addr && !d.addr.startsWith(primaryIp.value + ":"))
);

const selectedName = computed(() => selectedPath.value.split(/[\/\\]/).pop() ?? selectedPath.value);

const filteredResults = computed(() => {
  const q = searchFilter.value.trim().toLowerCase();
  return q ? searchResults.value.filter(r => r.path.toLowerCase().includes(q)) : searchResults.value;
});

// ── Lifecycle ─────────────────────────────────────────────────────────────────

onMounted(async () => {
  // Start listener + mDNS registration
  myPort.value = await invoke<number>("start_listener");

  localIps.value = await invoke<string[]>("get_local_ips");

  unlisten.value.push(
    // Send events
    await listen<TransferEvent>("send-progress", (e) => {
      sendProgress.value = e.payload;
      sendPhase.value = "transferring";
    }),
    await listen<string>("send-peer-connected", (e) => { sendPeer.value = e.payload; }),
    await listen("send-done", () => { sendPhase.value = "done"; }),
    await listen<string>("send-error", (e) => { sendError.value = e.payload; sendPhase.value = "error"; }),

    // Receive events (auto)
    await listen<string>("recv-peer-connected", (e) => {
      recvPeer.value = e.payload;
      recvPhase.value = "transferring";
      tab.value = "receive";
    }),
    await listen<TransferEvent>("recv-progress", (e) => { recvProgress.value = e.payload; }),
    await listen<string>("recv-done", (e) => {
      savedPath.value = e.payload ?? "";
      recvHistory.value.unshift({ peer: recvPeer.value, path: savedPath.value, bytes: recvProgress.value.bytes_done });
      recvPhase.value = "done";
      // System notification
      const filename = savedPath.value.split(/[\/\\]/).pop() ?? savedPath.value;
      new Notification("rust-air — 文件已接收", { body: filename, silent: false }).onclick = () => {};
    }),
    await listen<string>("recv-error", (e) => { recvError.value = e.payload; recvPhase.value = "error"; }),

    // Device discovery
    await listen<Device>("device-found", (e) => {
      const dev = e.payload;
      const idx = devices.value.findIndex(d => d.name === dev.name);
      if (!dev.addr) { if (idx >= 0) devices.value.splice(idx, 1); }
      else if (idx >= 0) { devices.value[idx] = dev; }
      else { devices.value.push(dev); }
    }),

    // Sync events
    await listen<SyncEventPayload>("sync-event", (e) => {
      const ev = e.payload;
      if (ev.kind === "Copied")        syncLog.value.unshift(`✅ ${ev.rel}  (${ev.bytes} B)`);
      else if (ev.kind === "Deleted")  syncLog.value.unshift(`🗑 ${ev.rel}`);
      else if (ev.kind === "Error")    syncLog.value.unshift(`❌ ${ev.rel}: ${ev.err}`);
      else if (ev.kind === "Progress") syncLog.value[0] = `⏳ 扫描中… ${ev.scanned} 个文件`;
      else if (ev.kind === "Done") {
        syncLog.value.unshift(`🎉 同步完成  共 ${ev.total_files} 个文件`);
        invoke("sync_done").then(() => invoke<SyncStatus>("get_sync_status").then(s => syncStatus.value = s));
      }
      if (syncLog.value.length > 200) syncLog.value.length = 200;
    }),
    await listen("sync-done", async () => {
      syncStatus.value = await invoke<SyncStatus>("get_sync_status");
    }),

    // Search events
    await listen<FileResult[]>("search-batch", (e) => {
      for (const r of e.payload) searchResults.value.push(r);
      searchStatus.value = `搜索中… 已找到 ${searchResults.value.length} 个`;
    }),
    await listen<SearchEvent>("search-result", (e) => {
      const ev = e.payload;
      if (ev.kind === "Done") {
        searchRunning.value = false;
        searchStatus.value = searchResults.value.length === 0
          ? `未找到结果 (${ev.ms}ms)` : `找到 ${ev.total} 个文件 (${ev.ms}ms)`;
      } else if (ev.kind === "Error") {
        searchRunning.value = false;
        searchStatus.value = `错误: ${ev.msg}`;
      }
    }),
  );

  syncConfig.value = await invoke<SyncConfig>("get_sync_config");
  syncStatus.value = await invoke<SyncStatus>("get_sync_status");
  const defaultEx  = await invoke<string[]>("get_default_excludes");
  if (syncConfig.value.excludes.length === 0) syncConfig.value.excludes = defaultEx;

  // Auto-scan on startup
  startScan();
});

onUnmounted(async () => {
  unlisten.value.forEach(fn => fn());
  await invoke("cancel_search").catch(() => {});
});

// ── Send ──────────────────────────────────────────────────────────────────────

async function pickFile()   { const r = await open({ multiple: false, directory: false }); if (r) selectedPath.value = r as string; }
async function pickFolder() { const r = await open({ multiple: false, directory: true  }); if (r) selectedPath.value = r as string; }
function onDrop(e: DragEvent) {
  dragOver.value = false;
  const f = e.dataTransfer?.files[0];
  if (f) selectedPath.value = (f as any).path ?? f.name;
}

async function sendToDevice(dev: Device) {
  if (!selectedPath.value) return;
  sendPhase.value = "transferring"; sendError.value = ""; sendPeer.value = dev.addr;
  sendProgress.value = { bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false };
  try {
    await invoke("send_to", { path: selectedPath.value, addr: dev.addr });
  } catch (e: any) { sendError.value = String(e); sendPhase.value = "error"; }
}

function resetSend() {
  invoke("cancel_send").catch(() => {});
  sendPhase.value = "idle"; sendPeer.value = ""; sendError.value = "";
  sendProgress.value = { bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false };
}

function resetRecv() {
  recvPhase.value = "idle"; recvPeer.value = ""; recvError.value = ""; savedPath.value = "";
  recvProgress.value = { bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false };
}

// ── Devices ───────────────────────────────────────────────────────────────────

async function startScan() {
  scanning.value = true;
  // Don't clear existing devices during rescan — avoids flicker.
  await invoke("scan_devices");
  setTimeout(() => { scanning.value = false; }, 8000);
}

// ── Search ────────────────────────────────────────────────────────────────────

async function doSearch() {
  if (!searchPattern.value.trim()) return;
  searchResults.value = []; searchRunning.value = true; searchStatus.value = "搜索中…";
  try {
    await invoke("start_search", {
      pattern: searchPattern.value, path: searchPath.value,
      ignoreCase: searchIgnoreCase.value, fixedString: searchFixed.value, mode: searchMode.value,
    });
  } catch (e: any) { searchRunning.value = false; searchStatus.value = `错误: ${e}`; }
}
async function stopSearch() {
  await invoke("cancel_search").catch(() => {});
  searchRunning.value = false; searchStatus.value = "已取消";
}
async function pickSearchPath() {
  const r = await open({ multiple: false, directory: true });
  if (r) {
    searchPath.value = r as string;
    localStorage.setItem('searchPath', searchPath.value);
  }
}

// ── Sync ──────────────────────────────────────────────────────────────────────

async function pickSyncSrc() { const r = await open({ multiple: false, directory: true }); if (r) syncConfig.value.src = r as string; }
async function pickSyncDst() { const r = await open({ multiple: false, directory: true }); if (r) syncConfig.value.dst = r as string; }
async function saveAndSync() {
  await invoke("save_sync_config", { config: syncConfig.value });
  syncLog.value = []; syncStatus.value.is_running = true;
  await invoke("start_sync").catch((e: any) => { syncLog.value.unshift(`❌ ${e}`); syncStatus.value.is_running = false; });
}
async function toggleWatch() {
  if (syncStatus.value.is_watching) {
    await invoke("stop_watch"); syncStatus.value.is_watching = false;
  } else {
    await invoke("save_sync_config", { config: syncConfig.value });
    await invoke("start_watch").catch((e: any) => syncLog.value.unshift(`❌ ${e}`));
    syncStatus.value.is_watching = true;
  }
}
function addExclude() {
  const v = syncExcludeInput.value.trim();
  if (v && !syncConfig.value.excludes.includes(v)) syncConfig.value.excludes.push(v);
  syncExcludeInput.value = "";
}
function removeExclude(i: number) { syncConfig.value.excludes.splice(i, 1); }

// ── IP ────────────────────────────────────────────────────────────────────────

async function copyIp() {
  const addr = primaryIp.value; if (!addr) return;
  await invoke("write_clipboard", { text: addr }).catch(() => navigator.clipboard?.writeText(addr).catch(() => {}));
  ipCopied.value = true; setTimeout(() => { ipCopied.value = false; }, 1500);
}
async function refreshIps() { localIps.value = await invoke<string[]>("get_local_ips"); }

// ── Helpers ───────────────────────────────────────────────────────────────────

function fmtBytes(n: number) {
  if (n > 1e9) return `${(n/1e9).toFixed(2)} GB`;
  if (n > 1e6) return `${(n/1e6).toFixed(1)} MB`;
  if (n > 1e3) return `${(n/1e3).toFixed(0)} KB`;
  return `${n} B`;
}
function shortName(fullname: string) {
  return fullname.split(".")[0] ?? fullname;
}

/** Split a line into plain/highlighted segments for rendering.
 *  ranges are char-unit [start, end) pairs from the backend. */
function highlightSegments(line: string, ranges: [number,number][]) {
  const chars = [...line];
  const out: { text: string; hl: boolean }[] = [];
  let pos = 0;
  for (const [s, e] of ranges) {
    if (s > pos) out.push({ text: chars.slice(pos, s).join(''), hl: false });
    out.push({ text: chars.slice(s, e).join(''), hl: true });
    pos = e;
  }
  if (pos < chars.length) out.push({ text: chars.slice(pos).join(''), hl: false });
  return out;
}
</script>

<template>
  <div class="h-screen bg-[#0d1117] text-gray-100 flex flex-col select-none font-sans overflow-hidden">

    <!-- Header -->
    <header class="flex items-center gap-4 px-5 h-14 border-b border-white/8 bg-[#161b22] flex-shrink-0">
      <span class="text-lg">✈️</span>
      <h1 class="text-sm font-bold tracking-wide text-white/90">rust-air</h1>
      <div class="flex-1"></div>
      <button v-if="primaryIp" @click="copyIp"
        :class="['flex items-center gap-2.5 px-4 py-1.5 rounded-xl font-mono transition-all duration-200',
          ipCopied ? 'bg-green-500/15 text-green-300 ring-1 ring-green-500/30'
                   : 'bg-cyan-500/10 text-cyan-300 hover:bg-cyan-500/20 ring-1 ring-cyan-500/20']">
        <span class="text-[11px] text-gray-500 font-sans">本机</span>
        <span class="text-base font-bold tracking-wide">{{ primaryIp }}</span>
        <span class="text-xs opacity-70">{{ ipCopied ? 'ok' : 'copy' }}</span>
      </button>
      <button @click="refreshIps" class="text-gray-600 hover:text-gray-300 text-sm transition-colors" title="刷新 IP">↻</button>
    </header>

    <!-- Body -->
    <div class="flex flex-1 overflow-hidden">

      <!-- Sidebar -->
      <nav class="flex flex-col gap-1 w-[72px] flex-shrink-0 border-r border-white/5 px-1.5 py-3 bg-[#161b22]">
        <button v-for="t in (['send','receive','devices','search','sync'] as Tab[])" :key="t"
          @click="tab = t"
          :class="['flex flex-col items-center gap-1 py-2.5 rounded-xl text-xs transition-all duration-150 w-full',
            tab === t ? 'bg-cyan-500/15 text-cyan-300' : 'text-gray-500 hover:text-gray-200 hover:bg-white/5']">
          <span class="text-[15px] leading-none">{{ t==='send'?'📤':t==='receive'?'📥':t==='devices'?'🔍':t==='search'?'📂':'🔄' }}</span>
          <span class="text-[10px] mt-0.5">{{ t==='send'?'发送':t==='receive'?'接收':t==='devices'?'设备':t==='search'?'搜索':'同步' }}</span>
          <!-- Badge for incoming transfer -->
          <span v-if="t==='receive' && recvPhase==='transferring'" class="w-1.5 h-1.5 rounded-full bg-cyan-400 animate-pulse mt-0.5"></span>
        </button>
      </nav>

      <!-- Main -->
      <main class="flex-1 flex flex-col p-5 gap-4 overflow-hidden bg-[#0d1117]">

        <!-- SEND TAB -->
        <template v-if="tab === 'send'">
          <div class="flex-1 flex flex-col gap-4">

            <!-- File picker -->
            <div @dragover.prevent="dragOver=true" @dragleave="dragOver=false" @drop.prevent="onDrop"
              :class="['border-2 border-dashed rounded-2xl p-6 text-center transition-all cursor-pointer flex-shrink-0',
                dragOver ? 'border-cyan-400 bg-cyan-950/30' : 'border-gray-700/60 hover:border-gray-500']"
              @click="pickFile">
              <div class="text-3xl mb-1">📦</div>
              <p class="text-gray-300 text-sm">拖拽文件 / 文件夹，或点击选择</p>
            </div>
            <div class="flex gap-2 flex-shrink-0">
              <button @click="pickFile"   class="px-3 py-1.5 bg-gray-800/80 hover:bg-gray-700 rounded-lg text-sm transition-colors">📄 文件</button>
              <button @click="pickFolder" class="px-3 py-1.5 bg-gray-800/80 hover:bg-gray-700 rounded-lg text-sm transition-colors">📁 文件夹</button>
            </div>

            <!-- Selected file -->
            <div v-if="selectedPath" class="bg-gray-900/80 rounded-xl p-3 flex items-center gap-3 ring-1 ring-white/5 flex-shrink-0">
              <span class="text-cyan-400">📎</span>
              <span class="text-sm text-gray-300 truncate flex-1">{{ selectedPath }}</span>
              <button @click="selectedPath=''" class="text-gray-600 hover:text-red-400 text-xs">✕</button>
            </div>

            <!-- Transfer progress -->
            <div v-if="sendPhase === 'transferring'" class="bg-gray-900/80 rounded-xl p-4 ring-1 ring-white/5 flex-shrink-0">
              <div class="flex items-center justify-between mb-2">
                <span class="text-sm text-gray-300">发送中 → {{ sendPeer }}</span>
                <span class="text-sm text-cyan-300">{{ sendSpeed }}</span>
              </div>
              <div class="w-full bg-gray-800 rounded-full h-2">
                <div class="bg-cyan-500 h-2 rounded-full transition-all duration-300"
                  :style="{ width: sendPct !== null ? sendPct + '%' : '0%' }"></div>
              </div>
              <div class="flex justify-between mt-1 text-xs text-gray-500">
                <span>{{ fmtBytes(sendProgress.bytes_done) }}</span>
                <span>{{ sendPct !== null ? sendPct + '%' : '…' }}</span>
              </div>
            </div>
            <div v-else-if="sendPhase === 'done'" class="bg-green-900/20 rounded-xl p-3 ring-1 ring-green-500/20 flex items-center gap-3 flex-shrink-0">
              <span class="text-2xl">✅</span>
              <div class="flex-1 min-w-0">
                <span class="text-green-300 text-sm">发送完成！{{ fmtBytes(sendProgress.bytes_done) }}</span>
                <p class="text-xs text-gray-500 truncate">{{ selectedName }}</p>
              </div>
              <button @click="resetSend" class="ml-auto text-xs text-gray-500 hover:text-gray-300">关闭</button>
            </div>
            <div v-else-if="sendPhase === 'error'" class="bg-red-900/20 rounded-xl p-3 ring-1 ring-red-500/20 flex items-center gap-3 flex-shrink-0">
              <span class="text-2xl">❌</span>
              <span class="text-red-400 text-sm truncate flex-1">{{ sendError }}</span>
              <button @click="resetSend" class="ml-auto text-xs text-gray-500 hover:text-gray-300">关闭</button>
            </div>

            <!-- Device list to send to -->
            <div class="flex-1 min-h-0 flex flex-col gap-2">
              <div class="flex items-center justify-between flex-shrink-0">
                <p class="text-xs text-gray-500">选择目标设备发送</p>
                <button @click="startScan" :class="['text-xs px-2 py-1 rounded-lg transition-colors', scanning ? 'text-cyan-400 animate-pulse' : 'text-gray-500 hover:text-gray-300 bg-gray-800/60']">
                  {{ scanning ? '扫描中…' : '🔄 刷新' }}
                </button>
              </div>
              <div v-if="peersOnly.length === 0" class="text-center text-gray-600 py-8 text-sm">
                {{ scanning ? '正在扫描局域网…' : '未发现设备 — 点击刷新' }}
              </div>
              <div v-for="dev in peersOnly" :key="dev.name"
                @click="selectedPath && sendToDevice(dev)"
                :class="['rounded-xl p-3.5 flex items-center gap-3 ring-1 ring-white/5 transition-all',
                  selectedPath
                    ? 'bg-gray-900/80 hover:bg-cyan-900/30 hover:ring-cyan-500/30 cursor-pointer'
                    : 'bg-gray-900/40 opacity-50 cursor-not-allowed']">
                <div class="w-2.5 h-2.5 rounded-full bg-green-400 flex-shrink-0"></div>
                <div class="flex-1 min-w-0">
                  <p class="text-sm font-medium text-gray-200">{{ shortName(dev.name) }}</p>
                  <p class="text-xs text-gray-500">{{ dev.addr }}</p>
                </div>
                <span v-if="selectedPath" class="text-xs text-cyan-400">点击发送 →</span>
              </div>
              <p v-if="!selectedPath && peersOnly.length > 0" class="text-xs text-gray-600 text-center">请先选择要发送的文件</p>
            </div>

          </div>
        </template>

        <!-- RECEIVE TAB -->
        <template v-else-if="tab === 'receive'">
          <div class="flex-1 flex flex-col gap-4 min-h-0">
            <p class="text-xs text-gray-500 flex-shrink-0">自动接收 — 有人向你发送文件时会在此显示</p>

            <!-- Active incoming transfer -->
            <div v-if="recvPhase === 'transferring'" class="bg-gray-900/80 rounded-xl p-4 ring-1 ring-cyan-500/20 flex-shrink-0">
              <div class="flex items-center justify-between mb-2">
                <span class="text-sm text-gray-300">接收中 ← {{ recvPeer }}</span>
                <span class="text-sm text-cyan-300">{{ recvSpeed }}</span>
              </div>
              <div class="w-full bg-gray-800 rounded-full h-2">
                <div class="bg-cyan-500 h-2 rounded-full transition-all duration-300"
                  :style="{ width: recvPct !== null ? recvPct + '%' : '0%' }"></div>
              </div>
              <div class="flex justify-between mt-1 text-xs text-gray-500">
                <span>{{ fmtBytes(recvProgress.bytes_done) }}</span>
                <span>{{ recvPct !== null ? recvPct + '%' : '…' }}</span>
              </div>
            </div>

            <div v-else-if="recvPhase === 'done'" class="bg-green-900/20 rounded-xl p-3 ring-1 ring-green-500/20 flex items-center gap-3 flex-shrink-0">
              <span class="text-2xl">✅</span>
              <div class="flex-1 min-w-0">
                <p class="text-green-300 text-sm">接收完成！{{ fmtBytes(recvProgress.bytes_done) }}</p>
                <p class="text-xs text-gray-500 truncate">{{ savedPath }}</p>
              </div>
              <button @click="resetRecv" class="text-xs text-gray-500 hover:text-gray-300">关闭</button>
            </div>

            <div v-else-if="recvPhase === 'error'" class="bg-red-900/20 rounded-xl p-3 ring-1 ring-red-500/20 flex items-center gap-3 flex-shrink-0">
              <span class="text-2xl">❌</span>
              <span class="text-red-400 text-sm truncate flex-1">{{ recvError }}</span>
              <button @click="resetRecv" class="text-xs text-gray-500 hover:text-gray-300">关闭</button>
            </div>

            <div v-else class="flex flex-col items-center justify-center py-10 text-gray-600 flex-shrink-0">
              <div class="text-4xl mb-2">📥</div>
              <p class="text-sm">等待接收…</p>
              <p class="text-xs mt-1">文件将保存到下载目录</p>
            </div>

            <!-- History -->
            <div v-if="recvHistory.length > 0" class="flex-1 min-h-0 overflow-y-auto space-y-1">
              <p class="text-xs text-gray-600 mb-2">接收历史</p>
              <div v-for="(h, i) in recvHistory" :key="i"
                class="bg-gray-900/60 rounded-lg p-2.5 flex items-center gap-2 text-xs">
                <span class="text-green-400">✓</span>
                <span class="text-gray-400 truncate flex-1">{{ h.path.split(/[/\\]/).pop() }}</span>
                <span class="text-gray-600">{{ fmtBytes(h.bytes) }}</span>
              </div>
            </div>
          </div>
        </template>

        <!-- DEVICES TAB -->
        <template v-else-if="tab === 'devices'">
          <div class="flex-1 flex flex-col gap-4 max-w-lg mx-auto w-full">
            <div class="flex items-center justify-between">
              <h2 class="text-gray-300 font-medium text-sm">局域网设备</h2>
              <button @click="startScan" :class="['px-3 py-1.5 rounded-lg text-xs transition-colors', scanning ? 'text-cyan-400 animate-pulse bg-gray-800/60' : 'bg-gray-800/80 hover:bg-gray-700']">
                {{ scanning ? '扫描中…' : '🔄 扫描' }}
              </button>
            </div>
            <div v-if="devices.length === 0" class="text-center text-gray-600 py-12 text-sm">未发现设备 — 点击扫描</div>
            <div v-for="dev in devices" :key="dev.name"
              class="bg-gray-900/80 rounded-xl p-3.5 flex items-center gap-4 ring-1 ring-white/5">
              <div class="w-3 h-3 rounded-full flex-shrink-0 bg-green-400"></div>
              <div class="flex-1 min-w-0">
                <p class="text-sm font-medium text-gray-200 truncate">{{ shortName(dev.name) }}</p>
                <p class="text-xs text-gray-500">{{ dev.addr }}</p>
              </div>
            </div>
          </div>
        </template>

        <!-- SEARCH TAB -->
        <template v-else-if="tab === 'search'">
          <div class="flex-1 flex flex-col gap-3 min-h-0">
            <div class="flex items-center gap-2 flex-shrink-0 flex-wrap">
              <input v-model="searchPath" placeholder="搜索路径"
                class="w-36 bg-gray-900 border border-gray-700 rounded-lg px-2 py-1.5 text-xs focus:outline-none focus:border-cyan-500 transition-colors" />
              <button @click="pickSearchPath" class="px-2 py-1.5 bg-gray-800 hover:bg-gray-700 rounded-lg text-xs transition-colors">📂</button>
              <select v-model="searchMode" class="bg-gray-900 border border-gray-700 rounded-lg px-2 py-1.5 text-xs focus:outline-none">
                <option value="filename">🗂 文件名</option>
                <option value="text">📄 文本</option>
              </select>
              <input v-model="searchPattern" @keyup.enter="doSearch" placeholder="搜索内容…"
                class="flex-1 bg-gray-900 border border-gray-700 rounded-lg px-3 py-1.5 text-sm focus:outline-none focus:border-cyan-500 transition-colors" />
              <label class="flex items-center gap-1 text-xs text-gray-400 cursor-pointer">
                <input type="checkbox" v-model="searchIgnoreCase" class="accent-cyan-500" />忽略大小写
              </label>
              <label class="flex items-center gap-1 text-xs text-gray-400 cursor-pointer">
                <input type="checkbox" v-model="searchFixed" class="accent-cyan-500" />纯文本
              </label>
              <button v-if="!searchRunning" @click="doSearch"
                class="px-3 py-1.5 bg-cyan-600 hover:bg-cyan-500 rounded-lg text-xs font-medium transition-colors">🔍 搜索</button>
              <button v-else @click="stopSearch"
                class="px-3 py-1.5 bg-red-700 hover:bg-red-600 rounded-lg text-xs transition-colors">⏹ 取消</button>
            </div>
            <div class="flex items-center gap-2 flex-shrink-0">
              <span class="text-xs text-gray-500 flex-1">{{ searchStatus }}</span>
              <input v-if="searchResults.length > 0" v-model="searchFilter" placeholder="过滤结果…"
                class="w-36 bg-gray-900 border border-gray-700 rounded-lg px-2 py-1 text-xs focus:outline-none focus:border-cyan-500 transition-colors" />
            </div>
            <div class="flex-1 overflow-y-auto space-y-1 pr-1 min-h-0">
              <div v-if="filteredResults.length === 0 && !searchRunning" class="text-center text-gray-600 py-16 text-sm">
                {{ searchResults.length === 0 ? '输入内容后按回车搜索' : '无匹配结果' }}
              </div>
              <div v-for="r in filteredResults" :key="r.path" class="bg-gray-900/80 rounded-xl p-3 ring-1 ring-white/5">
                <div class="flex items-center gap-2 mb-1">
                  <span>{{ r.icon }}</span>
                  <span class="text-xs text-cyan-300 font-mono truncate flex-1">{{ r.path }}</span>
                  <span class="text-xs text-gray-600">{{ r.matches.length }} 处</span>
                </div>
                <!-- filename mode: highlight matched chars in filename -->
                <div v-if="searchMode === 'filename'" class="font-mono text-xs mt-0.5">
                  <template v-for="seg in highlightSegments(r.matches[0].line, r.matches[0].ranges)" :key="seg.text">
                    <span :class="seg.hl ? 'bg-yellow-400/30 text-yellow-200 rounded px-0.5' : 'text-gray-400'">{{ seg.text }}</span>
                  </template>
                </div>
                <!-- text mode: show matching lines with highlights -->
                <div v-else class="space-y-0.5 mt-1">
                  <div v-for="(m, mi) in r.matches.slice(0, 5)" :key="mi" class="flex gap-2 font-mono text-xs">
                    <span class="text-green-500 w-8 text-right flex-shrink-0">{{ m.line_num }}:</span>
                    <span class="truncate">
                      <template v-for="seg in highlightSegments(m.line, m.ranges)" :key="seg.text">
                        <span :class="seg.hl ? 'bg-yellow-400/30 text-yellow-200 rounded px-0.5' : 'text-gray-300'">{{ seg.text }}</span>
                      </template>
                    </span>
                  </div>
                  <div v-if="r.matches.length > 5" class="text-xs text-gray-600 pl-10">…另外 {{ r.matches.length - 5 }} 处</div>
                </div>
              </div>
            </div>
          </div>
        </template>

        <!-- SYNC TAB -->
        <template v-else-if="tab === 'sync'">
          <div class="flex-1 flex flex-col gap-4 min-h-0 max-w-xl mx-auto w-full">
            <div class="flex items-center gap-3 bg-gray-900 rounded-xl px-4 py-2 text-xs flex-shrink-0">
              <span :class="syncStatus.is_running ? 'text-yellow-400 animate-pulse' : 'text-green-400'">
                {{ syncStatus.is_running ? '⏳ 同步中…' : '✅ 空闲' }}
              </span>
              <span class="text-gray-600">上次: {{ syncStatus.last_sync ?? '从未同步' }}</span>
              <span class="text-gray-600">共 {{ syncStatus.total_files }} 个文件 / {{ syncStatus.total_bytes }}</span>
              <span v-if="syncStatus.is_watching" class="ml-auto text-cyan-400">👁 监听中</span>
            </div>
            <div class="space-y-3 flex-shrink-0">
              <div class="flex gap-2 items-center">
                <span class="text-xs text-gray-500 w-8">源</span>
                <input v-model="syncConfig.src" placeholder="源目录路径"
                  class="flex-1 bg-gray-900 border border-gray-700 rounded-lg px-3 py-1.5 text-sm focus:outline-none focus:border-cyan-500 transition-colors" />
                <button @click="pickSyncSrc" class="px-2 py-1.5 bg-gray-800 hover:bg-gray-700 rounded-lg text-xs transition-colors">📂</button>
              </div>
              <div class="flex gap-2 items-center">
                <span class="text-xs text-gray-500 w-8">目标</span>
                <input v-model="syncConfig.dst" placeholder="目标目录路径"
                  class="flex-1 bg-gray-900 border border-gray-700 rounded-lg px-3 py-1.5 text-sm focus:outline-none focus:border-cyan-500 transition-colors" />
                <button @click="pickSyncDst" class="px-2 py-1.5 bg-gray-800 hover:bg-gray-700 rounded-lg text-xs transition-colors">📂</button>
              </div>
              <label class="flex items-center gap-2 text-xs text-gray-400 cursor-pointer">
                <input type="checkbox" v-model="syncConfig.delete_removed" class="accent-cyan-500" />删除已移除的文件
              </label>
              <div class="space-y-1">
                <p class="text-xs text-gray-500">排除规则</p>
                <div class="flex gap-2">
                  <input v-model="syncExcludeInput" @keyup.enter="addExclude" placeholder="*.tmp 或 node_modules"
                    class="flex-1 bg-gray-900 border border-gray-700 rounded-lg px-3 py-1 text-xs focus:outline-none focus:border-cyan-500 transition-colors" />
                  <button @click="addExclude" class="px-2 py-1 bg-gray-800 hover:bg-gray-700 rounded text-xs transition-colors">+</button>
                </div>
                <div class="flex flex-wrap gap-1 mt-1">
                  <span v-for="(ex, i) in syncConfig.excludes" :key="i"
                    class="flex items-center gap-1 bg-gray-800 text-gray-400 text-xs px-2 py-0.5 rounded-full">
                    {{ ex }}
                    <button @click="removeExclude(i)" class="text-gray-600 hover:text-red-400">x</button>
                  </span>
                </div>
              </div>
            </div>
            <div class="flex gap-2 flex-shrink-0">
              <button @click="saveAndSync" :disabled="syncStatus.is_running"
                :class="['flex-1 py-2 rounded-lg text-sm font-medium transition-colors',
                  syncStatus.is_running ? 'bg-gray-800 text-gray-600 cursor-not-allowed' : 'bg-cyan-600 hover:bg-cyan-500 text-white']">
                {{ syncStatus.is_running ? '同步中…' : '🔄 立即同步' }}
              </button>
              <button @click="toggleWatch"
                :class="['px-4 py-2 rounded-lg text-sm transition-colors',
                  syncStatus.is_watching ? 'bg-yellow-700 hover:bg-yellow-600 text-white' : 'bg-gray-800 hover:bg-gray-700 text-gray-300']">
                {{ syncStatus.is_watching ? '⏹ 停止监听' : '👁 实时监听' }}
              </button>
            </div>
            <div class="flex-1 overflow-y-auto bg-gray-900 rounded-xl p-3 font-mono text-xs space-y-0.5 min-h-0">
              <div v-if="syncLog.length === 0" class="text-gray-600 text-center py-4">日志将在此显示</div>
              <div v-for="(line, i) in syncLog" :key="i"
                :class="['leading-5', line.startsWith('❌') ? 'text-red-400' : line.startsWith('🗑') ? 'text-yellow-500' : 'text-gray-400']">
                {{ line }}
              </div>
            </div>
          </div>
        </template>

      </main>
    </div>

    <footer class="text-center text-[11px] text-gray-700 py-1.5 border-t border-white/5 bg-[#161b22]">
      rust-air v0.3 · E2EE · mDNS · SHA-256
    </footer>
  </div>
</template>
