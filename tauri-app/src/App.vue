<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { platform } from "@tauri-apps/plugin-os";
import { useSearch } from "./composables/useSearch";
import { useDevices } from "./composables/useDevices";
import { useClipboardSync } from "./composables/useClipboardSync";
import DevicesPanel from "./components/DevicesPanel.vue";
import SearchPanel from "./components/SearchPanel.vue";
import SyncPanel from "./components/SyncPanel.vue";
import { useSync } from "./composables/useSync";
import { useTransfer } from "./composables/useTransfer";
import { useUpdate } from "./composables/useUpdate";
import {
  todayStr,
} from "./utils/transfer";
import type {
  CalendarDay,
  ClipEntryView,
  ClipSyncError,
  ClipSyncReceived,
  Device,
  FileResult,
  SearchEvent,
  SyncEventPayload,
  Tab,
  TodoItem,
  TransferEvent,
  UpdateInfo,
  UpdateProgress,
  WhiteboardError,
  WhiteboardItem,
} from "./types/app";

// ── Types ─────────────────────────────────────────────────────────────────────

// ── State ─────────────────────────────────────────────────────────────────────

const tab = ref<Tab>("send");

// Platform detection (Android build support)
const isAndroid = ref(false);

// Theme
const isDark = ref(true);
function toggleTheme() {
  isDark.value = !isDark.value;
  document.documentElement.classList.toggle("light", !isDark.value);
  localStorage.setItem("theme", isDark.value ? "dark" : "light");
}

// Send
const {
  sendPhase,
  sendError,
  sendProgress,
  sendPeer,
  selectedPath,
  dragOver,
  recvPhase,
  recvError,
  recvProgress,
  recvPeer,
  savedPath,
  recvHistory,
  sendReconnecting,
  sendReconnectAttempt,
  sendReconnectMax,
  sendPct,
  sendSpeed,
  sendEta,
  recvPct,
  recvSpeed,
  recvEta,
  sendIndeterminate,
  recvIndeterminate,
  selectedName,
  pickFile,
  pickFolder,
  onDrop,
  sendToDevice,
  resetSend,
  retrySend,
  resetRecv,
  onSendProgress,
  onSendPeerConnected,
  onSendDone,
  onSendError,
  onRecvPeerConnected,
  onRecvProgress,
  onRecvDone,
  onRecvError,
} = useTransfer();

// Devices
const {
  devices,
  scanning,
  primaryIp,
  ipCopied,
  peersOnlyFor,
  initListenerAndIps,
  onDeviceFound,
  startScan,
  copyIp,
  refreshIps,
} = useDevices();

// Search
const {
  searchPattern,
  searchPath,
  searchMode,
  searchIgnoreCase,
  searchFixed,
  searchResults,
  searchStatus,
  searchRunning,
  searchFilter,
  filteredResults,
  onSearchFilterInput,
  cachedHighlight,
  onSearchBatch,
  onSearchResult,
  doSearch,
  stopSearch,
  pickSearchPath,
  revealInFolder,
  cancelSearchOnUnmount,
} = useSearch();

// Sync
const {
  syncConfig,
  syncStatus,
  syncLog,
  syncExcludeInput,
  loadInitialSyncState,
  onSyncEvent,
  onSyncDone,
  pickSyncSrc,
  pickSyncDst,
  saveAndSync,
  toggleWatch,
  addExclude,
  removeExclude,
} = useSync(fmtBytes);

// Update
const {
  updateInfo,
  updateProgress,
  updateChecking,
  updateSettings,
  loadInitialUpdateState,
  onUpdateAvailable,
  onUpdateProgress,
  manualCheckUpdate,
  startInstall,
  saveUpdateSettings,
} = useUpdate(showToast);

// Todo
const selectedDate   = ref<string>(todayStr());
const calendarYear   = ref(new Date().getFullYear());
const calendarMonth  = ref(new Date().getMonth() + 1);
const todos          = ref<TodoItem[]>([]);
const todoDates      = ref<string[]>([]);
const newTodoTitle   = ref("");

// Clipboard Sync (Task 10.1)
const {
  clipSyncEnabled,
  syncGroupConfig,
  clipEntries,
  loadInitialClipboardSyncState,
  onClipUpdate,
  onClipSyncError,
  onClipSyncReceived,
  toggleClipSync,
  isPeerInSyncGroup,
  toggleSyncPeer,
} = useClipboardSync(showToast);

// Whiteboard
const whiteboardItems = ref<WhiteboardItem[]>([]);
const wbTextInput     = ref("");
const wbClearConfirm  = ref(false);

// Toast notifications (Task 10.4)
const toasts = ref<{ id: number; kind: string; message: string; device?: string }[]>([]);
let toastId = 0;
function showToast(kind: string, message: string, device?: string) {
  const id = ++toastId;
  toasts.value.push({ id, kind, message, device });
  setTimeout(() => { toasts.value = toasts.value.filter(t => t.id !== id); }, 3000);
}

const unlisten = ref<UnlistenFn[]>([]);

// ── Computed ──────────────────────────────────────────────────────────────────

const peersOnly = computed(() => peersOnlyFor(primaryIp.value));

// ── Calendar computed ─────────────────────────────────────────────────────────

const calendarDays = computed<CalendarDay[]>(() => {
  const y = calendarYear.value;
  const m = calendarMonth.value;
  const daysInMonth = new Date(y, m, 0).getDate();
  const firstDow = new Date(y, m - 1, 1).getDay(); // 0=Sun
  const prevMonthDays = new Date(y, m - 1, 0).getDate();
  const grid: CalendarDay[] = [];

  // Previous month trailing days
  for (let i = firstDow - 1; i >= 0; i--) {
    const d = prevMonthDays - i;
    const pm = m === 1 ? 12 : m - 1;
    const py = m === 1 ? y - 1 : y;
    grid.push({ day: d, current: false, dateStr: `${py}-${String(pm).padStart(2,'0')}-${String(d).padStart(2,'0')}` });
  }

  // Current month days
  for (let d = 1; d <= daysInMonth; d++) {
    grid.push({ day: d, current: true, dateStr: `${y}-${String(m).padStart(2,'0')}-${String(d).padStart(2,'0')}` });
  }

  // Next month leading days to fill 6×7 grid
  const remaining = 42 - grid.length;
  const nm = m === 12 ? 1 : m + 1;
  const ny = m === 12 ? y + 1 : y;
  for (let d = 1; d <= remaining; d++) {
    grid.push({ day: d, current: false, dateStr: `${ny}-${String(nm).padStart(2,'0')}-${String(d).padStart(2,'0')}` });
  }

  return grid;
});

function prevMonth() {
  if (calendarMonth.value === 1) { calendarMonth.value = 12; calendarYear.value--; }
  else { calendarMonth.value--; }
}
function nextMonth() {
  if (calendarMonth.value === 12) { calendarMonth.value = 1; calendarYear.value++; }
  else { calendarMonth.value++; }
}

// ── Lifecycle ─────────────────────────────────────────────────────────────────

onMounted(async () => {
  // Restore theme
  const saved = localStorage.getItem("theme");
  if (saved === "light") { isDark.value = false; document.documentElement.classList.add("light"); }

  // Platform detection
  try {
    const p = await platform();
    isAndroid.value = (p === "android");
  } catch (_) { /* fallback: assume desktop */ }

  await initListenerAndIps();

  window.addEventListener("keydown", onKeyDown);

  unlisten.value.push(
    await listen<TransferEvent>("send-progress", (e) => {
      onSendProgress(e.payload);
    }),
    await listen<string>("send-peer-connected", (e) => {
      onSendPeerConnected(e.payload);
    }),
    await listen("send-done", () => {
      onSendDone();
    }),
    await listen<string>("send-error", (e) => { onSendError(e.payload); }),

    await listen<string>("recv-peer-connected", (e) => {
      onRecvPeerConnected(e.payload);
      tab.value = "receive";
    }),
    await listen<TransferEvent>("recv-progress", (e) => {
      onRecvProgress(e.payload);
    }),
    await listen<string>("recv-done", (e) => {
      onRecvDone(e.payload ?? "");
    }),
    await listen<string>("recv-error", (e) => { onRecvError(e.payload); }),

    await listen<Device>("device-found", (e) => {
      onDeviceFound(e.payload);
    }),

    await listen<SyncEventPayload>("sync-event", (e) => {
      onSyncEvent(e.payload);
    }),
    await listen("sync-done", async () => {
      await onSyncDone();
    }),

    await listen<FileResult[]>("search-batch", (e) => {
      onSearchBatch(e.payload);
    }),
    await listen<SearchEvent>("search-result", (e) => {
      onSearchResult(e.payload);
    }),
    await listen<UpdateInfo>("update-available", (e) => {
      onUpdateAvailable(e.payload);
    }),
    await listen<UpdateProgress>("update-progress", (e) => {
      onUpdateProgress(e.payload);
    }),

    // Clipboard sync events (Task 10.1 & 10.4)
    await listen<ClipEntryView[]>("clip-update", (e) => {
      onClipUpdate(e.payload);
    }),
    await listen<ClipSyncError>("clip-sync-error", (e) => {
      onClipSyncError(e.payload);
    }),
    await listen<ClipSyncReceived>("clip-sync-received", (e) => {
      onClipSyncReceived(e.payload);
    }),

    // Whiteboard events
    await listen<WhiteboardItem[]>("whiteboard-update", (e) => {
      whiteboardItems.value = e.payload;
    }),
    await listen<WhiteboardError>("whiteboard-error", (e) => {
      const err = e.payload;
      showToast("whiteboard_error", `白板同步: ${err.message}`, err.device ?? undefined);
    }),
  );

  // Desktop-only: load sync config, status, excludes, and update settings
  if (!isAndroid.value) {
    await loadInitialSyncState();
    await loadInitialUpdateState();
  }

  // Load clipboard sync state (Task 10.1)
  try {
    await loadInitialClipboardSyncState();
  } catch (_) { /* sync commands may not be registered yet */ }

  // Load initial todo data
  await loadTodos(selectedDate.value);
  await loadTodoDates(calendarYear.value, calendarMonth.value);

  // Load initial whiteboard data and start flush timer
  if (!isAndroid.value) {
    try { whiteboardItems.value = await invoke<WhiteboardItem[]>("get_whiteboard_items"); }
    catch (_) { /* whiteboard commands may not be registered yet */ }
    _whiteboardFlushTimer = setInterval(() => { invoke("flush_whiteboard").catch(() => {}); }, 3000);
  }

  startScan();
});

onUnmounted(async () => {
  window.removeEventListener("keydown", onKeyDown);
  unlisten.value.forEach(fn => fn());
  if (_whiteboardFlushTimer) {
    clearInterval(_whiteboardFlushTimer);
    _whiteboardFlushTimer = null;
  }
  await cancelSearchOnUnmount();
});

// ── Keyboard shortcuts ────────────────────────────────────────────────────────

const TAB_KEYS: Record<string, Tab> = { "1": "send", "2": "receive", "3": "devices", "4": "search", "5": "sync", "6": "todo", "7": "whiteboard" };
function onKeyDown(e: KeyboardEvent) {
  if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;
  if (TAB_KEYS[e.key]) { tab.value = TAB_KEYS[e.key]; e.preventDefault(); }
}

// ── Transfer moved to composable ──────────────────────────────────────────────

// ── Devices moved to composable ────────────────────────────────────────────────

// ── Search moved to composable ────────────────────────────────────────────────

// ── Sync / Update moved to composables ───────────────────────────────────────

// ── Clipboard Sync moved to composable ────────────────────────────────────────

// ── Todo IPC ─────────────────────────────────────────────────────────────────

async function loadTodos(date: string) {
  try { todos.value = await invoke<TodoItem[]>("get_todos", { date }); }
  catch (e: any) { console.error("loadTodos:", e); }
}

async function loadTodoDates(year: number, month: number) {
  try { todoDates.value = await invoke<string[]>("get_todo_dates", { year, month }); }
  catch (e: any) { console.error("loadTodoDates:", e); }
}

async function addTodo() {
  const title = newTodoTitle.value.trim();
  if (!title) return;
  try {
    await invoke<TodoItem>("add_todo", { title, date: selectedDate.value });
    newTodoTitle.value = "";
    await loadTodos(selectedDate.value);
    await loadTodoDates(calendarYear.value, calendarMonth.value);
  } catch (e: any) { console.error("addTodo:", e); }
}

async function toggleTodo(id: number) {
  try {
    await invoke<TodoItem>("toggle_todo", { id });
    await loadTodos(selectedDate.value);
    await loadTodoDates(calendarYear.value, calendarMonth.value);
  } catch (e: any) { console.error("toggleTodo:", e); }
}

async function deleteTodo(id: number) {
  try {
    await invoke("delete_todo", { id });
    await loadTodos(selectedDate.value);
    await loadTodoDates(calendarYear.value, calendarMonth.value);
  } catch (e: any) { console.error("deleteTodo:", e); }
}

watch(selectedDate, (d) => loadTodos(d));
watch([calendarYear, calendarMonth], ([y, m]) => loadTodoDates(y, m));

// ── Whiteboard IPC ───────────────────────────────────────────────────────────

async function addWbText() {
  const text = wbTextInput.value.trim();
  if (!text) return;
  try {
    await invoke<WhiteboardItem>("add_whiteboard_text", { text });
    wbTextInput.value = "";
  } catch (e: any) { console.error("addWbText:", e); }
}

async function addWbImage(b64: string) {
  try {
    await invoke<WhiteboardItem>("add_whiteboard_image", { imageB64: b64 });
  } catch (e: any) { console.error("addWbImage:", e); }
}

async function deleteWbItem(id: string) {
  try {
    await invoke("delete_whiteboard_item", { id });
  } catch (e: any) { console.error("deleteWbItem:", e); }
}

async function clearWhiteboard() {
  try {
    await invoke("clear_whiteboard");
    wbClearConfirm.value = false;
  } catch (e: any) { console.error("clearWhiteboard:", e); }
}

function onWbPaste(e: ClipboardEvent) {
  const items = e.clipboardData?.items;
  if (!items) return;
  for (const item of items) {
    if (item.type.startsWith("image/")) {
      e.preventDefault();
      const blob = item.getAsFile();
      if (!blob) return;
      const reader = new FileReader();
      reader.onload = () => {
        const result = reader.result as string;
        // Strip the data:image/...;base64, prefix
        const b64 = result.split(",")[1];
        if (b64) addWbImage(b64);
      };
      reader.readAsDataURL(blob);
      return;
    }
  }
}

function fmtWbTime(ts: number): string {
  const d = new Date(ts);
  const hh = String(d.getHours()).padStart(2, '0');
  const mm = String(d.getMinutes()).padStart(2, '0');
  const MM = String(d.getMonth() + 1).padStart(2, '0');
  const DD = String(d.getDate()).padStart(2, '0');
  return `${MM}-${DD} ${hh}:${mm}`;
}

// ── IP moved to devices composable ────────────────────────────────────────────

// ── Helpers ───────────────────────────────────────────────────────────────────

function fmtBytes(n: number) {
  if (n > 1e9) return `${(n/1e9).toFixed(2)} GB`;
  if (n > 1e6) return `${(n/1e6).toFixed(1)} MB`;
  if (n > 1e3) return `${(n/1e3).toFixed(0)} KB`;
  return `${n} B`;
}
function shortName(fullname: string) { return fullname.split(".")[0] ?? fullname; }

const now = ref(Date.now());
let _nowTimer: ReturnType<typeof setInterval>;
let _whiteboardFlushTimer: ReturnType<typeof setInterval> | null = null;
onMounted(() => { _nowTimer = setInterval(() => { now.value = Date.now(); }, 10_000); });
onUnmounted(() => clearInterval(_nowTimer));

function fmtLastSeen(ts?: number): string {
  if (!ts) return "";
  const secs = Math.floor((now.value - ts) / 1000);
  if (secs < 10)   return "刚刚";
  if (secs < 60)   return `${secs}s 前`;
  if (secs < 3600) return `${Math.floor(secs/60)}m 前`;
  return `${Math.floor(secs/3600)}h 前`;
}

function updateSearchField(field: string, value: string | boolean) {
  if (field === "searchPath") searchPath.value = value as string
  else if (field === "searchMode") searchMode.value = value as "filename" | "text"
  else if (field === "searchPattern") searchPattern.value = value as string
  else if (field === "searchIgnoreCase") searchIgnoreCase.value = value as boolean
  else if (field === "searchFixed") searchFixed.value = value as boolean
}

function updateSyncConfigField(field: string, value: string | boolean) {
  if (field === "src") syncConfig.value.src = value as string
  else if (field === "dst") syncConfig.value.dst = value as string
  else if (field === "delete_removed") syncConfig.value.delete_removed = value as boolean
}
</script>

<template>
  <div class="h-screen flex flex-col select-none font-sans overflow-hidden"
    style="background:var(--bg-base);color:var(--text-primary)">

    <!-- Header -->
    <header class="flex items-center gap-4 px-5 h-14 flex-shrink-0"
      style="background:var(--bg-surface);border-bottom:1px solid var(--border)">
      <span class="text-lg">✈️</span>
      <h1 class="text-sm font-bold tracking-wide" style="color:var(--text-primary)">rust-air</h1>
      <div class="flex-1"></div>
      <button v-if="primaryIp" @click="() => copyIp(isAndroid)"
        :style="ipCopied
          ? 'background:rgba(34,197,94,0.12);color:#86efac;box-shadow:0 0 0 1px rgba(34,197,94,0.25)'
          : 'background:var(--accent-bg);color:var(--accent);box-shadow:0 0 0 1px var(--accent-ring)'"
        class="flex items-center gap-2.5 px-4 py-1.5 rounded-xl font-mono transition-all duration-200">
        <span class="text-[11px] font-sans" style="color:var(--text-muted)">本机</span>
        <span class="text-base font-bold tracking-wide">{{ primaryIp }}</span>
        <span class="text-xs opacity-70">{{ ipCopied ? '✓' : 'copy' }}</span>
      </button>
      <!-- Theme toggle -->
      <button @click="toggleTheme"
        class="w-8 h-8 rounded-lg flex items-center justify-center text-base transition-colors"
        style="color:var(--text-secondary)"
        :title="isDark ? '切换浅色' : '切换深色'">
        {{ isDark ? '☀️' : '🌙' }}
      </button>
      <button @click="refreshIps" class="text-sm transition-colors"
        style="color:var(--text-faint)" title="刷新 IP">↻</button>
      <button @click="tab = 'settings'" class="w-8 h-8 rounded-lg flex items-center justify-center text-base transition-colors"
        :style="tab === 'settings' ? 'color:var(--accent)' : 'color:var(--text-secondary)'"
        title="设置">⚙️</button>
    </header>

    <!-- Update banner -->
    <div v-if="updateInfo && !updateProgress"
      class="flex items-center gap-3 px-5 py-2 text-sm flex-shrink-0"
      style="background:rgba(34,197,94,0.1);border-bottom:1px solid rgba(34,197,94,0.25)">
      <span style="color:#4ade80">🚀 发现新版本 {{ updateInfo.version }}</span>
      <span class="flex-1 text-xs truncate" style="color:var(--text-muted)">{{ updateInfo.release_notes.split('\n')[0] }}</span>
      <button @click="startInstall"
        class="px-3 py-1 rounded-lg text-xs font-medium text-white flex-shrink-0"
        style="background:#16a34a">立即更新</button>
      <button @click="updateInfo = null" class="text-xs flex-shrink-0" style="color:var(--text-muted)">忽略</button>
    </div>

    <!-- Download progress bar -->
    <div v-if="updateProgress && !updateProgress.done"
      class="flex items-center gap-3 px-5 py-2 text-xs flex-shrink-0"
      style="background:rgba(6,182,212,0.08);border-bottom:1px solid rgba(6,182,212,0.2)">
      <span style="color:var(--accent)">⬇️ 下载更新中…</span>
      <div class="flex-1 rounded-full h-1.5 overflow-hidden" style="background:var(--bg-muted)">
        <div class="h-1.5 rounded-full transition-all duration-200" style="background:var(--accent)"
          :style="{ width: updateProgress.total ? Math.round(updateProgress.downloaded/updateProgress.total*100)+'%' : '0%' }"></div>
      </div>
      <span style="color:var(--text-muted)">{{ fmtBytes(updateProgress.downloaded) }} / {{ fmtBytes(updateProgress.total) }}</span>
    </div>

    <!-- Body -->
    <div class="flex flex-1 overflow-hidden">

      <!-- Sidebar -->
      <nav class="flex flex-col gap-1 w-[72px] flex-shrink-0 px-1.5 py-3"
        style="background:var(--bg-surface);border-right:1px solid var(--border)">
        <button v-for="(t, idx) in (isAndroid ? (['send','receive','devices','todo'] as Tab[]) : (['send','receive','devices','search','sync','todo','whiteboard'] as Tab[]))" :key="t"
          @click="tab = t"
          :title="`${t === 'send' ? '发送' : t === 'receive' ? '接收' : t === 'devices' ? '设备' : t === 'search' ? '搜索' : t === 'sync' ? '同步' : t === 'todo' ? '待办' : '白板'} (${idx+1})`"
          :style="tab === t
            ? 'background:var(--accent-bg);color:var(--accent)'
            : 'color:var(--text-muted)'"
          class="flex flex-col items-center gap-1 py-2.5 rounded-xl text-xs transition-all duration-150 w-full hover:opacity-80">
          <span class="text-[15px] leading-none">{{ t==='send'?'📤':t==='receive'?'📥':t==='devices'?'🔍':t==='search'?'📂':t==='sync'?'🔄':t==='todo'?'📋':'🖊️' }}</span>
          <span class="text-[10px] mt-0.5">{{ t==='send'?'发送':t==='receive'?'接收':t==='devices'?'设备':t==='search'?'搜索':t==='sync'?'同步':t==='todo'?'待办':'白板' }}</span>
          <span class="text-[9px] leading-none" style="color:var(--text-faint)">{{ idx+1 }}</span>
          <span v-if="t==='receive' && recvPhase==='transferring'"
            class="w-1.5 h-1.5 rounded-full animate-pulse mt-0.5"
            style="background:var(--accent)"></span>
        </button>
        <div class="flex-1"></div>
        <button @click="tab = 'settings'"
          :style="tab === 'settings' ? 'background:var(--accent-bg);color:var(--accent)' : 'color:var(--text-muted)'"
          class="flex flex-col items-center gap-1 py-2.5 rounded-xl text-xs transition-all duration-150 w-full hover:opacity-80"
          title="设置">
          <span class="text-[15px] leading-none">⚙️</span>
          <span class="text-[10px] mt-0.5">设置</span>
          <span v-if="updateInfo" class="w-1.5 h-1.5 rounded-full mt-0.5" style="background:#4ade80"></span>
        </button>
      </nav>

      <!-- Main -->
      <main class="flex-1 flex flex-col p-5 gap-4 overflow-hidden" style="background:var(--bg-base)">

        <!-- SEND TAB -->
        <template v-if="tab === 'send'">
          <div class="flex-1 flex flex-col gap-4">

            <!-- Drop zone -->
            <div @dragover.prevent="dragOver=true" @dragleave="dragOver=false" @drop.prevent="onDrop"
              :style="dragOver
                ? 'border-color:var(--accent);background:var(--accent-bg)'
                : 'border-color:var(--border-input)'"
              class="border-2 border-dashed rounded-2xl p-6 text-center transition-all cursor-pointer flex-shrink-0"
              @click="pickFile">
              <div class="text-3xl mb-1">📦</div>
              <p class="text-sm" style="color:var(--text-secondary)">拖拽文件 / 文件夹，或点击选择</p>
            </div>

            <div class="flex gap-2 flex-shrink-0">
              <button @click="pickFile"
                class="px-3 py-1.5 rounded-lg text-sm transition-colors"
                style="background:var(--bg-muted);color:var(--text-secondary)">📄 文件</button>
              <button @click="pickFolder"
                class="px-3 py-1.5 rounded-lg text-sm transition-colors"
                style="background:var(--bg-muted);color:var(--text-secondary)">📁 文件夹</button>
            </div>

            <!-- Selected file -->
            <div v-if="selectedPath"
              class="rounded-xl p-3 flex items-center gap-3 flex-shrink-0"
              style="background:var(--bg-card);box-shadow:0 0 0 1px var(--border)">
              <span style="color:var(--accent)">📎</span>
              <span class="text-sm truncate flex-1" style="color:var(--text-secondary)" :title="selectedPath">{{ selectedPath }}</span>
              <button @click="selectedPath=''" class="text-xs flex-shrink-0"
                style="color:var(--text-muted)">✕</button>
            </div>

            <!-- Transfer progress -->
            <div v-if="sendPhase === 'transferring'"
              class="rounded-xl p-4 flex-shrink-0"
              style="background:var(--bg-card);box-shadow:0 0 0 1px var(--border)">
              <!-- Reconnect banner (sender-initiated) -->
              <div v-if="sendReconnecting"
                class="flex items-center gap-2 mb-3 px-3 py-2 rounded-lg text-xs"
                style="background:rgba(251,191,36,0.1);border:1px solid rgba(251,191,36,0.25)">
                <span class="animate-pulse">🔄</span>
                <span style="color:#fbbf24">重连中 (第 {{ sendReconnectAttempt }} 次 / 共 {{ sendReconnectMax }} 次)</span>
              </div>
              <div class="flex items-center justify-between mb-2">
                <span class="text-sm truncate" style="color:var(--text-secondary)">
                  发送中 → <span :style="`color:var(--accent)`">{{ sendPeer }}</span>
                </span>
                <div class="flex items-center gap-2 flex-shrink-0">
                  <span v-if="sendEta" class="text-xs" style="color:var(--text-muted)">剩 {{ sendEta }}</span>
                  <span class="text-sm" :style="`color:var(--accent)`">{{ sendSpeed }}</span>
                </div>
              </div>
              <div class="w-full rounded-full h-2 overflow-hidden" style="background:var(--bg-muted)">
                <div v-if="sendIndeterminate"
                  class="h-2 rounded-full animate-[slide_1.5s_ease-in-out_infinite]"
                  style="width:40%;background:var(--accent)"></div>
                <div v-else class="h-2 rounded-full transition-all duration-300"
                  style="background:var(--accent)"
                  :style="{ width: (sendPct ?? 0) + '%' }"></div>
              </div>
              <div class="flex justify-between mt-1 text-xs" style="color:var(--text-muted)">
                <span>{{ fmtBytes(sendProgress.bytes_done) }}</span>
                <span>{{ sendIndeterminate ? '计算中…' : (sendPct !== null ? sendPct + '%' : '…') }}</span>
              </div>
            </div>

            <!-- Send done -->
            <div v-else-if="sendPhase === 'done'"
              class="rounded-xl p-3 flex items-center gap-3 flex-shrink-0"
              style="background:rgba(34,197,94,0.08);box-shadow:0 0 0 1px rgba(34,197,94,0.2)">
              <span class="text-2xl">✅</span>
              <div class="flex-1 min-w-0">
                <span class="text-sm" style="color:#86efac">发送完成！{{ fmtBytes(sendProgress.bytes_done) }}</span>
                <p class="text-xs truncate" style="color:var(--text-muted)">{{ selectedName }}</p>
              </div>
              <button @click="resetSend" class="ml-auto text-xs flex-shrink-0"
                style="color:var(--text-muted)">关闭</button>
            </div>

            <!-- Send error -->
            <div v-else-if="sendPhase === 'error'"
              class="rounded-xl p-3 flex items-center gap-3 flex-shrink-0"
              style="background:rgba(239,68,68,0.08);box-shadow:0 0 0 1px rgba(239,68,68,0.2)">
              <span class="text-2xl">❌</span>
              <span class="text-sm truncate flex-1" style="color:#fca5a5" :title="sendError">{{ sendError }}</span>
              <button @click="retrySend"
                class="px-3 py-1.5 rounded-lg text-xs font-medium flex-shrink-0 transition-colors"
                style="background:var(--accent-bg);color:var(--accent)">重试</button>
              <button @click="resetSend" class="ml-1 text-xs flex-shrink-0"
                style="color:var(--text-muted)">关闭</button>
            </div>

            <!-- Device list -->
            <div class="flex-1 min-h-0 flex flex-col gap-2">
              <div class="flex items-center justify-between flex-shrink-0">
                <p class="text-xs" style="color:var(--text-muted)">选择目标设备发送</p>
                <button @click="startScan"
                  :style="scanning ? `color:var(--accent)` : `color:var(--text-muted);background:var(--bg-muted)`"
                  :class="['text-xs px-2 py-1 rounded-lg transition-colors', scanning ? 'animate-pulse' : '']">
                  {{ scanning ? '扫描中…' : '🔄 刷新' }}
                </button>
              </div>
              <div v-if="peersOnly.length === 0" class="text-center py-8 text-sm" style="color:var(--text-faint)">
                {{ scanning ? '正在扫描局域网…' : '未发现设备 — 点击刷新' }}
              </div>
              <div v-for="dev in peersOnly" :key="dev.name"
                @click="selectedPath && sendToDevice(dev)"
                :style="selectedPath
                  ? `background:var(--bg-card);box-shadow:0 0 0 1px var(--border);cursor:pointer`
                  : `background:var(--bg-card);opacity:0.45;cursor:not-allowed`"
                class="rounded-xl p-3.5 flex items-center gap-3 transition-all">
                <div class="w-2.5 h-2.5 rounded-full flex-shrink-0" style="background:#4ade80"></div>
                <div class="flex-1 min-w-0">
                  <p class="text-sm font-medium" style="color:var(--text-primary)">{{ shortName(dev.name) }}</p>
                  <p class="text-xs" style="color:var(--text-muted)">{{ dev.addr }}
                    <span v-if="dev.lastSeen" class="ml-1" style="color:var(--text-faint)">· {{ fmtLastSeen(dev.lastSeen) }}</span>
                  </p>
                </div>
                <span v-if="selectedPath" class="text-xs flex-shrink-0" style="color:var(--accent)">发送 →</span>
              </div>
              <p v-if="!selectedPath && peersOnly.length > 0" class="text-xs text-center" style="color:var(--text-faint)">请先选择要发送的文件</p>
            </div>

          </div>
        </template>

        <!-- RECEIVE TAB -->
        <template v-else-if="tab === 'receive'">
          <div class="flex-1 flex flex-col gap-4 min-h-0">
            <p class="text-xs flex-shrink-0" style="color:var(--text-muted)">自动接收 — 有人向你发送文件时会在此显示</p>

            <div v-if="recvPhase === 'transferring'"
              class="rounded-xl p-4 flex-shrink-0"
              style="background:var(--bg-card);box-shadow:0 0 0 1px var(--accent-ring)">
              <!-- Resume indicator -->
              <div v-if="recvProgress.resumed && recvProgress.resume_offset"
                class="flex items-center gap-2 mb-2 text-xs"
                style="color:var(--accent)">
                <span>⏩</span>
                <span>续传中 — 已跳过 {{ fmtBytes(recvProgress.resume_offset) }}</span>
              </div>
              <div class="flex items-center justify-between mb-2">
                <span class="text-sm" style="color:var(--text-secondary)">
                  接收中 ← <span :style="`color:var(--accent)`">{{ recvPeer }}</span>
                </span>
                <div class="flex items-center gap-2 flex-shrink-0">
                  <span v-if="recvEta" class="text-xs" style="color:var(--text-muted)">剩 {{ recvEta }}</span>
                  <span class="text-sm" :style="`color:var(--accent)`">{{ recvSpeed }}</span>
                </div>
              </div>
              <div class="w-full rounded-full h-2 overflow-hidden" style="background:var(--bg-muted)">
                <div v-if="recvIndeterminate"
                  class="h-2 rounded-full animate-[slide_1.5s_ease-in-out_infinite]"
                  style="width:40%;background:var(--accent)"></div>
                <div v-else class="h-2 rounded-full transition-all duration-300"
                  style="background:var(--accent)"
                  :style="{ width: (recvPct ?? 0) + '%' }"></div>
              </div>
              <div class="flex justify-between mt-1 text-xs" style="color:var(--text-muted)">
                <span>{{ fmtBytes(recvProgress.bytes_done) }}</span>
                <span>{{ recvIndeterminate ? '计算中…' : (recvPct !== null ? recvPct + '%' : '…') }}</span>
              </div>
            </div>

            <div v-else-if="recvPhase === 'done'"
              class="rounded-xl p-3 flex items-center gap-3 flex-shrink-0"
              style="background:rgba(34,197,94,0.08);box-shadow:0 0 0 1px rgba(34,197,94,0.2)">
              <span class="text-2xl">✅</span>
              <div class="flex-1 min-w-0">
                <p class="text-sm" style="color:#86efac">接收完成！{{ fmtBytes(recvProgress.bytes_done) }}</p>
                <button @click="revealInFolder(savedPath)"
                  class="text-xs truncate block max-w-full text-left mt-0.5 hover:underline"
                  style="color:var(--accent)" :title="savedPath">
                  📂 {{ savedPath }}
                </button>
              </div>
              <button @click="resetRecv" class="text-xs flex-shrink-0" style="color:var(--text-muted)">关闭</button>
            </div>

            <div v-else-if="recvPhase === 'error'"
              class="rounded-xl p-3 flex items-center gap-3 flex-shrink-0"
              style="background:rgba(239,68,68,0.08);box-shadow:0 0 0 1px rgba(239,68,68,0.2)">
              <span class="text-2xl">❌</span>
              <span class="text-sm truncate flex-1" style="color:#fca5a5">{{ recvError }}</span>
              <button @click="resetRecv" class="text-xs flex-shrink-0" style="color:var(--text-muted)">关闭</button>
            </div>

            <div v-else class="flex flex-col items-center justify-center py-10 flex-shrink-0" style="color:var(--text-faint)">
              <div class="text-4xl mb-2">📥</div>
              <p class="text-sm">等待接收…</p>
              <p class="text-xs mt-1">文件将保存到下载目录</p>
            </div>

            <!-- History -->
            <div v-if="recvHistory.length > 0" class="flex-1 min-h-0 overflow-y-auto space-y-1">
              <p class="text-xs mb-2" style="color:var(--text-faint)">接收历史</p>
              <div v-for="(h, i) in recvHistory" :key="i"
                class="rounded-lg p-2.5 flex items-center gap-2 text-xs group"
                style="background:var(--bg-card)">
                <span class="flex-shrink-0" style="color:#4ade80">✓</span>
                <button @click="revealInFolder(h.path)"
                  class="truncate flex-1 text-left transition-colors hover:underline"
                  style="color:var(--text-secondary)" :title="h.path">
                  {{ h.path.split(/[/\\]/).pop() }}
                </button>
                <span class="flex-shrink-0" style="color:var(--text-faint)">{{ fmtBytes(h.bytes) }}</span>
                <button @click="revealInFolder(h.path)"
                  class="opacity-0 group-hover:opacity-100 transition-opacity flex-shrink-0"
                  style="color:var(--accent)" title="打开所在目录">📂</button>
              </div>
            </div>

            <!-- Clipboard History (Task 10.3) -->
            <div v-if="clipEntries.length > 0" class="flex-1 min-h-0 flex flex-col">
              <p class="text-xs mb-2 flex-shrink-0" style="color:var(--text-faint)">剪贴板历史</p>
              <div class="flex-1 min-h-0 overflow-y-auto space-y-1">
                <div v-for="entry in clipEntries" :key="entry.id"
                  class="rounded-lg p-2.5 flex items-center gap-2 text-xs group"
                  style="background:var(--bg-card)">
                  <span class="flex-shrink-0">{{ entry.kind === 'image' ? '🖼️' : '📝' }}</span>
                  <div class="flex-1 min-w-0">
                    <p class="truncate" style="color:var(--text-secondary)" :title="entry.preview">{{ entry.preview }}</p>
                    <div class="flex items-center gap-2 mt-0.5">
                      <span style="color:var(--text-faint)">{{ entry.time_str }}</span>
                      <span style="color:var(--text-faint)">{{ entry.stats }}</span>
                      <span v-if="entry.source_device" class="px-1.5 py-0.5 rounded-full text-[10px]"
                        style="background:var(--accent-bg);color:var(--accent)">
                        来自 {{ entry.source_device }}
                      </span>
                    </div>
                  </div>
                  <span v-if="entry.pinned" class="flex-shrink-0" title="已固定">📌</span>
                </div>
              </div>
            </div>
          </div>
        </template>

        <!-- DEVICES TAB -->
        <template v-else-if="tab === 'devices'">
          <DevicesPanel
            :scanning="scanning"
            :clip-sync-enabled="clipSyncEnabled"
            :devices="devices"
            :short-name="shortName"
            :fmt-last-seen="fmtLastSeen"
            :is-peer-in-sync-group="isPeerInSyncGroup"
            @scan="startScan"
            @toggle-clip-sync="toggleClipSync(!clipSyncEnabled)"
            @toggle-sync-peer="toggleSyncPeer"
          />
        </template>

        <!-- SEARCH TAB -->
        <template v-else-if="tab === 'search'">
          <SearchPanel
            :search-path="searchPath"
            :search-mode="searchMode"
            :search-pattern="searchPattern"
            :search-ignore-case="searchIgnoreCase"
            :search-fixed="searchFixed"
            :search-running="searchRunning"
            :search-status="searchStatus"
            :search-results="searchResults"
            :search-filter="searchFilter"
            :filtered-results="filteredResults"
            :cached-highlight="cachedHighlight"
            @pick-path="pickSearchPath"
            @search="doSearch"
            @stop="stopSearch"
            @filter-input="onSearchFilterInput"
            @reveal="revealInFolder"
            @update="updateSearchField"
          />
        </template>

        <!-- SYNC TAB -->
        <template v-else-if="tab === 'sync'">
          <SyncPanel
            :sync-config="syncConfig"
            :sync-status="syncStatus"
            :sync-exclude-input="syncExcludeInput"
            :sync-log="syncLog"
            @pick-src="pickSyncSrc"
            @pick-dst="pickSyncDst"
            @save-and-sync="saveAndSync"
            @toggle-watch="toggleWatch"
            @add-exclude="addExclude"
            @remove-exclude="removeExclude"
            @update-config="updateSyncConfigField"
            @update-exclude-input="(value) => (syncExcludeInput = value)"
          />
        </template>

        <!-- TODO TAB -->
        <template v-else-if="tab === 'todo'">
          <div class="flex-1 flex flex-col gap-4 min-h-0 max-w-xl mx-auto w-full">

            <!-- Calendar -->
            <div class="rounded-xl p-4 flex-shrink-0"
              style="background:var(--bg-card);box-shadow:0 0 0 1px var(--border)">
              <!-- Month navigation -->
              <div class="flex items-center justify-between mb-3">
                <button @click="prevMonth"
                  class="w-8 h-8 rounded-lg flex items-center justify-center text-sm transition-colors hover:opacity-80"
                  style="background:var(--bg-muted);color:var(--text-secondary)">‹</button>
                <span class="text-sm font-medium" style="color:var(--text-primary)">{{ calendarYear }} 年 {{ calendarMonth }} 月</span>
                <button @click="nextMonth"
                  class="w-8 h-8 rounded-lg flex items-center justify-center text-sm transition-colors hover:opacity-80"
                  style="background:var(--bg-muted);color:var(--text-secondary)">›</button>
              </div>
              <!-- Weekday headers -->
              <div class="grid grid-cols-7 text-center mb-1">
                <span v-for="w in ['日','一','二','三','四','五','六']" :key="w"
                  class="text-[10px] py-1" style="color:var(--text-muted)">{{ w }}</span>
              </div>
              <!-- Date grid -->
              <div class="grid grid-cols-7 text-center gap-y-0.5">
                <button v-for="(d, i) in calendarDays" :key="i"
                  @click="d.current && (selectedDate = d.dateStr)"
                  :class="[
                    'relative w-full py-1.5 rounded-lg text-xs transition-all duration-150',
                    d.current ? 'cursor-pointer hover:opacity-80' : 'cursor-default'
                  ]"
                  :style="
                    d.dateStr === selectedDate
                      ? 'background:var(--accent-bg);color:var(--accent);font-weight:600'
                      : d.dateStr === todayStr() && d.current
                        ? 'color:var(--accent);font-weight:600'
                        : d.current
                          ? 'color:var(--text-primary)'
                          : 'color:var(--text-faint)'
                  ">
                  {{ d.day }}
                  <span v-if="d.current && todoDates.includes(d.dateStr)"
                    class="absolute bottom-0.5 left-1/2 -translate-x-1/2 w-1 h-1 rounded-full"
                    style="background:var(--accent)"></span>
                </button>
              </div>
            </div>

            <!-- Selected date label -->
            <div class="flex items-center gap-2 flex-shrink-0">
              <span class="text-xs" style="color:var(--text-muted)">{{ selectedDate }}</span>
              <button v-if="selectedDate !== todayStr()" @click="selectedDate = todayStr(); calendarYear = new Date().getFullYear(); calendarMonth = new Date().getMonth() + 1"
                class="text-xs px-2 py-0.5 rounded-lg transition-colors"
                style="background:var(--bg-muted);color:var(--text-secondary)">回到今天</button>
            </div>

            <!-- Todo list -->
            <div class="flex-1 min-h-0 overflow-y-auto space-y-1">
              <div v-if="todos.length === 0" class="text-center py-8 text-sm" style="color:var(--text-faint)">
                暂无待办
              </div>
              <div v-for="item in todos" :key="item.id"
                class="rounded-xl p-3 flex items-center gap-3 group"
                style="background:var(--bg-card);box-shadow:0 0 0 1px var(--border)">
                <input type="checkbox" :checked="item.completed" @change="toggleTodo(item.id)"
                  class="accent-cyan-500 flex-shrink-0 cursor-pointer" />
                <span class="flex-1 text-sm truncate"
                  :style="item.completed
                    ? 'color:var(--text-faint);text-decoration:line-through'
                    : 'color:var(--text-primary)'">{{ item.title }}</span>
                <button @click="deleteTodo(item.id)"
                  class="text-xs flex-shrink-0 opacity-0 group-hover:opacity-100 transition-opacity"
                  style="color:var(--text-muted)">✕</button>
              </div>
            </div>

            <!-- Add todo input -->
            <div class="flex gap-2 flex-shrink-0">
              <input v-model="newTodoTitle" @keyup.enter="addTodo"
                placeholder="添加待办事项…"
                class="flex-1 rounded-lg px-3 py-2 text-sm focus:outline-none transition-colors"
                style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)" />
            </div>

          </div>
        </template>

        <!-- WHITEBOARD TAB -->
        <template v-else-if="tab === 'whiteboard'">
          <div class="flex-1 flex flex-col gap-4 min-h-0 max-w-xl mx-auto w-full">

            <!-- Input area -->
            <div class="flex gap-2 flex-shrink-0">
              <input v-model="wbTextInput" @keyup.enter="addWbText" @paste="onWbPaste"
                placeholder="输入文字或粘贴图片…"
                class="flex-1 rounded-lg px-3 py-2 text-sm focus:outline-none transition-colors"
                style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)" />
              <button @click="addWbText"
                :disabled="!wbTextInput.trim()"
                :style="wbTextInput.trim()
                  ? 'background:var(--accent);color:#fff'
                  : 'background:var(--bg-muted);color:var(--text-faint);cursor:not-allowed'"
                class="px-4 py-2 rounded-lg text-sm font-medium transition-colors flex-shrink-0">
                添加
              </button>
            </div>

            <!-- Clear button -->
            <div class="flex items-center justify-between flex-shrink-0">
              <span class="text-xs" style="color:var(--text-muted)">共 {{ whiteboardItems.length }} 条</span>
              <div v-if="whiteboardItems.length > 0" class="flex items-center gap-2">
                <template v-if="!wbClearConfirm">
                  <button @click="wbClearConfirm = true"
                    class="px-3 py-1 rounded-lg text-xs transition-colors"
                    style="background:rgba(239,68,68,0.1);color:#f87171">
                    🗑 清空白板
                  </button>
                </template>
                <template v-else>
                  <span class="text-xs" style="color:#f87171">确定清空？此操作将同步到所有设备</span>
                  <button @click="clearWhiteboard"
                    class="px-3 py-1 rounded-lg text-xs font-medium text-white"
                    style="background:#dc2626">确定</button>
                  <button @click="wbClearConfirm = false"
                    class="px-2 py-1 rounded-lg text-xs"
                    style="color:var(--text-muted)">取消</button>
                </template>
              </div>
            </div>

            <!-- Item list -->
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
                    <!-- Text content -->
                    <p v-if="item.content_type === 'Text' && item.text"
                      class="text-sm whitespace-pre-wrap break-words"
                      style="color:var(--text-primary)">{{ item.text }}</p>
                    <!-- Image content -->
                    <img v-if="item.content_type === 'Image' && item.image_b64"
                      :src="'data:image/png;base64,' + item.image_b64"
                      class="max-w-full max-h-48 rounded-lg object-contain"
                      style="background:var(--bg-muted)" />
                    <!-- Meta -->
                    <div class="flex items-center gap-2 mt-1.5 text-[11px]" style="color:var(--text-faint)">
                      <span>{{ fmtWbTime(item.timestamp) }}</span>
                      <span v-if="item.source_device" class="px-1.5 py-0.5 rounded-full"
                        style="background:var(--accent-bg);color:var(--accent)">
                        {{ item.source_device }}
                      </span>
                    </div>
                  </div>
                  <button @click="deleteWbItem(item.id)"
                    class="text-xs flex-shrink-0 opacity-0 group-hover:opacity-100 transition-opacity p-1 rounded-lg"
                    style="color:var(--text-muted)"
                    title="删除">🗑</button>
                </div>
              </div>
            </div>

          </div>
        </template>

        <!-- SETTINGS TAB -->
        <template v-else-if="tab === 'settings'">
          <div class="flex-1 flex flex-col gap-5 max-w-md mx-auto w-full">
            <h2 class="font-medium text-sm flex-shrink-0" style="color:var(--text-secondary)">⚙️ 设置</h2>

            <!-- Clipboard sync section (Task 10.2) -->
            <div class="rounded-xl p-4 space-y-3 flex-shrink-0"
              style="background:var(--bg-card);box-shadow:0 0 0 1px var(--border)">
              <p class="text-xs font-medium" style="color:var(--text-secondary)">📋 共享剪贴板</p>

              <label class="flex items-center justify-between text-xs cursor-pointer"
                style="color:var(--text-secondary)">
                <span>启用剪贴板同步</span>
                <input type="checkbox" :checked="clipSyncEnabled"
                  @change="toggleClipSync(!clipSyncEnabled)" class="accent-cyan-500" />
              </label>

              <div v-if="syncGroupConfig.peers.length > 0" class="space-y-1 pt-1">
                <p class="text-xs" style="color:var(--text-faint)">同步设备 ({{ syncGroupConfig.peers.length }})</p>
                <div v-for="peer in syncGroupConfig.peers" :key="peer.device_name"
                  class="flex items-center gap-2 text-xs py-1">
                  <div class="w-2 h-2 rounded-full flex-shrink-0"
                    :style="peer.online ? 'background:#4ade80' : 'background:var(--text-faint)'"></div>
                  <span class="flex-1 truncate" style="color:var(--text-secondary)">{{ shortName(peer.device_name) }}</span>
                  <span class="text-[10px]" style="color:var(--text-faint)">{{ peer.online ? '在线' : '离线' }}</span>
                </div>
              </div>
              <p v-else class="text-xs" style="color:var(--text-faint)">在"设备"页面中添加共享设备</p>
            </div>

            <!-- Update section -->
            <div class="rounded-xl p-4 space-y-3 flex-shrink-0"
              style="background:var(--bg-card);box-shadow:0 0 0 1px var(--border)">
              <p class="text-xs font-medium" style="color:var(--text-secondary)">🚀 自动更新</p>

              <label class="flex items-center justify-between text-xs cursor-pointer"
                style="color:var(--text-secondary)">
                <span>启动时自动检查更新</span>
                <input type="checkbox" v-model="updateSettings.auto_check"
                  @change="saveUpdateSettings" class="accent-cyan-500" />
              </label>

              <label class="flex items-center justify-between text-xs cursor-pointer"
                style="color:var(--text-secondary)">
                <span>发现新版本后自动在后台下载并安装</span>
                <input type="checkbox" v-model="updateSettings.auto_install"
                  @change="saveUpdateSettings" class="accent-cyan-500" />
              </label>

              <!-- Current version + check button -->
              <div class="flex items-center justify-between pt-1">
                <span class="text-xs" style="color:var(--text-faint)">当前版本: v{{ '0.3.41' }}</span>
                <button @click="manualCheckUpdate" :disabled="updateChecking"
                  :style="updateChecking
                    ? 'background:var(--bg-muted);color:var(--text-faint);cursor:not-allowed'
                    : 'background:var(--accent);color:#fff'"
                  class="px-3 py-1.5 rounded-lg text-xs font-medium transition-colors">
                  {{ updateChecking ? '检查中…' : '立即检查' }}
                </button>
              </div>

              <!-- Update available card -->
              <div v-if="updateInfo"
                class="rounded-lg p-3 space-y-2"
                style="background:rgba(34,197,94,0.08);box-shadow:0 0 0 1px rgba(34,197,94,0.2)">
                <div class="flex items-center justify-between">
                  <span class="text-xs font-medium" style="color:#4ade80">🚀 新版本 {{ updateInfo.version }}</span>
                  <button @click="startInstall" :disabled="!!updateProgress"
                    :style="updateProgress
                      ? 'background:var(--bg-muted);color:var(--text-faint);cursor:not-allowed'
                      : 'background:#16a34a;color:#fff'"
                    class="px-3 py-1 rounded-lg text-xs font-medium transition-colors">
                    {{ updateProgress ? '下载中…' : '立即安装' }}
                  </button>
                </div>
                <p v-if="updateInfo.release_notes" class="text-xs leading-relaxed"
                  style="color:var(--text-muted)">{{ updateInfo.release_notes.split('\n').slice(0,3).join(' · ') }}</p>
                <!-- Download progress -->
                <div v-if="updateProgress && !updateProgress.done" class="space-y-1">
                  <div class="w-full rounded-full h-1.5 overflow-hidden" style="background:var(--bg-muted)">
                    <div class="h-1.5 rounded-full transition-all duration-200" style="background:#4ade80"
                      :style="{ width: updateProgress.total ? Math.round(updateProgress.downloaded/updateProgress.total*100)+'%' : '0%' }"></div>
                  </div>
                  <div class="flex justify-between text-xs" style="color:var(--text-faint)">
                    <span>{{ fmtBytes(updateProgress.downloaded) }}</span>
                    <span>{{ fmtBytes(updateProgress.total) }}</span>
                  </div>
                </div>
                <p v-if="updateProgress?.done" class="text-xs" style="color:#4ade80">✅ 下载完成，安装程序已启动</p>
              </div>
            </div>
          </div>
        </template>

      </main>
    </div>

    <footer class="text-center text-[11px] py-1.5"
      style="color:var(--text-faint);border-top:1px solid var(--border);background:var(--bg-surface)">
      rust-air v0.3 · E2EE · mDNS · SHA-256 · 自动更新 · 快捷键 1-7 切换标签
    </footer>

    <!-- Toast notifications (Task 10.4) -->
    <div class="fixed top-16 right-4 z-50 flex flex-col gap-2 pointer-events-none" style="max-width:320px">
      <transition-group name="toast">
        <div v-for="t in toasts" :key="t.id"
          class="rounded-xl px-4 py-3 text-xs shadow-lg pointer-events-auto flex items-start gap-2"
          :style="t.kind === 'sync_received'
            ? 'background:rgba(34,197,94,0.15);box-shadow:0 0 0 1px rgba(34,197,94,0.3);color:#86efac'
            : 'background:rgba(239,68,68,0.15);box-shadow:0 0 0 1px rgba(239,68,68,0.3);color:#fca5a5'">
          <span class="flex-shrink-0 text-sm">{{ t.kind === 'sync_received' ? '📋' : t.kind === 'size_limit' ? '📏' : t.kind === 'checksum_failed' ? '🔒' : '⚠️' }}</span>
          <div class="flex-1 min-w-0">
            <p class="leading-relaxed">{{ t.message }}</p>
            <p v-if="t.device" class="mt-0.5" style="opacity:0.7">{{ t.device }}</p>
          </div>
        </div>
      </transition-group>
    </div>
  </div>
</template>

<style>
@keyframes slide {
  0%   { transform: translateX(-100%); }
  50%  { transform: translateX(150%); }
  100% { transform: translateX(150%); }
}

/* Toast transitions (Task 10.4) */
.toast-enter-active { transition: all 0.3s ease-out; }
.toast-leave-active { transition: all 0.3s ease-in; }
.toast-enter-from   { opacity: 0; transform: translateX(40px); }
.toast-leave-to     { opacity: 0; transform: translateX(40px); }
.toast-move         { transition: transform 0.3s ease; }
</style>
