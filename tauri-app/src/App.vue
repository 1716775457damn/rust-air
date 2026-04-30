<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { platform } from "@tauri-apps/plugin-os";

// ── Types ─────────────────────────────────────────────────────────────────────

interface Device { name: string; addr: string; status: "Idle" | "Busy"; lastSeen?: number }
interface TransferEvent { bytes_done: number; total_bytes: number; bytes_per_sec: number; done: boolean; resumed?: boolean; resume_offset?: number; reconnect_info?: { attempt: number; max_attempts: number }; error?: string }
interface MatchLine { line_num: number; line: string; ranges: [number,number][] }
interface FileResult { path: string; icon: string; matches: MatchLine[] }
interface SearchEvent { kind: string; path?: string; icon?: string; matches?: MatchLine[]; ms?: number; total?: number; msg?: string }
interface SyncConfig { src: string; dst: string; delete_removed: boolean; excludes: string[]; auto_watch: boolean }
interface SyncStatus { last_sync: string | null; total_files: number; total_bytes: string; is_running: boolean; is_watching: boolean }
interface SyncEventPayload { kind: string; rel?: string; bytes?: number; err?: string; scanned?: number; total?: number; total_files?: number; total_bytes?: number }

interface UpdateInfo { version: string; url: string; size: number; release_notes: string }
interface UpdateProgress { downloaded: number; total: number; done: boolean }
interface UpdateSettings { auto_check: boolean; auto_install: boolean }

interface TodoItem { id: number; title: string; date: string; completed: boolean }

// Shared clipboard sync types (Task 10.1)
interface SyncGroupConfig { enabled: boolean; peers: SyncPeer[] }
interface SyncPeer { device_name: string; addr: string; last_seen: number; online: boolean }
interface ClipSyncError { kind: string; message: string; device?: string }
interface ClipSyncReceived { source_device: string; content_type: string }
interface ClipEntryView { id: number; kind: string; preview: string; stats: string; time_str: string; pinned: boolean; char_count: number; image_b64?: string; source_device?: string }

// Shared whiteboard types
interface WhiteboardItem { id: string; content_type: "Text" | "Image"; text?: string; image_b64?: string; timestamp: number; source_device: string }
interface WhiteboardError { kind: string; message: string; device?: string }

type Tab   = "send" | "receive" | "devices" | "search" | "sync" | "todo" | "whiteboard" | "settings";
type Phase = "idle" | "transferring" | "done" | "error";

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
const sendPhase    = ref<Phase>("idle");
const sendError    = ref("");
const sendProgress = ref<TransferEvent>({ bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false });
const sendPeer     = ref("");
const selectedPath = ref("");
const dragOver     = ref(false);
const sendStartTime = ref(0);

// Receive
const recvPhase    = ref<Phase>("idle");
const recvError    = ref("");
const recvProgress = ref<TransferEvent>({ bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false });
const recvPeer     = ref("");
const savedPath    = ref("");
const recvHistory  = ref<{peer: string; path: string; bytes: number}[]>([]);
const recvReconnecting = ref(false);
const recvReconnectAttempt = ref(0);
const recvReconnectMax = ref(5);

// Devices
const devices  = ref<Device[]>([]);
const scanning = ref(false);
const myPort   = ref(0);

// Search
const searchPattern         = ref("");
const searchPath            = ref(localStorage.getItem("searchPath") || "C:/");
const searchMode            = ref<"filename"|"text">("filename");
const searchIgnoreCase      = ref(true);
const searchFixed           = ref(false);
const searchResults         = ref<FileResult[]>([]);
const searchStatus          = ref("就绪");
const searchRunning         = ref(false);
const searchFilter          = ref("");
const searchFilterDebounced = ref("");
let   searchFilterTimer: ReturnType<typeof setTimeout> | null = null;
function onSearchFilterInput(v: string) {
  searchFilter.value = v;
  if (searchFilterTimer) clearTimeout(searchFilterTimer);
  searchFilterTimer = setTimeout(() => { searchFilterDebounced.value = v; }, 150);
}

// Sync
const syncConfig       = ref<SyncConfig>({ src: "", dst: "", delete_removed: false, excludes: [], auto_watch: false });
const syncStatus       = ref<SyncStatus>({ last_sync: null, total_files: 0, total_bytes: "0 B", is_running: false, is_watching: false });
const syncLog          = ref<string[]>([]);
const syncExcludeInput = ref("");

// IP
const localIps  = ref<string[]>([]);
const primaryIp = computed(() => localIps.value[0] ?? "");
const ipCopied  = ref(false);

// Update
const updateInfo     = ref<UpdateInfo | null>(null);
const updateProgress = ref<UpdateProgress | null>(null);
const updateChecking = ref(false);
const updateSettings = ref<UpdateSettings>({ auto_check: true, auto_install: false });

// Todo
function todayStr(): string {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth()+1).padStart(2,'0')}-${String(d.getDate()).padStart(2,'0')}`;
}
const selectedDate   = ref<string>(todayStr());
const calendarYear   = ref(new Date().getFullYear());
const calendarMonth  = ref(new Date().getMonth() + 1);
const todos          = ref<TodoItem[]>([]);
const todoDates      = ref<string[]>([]);
const newTodoTitle   = ref("");

// Clipboard Sync (Task 10.1)
const clipSyncEnabled = ref(false);
const syncGroupConfig = ref<SyncGroupConfig>({ enabled: false, peers: [] });
const clipEntries     = ref<ClipEntryView[]>([]);

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
function makeEta(p: TransferEvent): string {
  if (!p.total_bytes || !p.bytes_per_sec || p.bytes_done >= p.total_bytes) return "";
  const secs = Math.ceil((p.total_bytes - p.bytes_done) / p.bytes_per_sec);
  if (secs < 60)   return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs/60)}m${secs%60}s`;
  return `${Math.floor(secs/3600)}h${Math.floor((secs%3600)/60)}m`;
}

const sendPct   = computed(() => makePct(sendProgress.value));
const sendSpeed = computed(() => makeSpeed(sendProgress.value));
const sendEta   = computed(() => makeEta(sendProgress.value));
const recvPct   = computed(() => makePct(recvProgress.value));
const recvSpeed = computed(() => makeSpeed(recvProgress.value));
const recvEta   = computed(() => makeEta(recvProgress.value));

const sendIndeterminate = computed(() => sendPhase.value === "transferring" && !sendProgress.value.total_bytes);
const recvIndeterminate = computed(() => recvPhase.value === "transferring" && !recvProgress.value.total_bytes);

const peersOnly = computed(() =>
  devices.value.filter(d => d.addr && !d.addr.startsWith(primaryIp.value + ":"))
);

const selectedName = computed(() => selectedPath.value.split(/[\/\\]/).pop() ?? selectedPath.value);

const filteredResults = computed(() => {
  const q = searchFilterDebounced.value.trim().toLowerCase();
  return q ? searchResults.value.filter(r => r.path.toLowerCase().includes(q)) : searchResults.value;
});

const hlCache = new Map<string, { text: string; hl: boolean }[]>();
function cachedHighlight(path: string, lineNum: number, line: string, ranges: [number,number][]) {
  const key = `${path}:${lineNum}`;
  if (!hlCache.has(key)) hlCache.set(key, highlightSegments(line, ranges));
  return hlCache.get(key)!;
}
watch(searchResults, () => hlCache.clear());

// ── Calendar computed ─────────────────────────────────────────────────────────

interface CalendarDay { day: number; current: boolean; dateStr: string }

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

  myPort.value    = await invoke<number>("start_listener");
  localIps.value  = await invoke<string[]>("get_local_ips");

  window.addEventListener("keydown", onKeyDown);

  unlisten.value.push(
    await listen<TransferEvent>("send-progress", (e) => {
      sendProgress.value = e.payload;
      sendPhase.value = "transferring";
    }),
    await listen<string>("send-peer-connected", (e) => {
      sendPeer.value = e.payload;
      sendStartTime.value = Date.now();
    }),
    await listen("send-done", () => {
      sendPhase.value = "done";
      setTimeout(() => { if (sendPhase.value === "done") resetSend(); }, 4000);
    }),
    await listen<string>("send-error", (e) => { sendError.value = e.payload; sendPhase.value = "error"; }),

    await listen<string>("recv-peer-connected", (e) => {
      recvPeer.value = e.payload;
      recvPhase.value = "transferring";
      tab.value = "receive";
    }),
    await listen<TransferEvent>("recv-progress", (e) => {
      recvProgress.value = e.payload;
      if (e.payload.reconnect_info) {
        recvReconnecting.value = true;
        recvReconnectAttempt.value = e.payload.reconnect_info.attempt;
        recvReconnectMax.value = e.payload.reconnect_info.max_attempts;
      } else {
        recvReconnecting.value = false;
      }
    }),
    await listen<string>("recv-done", (e) => {
      savedPath.value = e.payload ?? "";
      recvHistory.value.unshift({ peer: recvPeer.value, path: savedPath.value, bytes: recvProgress.value.bytes_done });
      recvPhase.value = "done";
      const filename = savedPath.value.split(/[\/\\]/).pop() ?? savedPath.value;
      new Notification("rust-air — 文件已接收", { body: filename, silent: false });
    }),
    await listen<string>("recv-error", (e) => { recvError.value = e.payload; recvPhase.value = "error"; }),

    await listen<Device>("device-found", (e) => {
      const dev = { ...e.payload, lastSeen: Date.now() };
      const idx = devices.value.findIndex(d => d.name === dev.name);
      if (!dev.addr) { if (idx >= 0) devices.value.splice(idx, 1); }
      else if (idx >= 0) { devices.value[idx] = dev; }
      else { devices.value.push(dev); }
    }),

    await listen<SyncEventPayload>("sync-event", (e) => {
      const ev = e.payload;
      if (ev.kind === "Copied")
        syncLog.value.unshift(`✅ ${ev.rel}  (${fmtBytes(ev.bytes ?? 0)})`);
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
    await listen<UpdateInfo>("update-available", (e) => {
      updateInfo.value = e.payload;
    }),
    await listen<UpdateProgress>("update-progress", (e) => {
      updateProgress.value = e.payload;
    }),

    // Clipboard sync events (Task 10.1 & 10.4)
    await listen<ClipEntryView[]>("clip-update", (e) => {
      clipEntries.value = e.payload;
    }),
    await listen<ClipSyncError>("clip-sync-error", (e) => {
      const err = e.payload;
      const kindLabel = err.kind === "size_limit" ? "大小超限"
        : err.kind === "transfer_failed" ? "传输失败"
        : err.kind === "checksum_failed" ? "校验失败"
        : err.kind;
      showToast(err.kind, `${kindLabel}: ${err.message}`, err.device ?? undefined);
    }),
    await listen<ClipSyncReceived>("clip-sync-received", (e) => {
      showToast("sync_received", `已接收来自 ${e.payload.source_device} 的剪贴板内容`);
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
    syncConfig.value = await invoke<SyncConfig>("get_sync_config");
    syncStatus.value = await invoke<SyncStatus>("get_sync_status");
    const defaultEx  = await invoke<string[]>("get_default_excludes");
    if (syncConfig.value.excludes.length === 0) syncConfig.value.excludes = defaultEx;

    updateSettings.value = await invoke<UpdateSettings>("get_update_settings");
  }

  // Load clipboard sync state (Task 10.1)
  try {
    syncGroupConfig.value = await invoke<SyncGroupConfig>("get_sync_group");
    clipSyncEnabled.value = syncGroupConfig.value.enabled;
  } catch (_) { /* sync commands may not be registered yet */ }

  // Load initial todo data
  await loadTodos(selectedDate.value);
  await loadTodoDates(calendarYear.value, calendarMonth.value);

  // Load initial whiteboard data and start flush timer
  if (!isAndroid.value) {
    try { whiteboardItems.value = await invoke<WhiteboardItem[]>("get_whiteboard_items"); }
    catch (_) { /* whiteboard commands may not be registered yet */ }
    setInterval(() => { invoke("flush_whiteboard").catch(() => {}); }, 3000);
  }

  startScan();
});

onUnmounted(async () => {
  window.removeEventListener("keydown", onKeyDown);
  unlisten.value.forEach(fn => fn());
  await invoke("cancel_search").catch(() => {});
});

// ── Keyboard shortcuts ────────────────────────────────────────────────────────

const TAB_KEYS: Record<string, Tab> = { "1": "send", "2": "receive", "3": "devices", "4": "search", "5": "sync", "6": "todo", "7": "whiteboard" };
function onKeyDown(e: KeyboardEvent) {
  if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;
  if (TAB_KEYS[e.key]) { tab.value = TAB_KEYS[e.key]; e.preventDefault(); }
}

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

async function retrySend() {
  sendPhase.value = "transferring"; sendError.value = "";
  sendProgress.value = { bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false };
  try {
    await invoke("retry_send");
  } catch (e: any) { sendError.value = String(e); sendPhase.value = "error"; }
}

function resetRecv() {
  recvPhase.value = "idle"; recvPeer.value = ""; recvError.value = ""; savedPath.value = "";
  recvProgress.value = { bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false };
  recvReconnecting.value = false; recvReconnectAttempt.value = 0;
}

// ── Devices ───────────────────────────────────────────────────────────────────

async function startScan() {
  scanning.value = true;
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
    localStorage.setItem("searchPath", searchPath.value);
  }
}

async function openPath(p: string) { await invoke("open_path", { path: p }).catch(() => {}); }
async function revealInFolder(p: string) {
  const dir = p.replace(/[\/\\][^\/\\]+$/, "") || p;
  await openPath(dir);
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

// ── Update ───────────────────────────────────────────────────────────────────

async function manualCheckUpdate() {
  updateChecking.value = true;
  try {
    const info = await invoke<UpdateInfo | null>("check_update");
    if (info) updateInfo.value = info;
    else alert("已是最新版本 ✅");
  } catch (e: any) {
    alert(`检查更新失败: ${e}`);
  } finally {
    updateChecking.value = false;
  }
}

async function startInstall() {
  if (!updateInfo.value) return;
  updateProgress.value = { downloaded: 0, total: updateInfo.value.size, done: false };
  try {
    await invoke("download_and_install", { url: updateInfo.value.url, size: updateInfo.value.size });
  } catch (e: any) {
    updateProgress.value = null;
    alert(`更新失败: ${e}`);
  }
}

async function saveUpdateSettings() {
  await invoke("save_update_settings", { settings: updateSettings.value });
}

// ── Clipboard Sync IPC (Task 10.2) ──────────────────────────────────────────

async function toggleClipSync(enabled: boolean) {
  clipSyncEnabled.value = enabled;
  try {
    await invoke("set_clip_sync_enabled", { enabled });
    syncGroupConfig.value = await invoke<SyncGroupConfig>("get_sync_group");
  } catch (e: any) { console.error("toggleClipSync:", e); }
}

function isPeerInSyncGroup(deviceName: string): boolean {
  return syncGroupConfig.value.peers.some(p => p.device_name === deviceName);
}

async function toggleSyncPeer(dev: Device) {
  try {
    if (isPeerInSyncGroup(dev.name)) {
      await invoke("remove_sync_peer", { deviceName: dev.name });
    } else {
      await invoke("add_sync_peer", { deviceName: dev.name, addr: dev.addr });
    }
    syncGroupConfig.value = await invoke<SyncGroupConfig>("get_sync_group");
  } catch (e: any) { console.error("toggleSyncPeer:", e); }
}

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

// ── IP ────────────────────────────────────────────────────────────────────────

async function copyIp() {
  const addr = primaryIp.value; if (!addr) return;
  if (isAndroid.value) {
    await navigator.clipboard?.writeText(addr).catch(() => {});
  } else {
    await invoke("write_clipboard", { text: addr }).catch(() => navigator.clipboard?.writeText(addr).catch(() => {}));
  }
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
function shortName(fullname: string) { return fullname.split(".")[0] ?? fullname; }

const now = ref(Date.now());
let _nowTimer: ReturnType<typeof setInterval>;
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

function highlightSegments(line: string, ranges: [number,number][]) {
  const chars = [...line];
  const out: { text: string; hl: boolean }[] = [];
  let pos = 0;
  for (const [s, e] of ranges) {
    if (s > pos) out.push({ text: chars.slice(pos, s).join(""), hl: false });
    out.push({ text: chars.slice(s, e).join(""), hl: true });
    pos = e;
  }
  if (pos < chars.length) out.push({ text: chars.slice(pos).join(""), hl: false });
  return out;
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
      <button v-if="primaryIp" @click="copyIp"
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
              <!-- Reconnect banner -->
              <div v-if="recvReconnecting"
                class="flex items-center gap-2 mb-3 px-3 py-2 rounded-lg text-xs"
                style="background:rgba(251,191,36,0.1);border:1px solid rgba(251,191,36,0.25)">
                <span class="animate-pulse">🔄</span>
                <span style="color:#fbbf24">重连中 (第 {{ recvReconnectAttempt }} 次 / 共 {{ recvReconnectMax }} 次)</span>
              </div>
              <!-- Resume indicator -->
              <div v-if="recvProgress.resumed && recvProgress.resume_offset && !recvReconnecting"
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
          <div class="flex-1 flex flex-col gap-4 max-w-lg mx-auto w-full">
            <div class="flex items-center justify-between">
              <h2 class="font-medium text-sm" style="color:var(--text-secondary)">局域网设备</h2>
              <button @click="startScan"
                :style="scanning ? `color:var(--accent);background:var(--bg-muted)` : `background:var(--bg-muted);color:var(--text-secondary)`"
                :class="['px-3 py-1.5 rounded-lg text-xs transition-colors', scanning ? 'animate-pulse' : '']">
                {{ scanning ? '扫描中…' : '🔄 扫描' }}
              </button>
            </div>

            <!-- Clipboard sync global toggle (Task 10.2) -->
            <div class="rounded-xl p-3 flex items-center justify-between"
              style="background:var(--bg-card);box-shadow:0 0 0 1px var(--border)">
              <div class="flex items-center gap-2">
                <span class="text-sm">📋</span>
                <span class="text-xs" style="color:var(--text-secondary)">共享剪贴板</span>
              </div>
              <label class="relative inline-flex items-center cursor-pointer">
                <input type="checkbox" :checked="clipSyncEnabled" @change="toggleClipSync(!clipSyncEnabled)" class="sr-only peer" />
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
              <!-- Sync peer toggle button (Task 10.2) -->
              <button @click.stop="toggleSyncPeer(dev)"
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

        <!-- SEARCH TAB -->
        <template v-else-if="tab === 'search'">
          <div class="flex-1 flex flex-col gap-3 min-h-0">
            <!-- Controls -->
            <div class="flex items-center gap-2 flex-shrink-0 flex-wrap">
              <div class="flex items-center gap-1 flex-shrink-0">
                <input v-model="searchPath" placeholder="搜索路径" :title="searchPath"
                  class="w-44 rounded-lg px-2 py-1.5 text-xs focus:outline-none transition-colors"
                  style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)" />
                <button @click="pickSearchPath"
                  class="px-2 py-1.5 rounded-lg text-xs transition-colors"
                  style="background:var(--bg-muted);color:var(--text-secondary)" title="选择目录">📂</button>
              </div>
              <select v-model="searchMode"
                class="rounded-lg px-2 py-1.5 text-xs focus:outline-none flex-shrink-0"
                style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)">
                <option value="filename">🗂 文件名</option>
                <option value="text">📄 文本</option>
              </select>
              <input v-model="searchPattern" @keyup.enter="doSearch" placeholder="搜索内容…"
                class="flex-1 min-w-[120px] rounded-lg px-3 py-1.5 text-sm focus:outline-none transition-colors"
                style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)" />
              <label class="flex items-center gap-1 text-xs cursor-pointer flex-shrink-0" style="color:var(--text-secondary)">
                <input type="checkbox" v-model="searchIgnoreCase" class="accent-cyan-500" />忽略大小写
              </label>
              <label class="flex items-center gap-1 text-xs cursor-pointer flex-shrink-0" style="color:var(--text-secondary)">
                <input type="checkbox" v-model="searchFixed" class="accent-cyan-500" />纯文本
              </label>
              <button v-if="!searchRunning" @click="doSearch"
                class="px-3 py-1.5 rounded-lg text-xs font-medium transition-colors flex-shrink-0 text-white"
                style="background:var(--accent)">🔍 搜索</button>
              <button v-else @click="stopSearch"
                class="px-3 py-1.5 rounded-lg text-xs transition-colors flex-shrink-0 text-white"
                style="background:#b91c1c">⏹ 取消</button>
            </div>
            <!-- Status + filter -->
            <div class="flex items-center gap-2 flex-shrink-0">
              <span class="text-xs flex-1" style="color:var(--text-muted)">{{ searchStatus }}</span>
              <input v-if="searchResults.length > 0" :value="searchFilter"
                @input="onSearchFilterInput(($event.target as HTMLInputElement).value)"
                placeholder="过滤结果…"
                class="w-36 rounded-lg px-2 py-1 text-xs focus:outline-none transition-colors"
                style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)" />
            </div>
            <!-- Results -->
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
                  <button @click="revealInFolder(r.path)"
                    class="text-xs font-mono truncate flex-1 text-left hover:underline transition-colors"
                    style="color:var(--accent)" :title="r.path">{{ r.path }}</button>
                  <span class="text-xs flex-shrink-0" style="color:var(--text-faint)">{{ r.matches.length }} 处</span>
                  <button @click="revealInFolder(r.path)"
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

        <!-- SYNC TAB -->
        <template v-else-if="tab === 'sync'">
          <div class="flex-1 flex flex-col gap-4 min-h-0 max-w-xl mx-auto w-full">
            <!-- Status bar -->
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
            <!-- Config -->
            <div class="space-y-3 flex-shrink-0">
              <div class="flex gap-2 items-center">
                <span class="text-xs w-8 flex-shrink-0" style="color:var(--text-muted)">源</span>
                <input v-model="syncConfig.src" placeholder="源目录路径" :title="syncConfig.src"
                  class="flex-1 rounded-lg px-3 py-1.5 text-sm focus:outline-none transition-colors"
                  style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)" />
                <button @click="pickSyncSrc"
                  class="px-2 py-1.5 rounded-lg text-xs transition-colors flex-shrink-0"
                  style="background:var(--bg-muted);color:var(--text-secondary)">📂</button>
              </div>
              <div class="flex gap-2 items-center">
                <span class="text-xs w-8 flex-shrink-0" style="color:var(--text-muted)">目标</span>
                <input v-model="syncConfig.dst" placeholder="目标目录路径" :title="syncConfig.dst"
                  class="flex-1 rounded-lg px-3 py-1.5 text-sm focus:outline-none transition-colors"
                  style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)" />
                <button @click="pickSyncDst"
                  class="px-2 py-1.5 rounded-lg text-xs transition-colors flex-shrink-0"
                  style="background:var(--bg-muted);color:var(--text-secondary)">📂</button>
              </div>
              <label class="flex items-center gap-2 text-xs cursor-pointer" style="color:var(--text-secondary)">
                <input type="checkbox" v-model="syncConfig.delete_removed" class="accent-cyan-500" />删除已移除的文件
              </label>
              <div class="space-y-1">
                <p class="text-xs" style="color:var(--text-muted)">排除规则</p>
                <div class="flex gap-2">
                  <input v-model="syncExcludeInput" @keyup.enter="addExclude" placeholder="*.tmp 或 node_modules"
                    class="flex-1 rounded-lg px-3 py-1 text-xs focus:outline-none transition-colors"
                    style="background:var(--bg-input);border:1px solid var(--border-input);color:var(--text-primary)" />
                  <button @click="addExclude"
                    class="px-2 py-1 rounded text-xs transition-colors"
                    style="background:var(--bg-muted);color:var(--text-secondary)">+</button>
                </div>
                <div class="flex flex-wrap gap-1 mt-1">
                  <span v-for="(ex, i) in syncConfig.excludes" :key="i"
                    class="flex items-center gap-1 text-xs px-2 py-0.5 rounded-full"
                    style="background:var(--bg-muted);color:var(--text-secondary)">
                    {{ ex }}
                    <button @click="removeExclude(i)" class="leading-none" style="color:var(--text-muted)">×</button>
                  </span>
                </div>
              </div>
            </div>
            <!-- Actions -->
            <div class="flex gap-2 flex-shrink-0">
              <button @click="saveAndSync" :disabled="syncStatus.is_running"
                :style="syncStatus.is_running
                  ? `background:var(--bg-muted);color:var(--text-faint);cursor:not-allowed`
                  : `background:var(--accent);color:#fff`"
                class="flex-1 py-2 rounded-lg text-sm font-medium transition-colors">
                {{ syncStatus.is_running ? '同步中…' : '🔄 立即同步' }}
              </button>
              <button @click="toggleWatch"
                :style="syncStatus.is_watching
                  ? 'background:#92400e;color:#fff'
                  : `background:var(--bg-muted);color:var(--text-secondary)`"
                class="px-4 py-2 rounded-lg text-sm transition-colors">
                {{ syncStatus.is_watching ? '⏹ 停止监听' : '👁 实时监听' }}
              </button>
            </div>
            <!-- Log -->
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
                <span class="text-xs" style="color:var(--text-faint)">当前版本: v{{ '0.3.39' }}</span>
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
