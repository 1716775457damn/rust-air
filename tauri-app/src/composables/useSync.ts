import { ref } from "vue"
import { invoke } from "@tauri-apps/api/core"
import { open } from "@tauri-apps/plugin-dialog"
import type { SyncConfig, SyncEventPayload, SyncStatus } from "../types/app"

export function useSync(fmtBytes: (n: number) => string) {
  const syncConfig = ref<SyncConfig>({ src: "", dst: "", delete_removed: false, excludes: [], auto_watch: false })
  const syncStatus = ref<SyncStatus>({ last_sync: null, total_files: 0, total_bytes: "0 B", is_running: false, is_watching: false })
  const syncLog = ref<string[]>([])
  const syncExcludeInput = ref("")

  async function loadInitialSyncState() {
    syncConfig.value = await invoke<SyncConfig>("get_sync_config")
    syncStatus.value = await invoke<SyncStatus>("get_sync_status")
    const defaultEx = await invoke<string[]>("get_default_excludes")
    if (syncConfig.value.excludes.length === 0) syncConfig.value.excludes = defaultEx
  }

  function onSyncEvent(ev: SyncEventPayload) {
    if (ev.kind === "Copied") syncLog.value.unshift(`✅ ${ev.rel}  (${fmtBytes(ev.bytes ?? 0)})`)
    else if (ev.kind === "Deleted") syncLog.value.unshift(`🗑 ${ev.rel}`)
    else if (ev.kind === "Error") syncLog.value.unshift(`❌ ${ev.rel}: ${ev.err}`)
    else if (ev.kind === "Progress") syncLog.value[0] = `⏳ 扫描中… ${ev.scanned} 个文件`
    else if (ev.kind === "Done") {
      syncLog.value.unshift(`🎉 同步完成  共 ${ev.total_files} 个文件`)
      invoke("sync_done").then(() => invoke<SyncStatus>("get_sync_status").then((s) => (syncStatus.value = s)))
    }
    if (syncLog.value.length > 200) syncLog.value.length = 200
  }

  async function onSyncDone() {
    syncStatus.value = await invoke<SyncStatus>("get_sync_status")
  }

  async function pickSyncSrc() {
    const r = await open({ multiple: false, directory: true })
    if (r) syncConfig.value.src = r as string
  }

  async function pickSyncDst() {
    const r = await open({ multiple: false, directory: true })
    if (r) syncConfig.value.dst = r as string
  }

  async function saveAndSync() {
    await invoke("save_sync_config", { config: syncConfig.value })
    syncLog.value = []
    syncStatus.value.is_running = true
    await invoke("start_sync").catch((e: any) => {
      syncLog.value.unshift(`❌ ${e}`)
      syncStatus.value.is_running = false
    })
  }

  async function toggleWatch() {
    if (syncStatus.value.is_watching) {
      await invoke("stop_watch")
      syncStatus.value.is_watching = false
    } else {
      await invoke("save_sync_config", { config: syncConfig.value })
      await invoke("start_watch").catch((e: any) => syncLog.value.unshift(`❌ ${e}`))
      syncStatus.value.is_watching = true
    }
  }

  function addExclude() {
    const v = syncExcludeInput.value.trim()
    if (v && !syncConfig.value.excludes.includes(v)) syncConfig.value.excludes.push(v)
    syncExcludeInput.value = ""
  }

  function removeExclude(i: number) {
    syncConfig.value.excludes.splice(i, 1)
  }

  return {
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
  }
}
