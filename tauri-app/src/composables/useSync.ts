import { ref } from "vue"
import { invoke } from "@tauri-apps/api/core"
import { open } from "@tauri-apps/plugin-dialog"
import type { SyncConfig, SyncEventPayload, SyncProgressView, SyncStatsView, SyncStatus } from "../types/app"

export function useSync(fmtBytes: (n: number) => string) {
  const syncConfig = ref<SyncConfig>({ src: "", dst: "", remote_addr: "", delete_removed: false, excludes: [], auto_watch: false })
  const syncStatus = ref<SyncStatus>({ last_sync: null, total_files: 0, total_bytes: "0 B", is_running: false, is_watching: false })
  const syncProgress = ref<SyncProgressView>({ phase: "idle", detail: "等待同步开始", tone: "idle", current: 0, total: 0, push_count: 0, pull_count: 0, delete_count: 0, stats: [] })
  const syncLog = ref<string[]>([])
  const syncErrors = ref<string[]>([])
  const syncExcludeInput = ref("")

  function resetSyncProgress(detail: string) {
    syncProgress.value = {
      phase: "starting",
      detail,
      tone: "running",
      current: 0,
      total: 0,
      push_count: 0,
      pull_count: 0,
      delete_count: 0,
      stats: [],
    }
    syncErrors.value = []
  }

  function upsertSyncStats(stat: SyncStatsView) {
    const idx = syncProgress.value.stats.findIndex((item) => item.label === stat.label)
    if (idx >= 0) syncProgress.value.stats[idx] = stat
    else syncProgress.value.stats.push(stat)
  }

  async function loadInitialSyncState() {
    syncConfig.value = await invoke<SyncConfig>("get_sync_config")
    syncStatus.value = await invoke<SyncStatus>("get_sync_status")
    const defaultEx = await invoke<string[]>("get_default_excludes")
    if (syncConfig.value.excludes.length === 0) syncConfig.value.excludes = defaultEx

    if (syncConfig.value.auto_watch && !syncStatus.value.is_watching) {
      await invoke("start_watch").then(async () => {
        syncStatus.value = await invoke<SyncStatus>("get_sync_status")
      }).catch((e: any) => {
        syncLog.value.unshift(`❌ 自动同步恢复失败: ${e}`)
        syncConfig.value.auto_watch = false
        void invoke("save_sync_config", { config: syncConfig.value })
      })
    }
  }

  function onSyncEvent(ev: SyncEventPayload) {
    if (ev.kind === "Info") syncLog.value.unshift(`ℹ️ ${ev.msg}`)
    else if (ev.kind === "Phase") {
      syncProgress.value.phase = ev.phase ?? "running"
      syncProgress.value.detail = ev.detail ?? "同步阶段更新"
      syncProgress.value.tone = "running"
      syncLog.value.unshift(`🧭 ${ev.detail ?? ev.phase ?? "同步阶段更新"}`)
    }
    else if (ev.kind === "ActionProgress") {
      syncProgress.value.current = ev.current ?? 0
      syncProgress.value.total = ev.total ?? 0
      syncProgress.value.push_count = ev.push_count ?? 0
      syncProgress.value.pull_count = ev.pull_count ?? 0
      syncProgress.value.delete_count = ev.delete_count ?? 0
      if ((ev.total ?? 0) > 0) {
        syncProgress.value.detail = `已执行 ${ev.current ?? 0}/${ev.total ?? 0} 个同步动作`
      }
    }
    else if (ev.kind === "Stats") {
      upsertSyncStats({
        label: ev.label ?? "同步统计",
        scanned_files: ev.scanned_files ?? 0,
        hashed_files: ev.hashed_files ?? 0,
        cached_files: ev.cached_files ?? 0,
      })
      syncLog.value.unshift(`📊 ${ev.label ?? "同步统计"}: 扫描 ${ev.scanned_files ?? 0} / 复用缓存 ${ev.cached_files ?? 0} / 重算哈希 ${ev.hashed_files ?? 0}`)
    }
    else if (ev.kind === "Copied") syncLog.value.unshift(`✅ ${ev.rel}  (${fmtBytes(ev.bytes ?? 0)})`)
    else if (ev.kind === "Deleted") syncLog.value.unshift(`🗑 ${ev.rel}`)
    else if (ev.kind === "Error") {
      syncProgress.value.phase = "error"
      syncProgress.value.detail = ev.err ?? "同步失败"
      syncProgress.value.tone = "error"
      syncErrors.value.unshift(`${ev.rel}: ${ev.err}`)
      if (syncErrors.value.length > 20) syncErrors.value.length = 20
      syncLog.value.unshift(`❌ ${ev.rel}: ${ev.err}`)
    }
    else if (ev.kind === "Progress") syncLog.value[0] = `⏳ 同步处理中… ${ev.scanned} 个文件`
    else if (ev.kind === "Done") {
      syncProgress.value.phase = "done"
      syncProgress.value.detail = `同步完成，共 ${ev.total_files ?? 0} 个文件`
      syncProgress.value.tone = "done"
      syncLog.value.unshift(`🎉 同步完成  共 ${ev.total_files} 个文件`)
    }
    if (syncLog.value.length > 200) syncLog.value.length = 200
  }

  async function onSyncDone() {
    syncStatus.value = await invoke<SyncStatus>("get_sync_status")
    if (syncProgress.value.phase !== "error") {
      syncProgress.value.phase = "idle"
      syncProgress.value.tone = "idle"
      if (syncProgress.value.detail.startsWith("同步完成")) return
      syncProgress.value.detail = "等待同步开始"
    }
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
    resetSyncProgress("准备本地镜像同步")
    syncStatus.value.is_running = true
    await invoke("start_sync").catch((e: any) => {
      syncLog.value.unshift(`❌ ${e}`)
      syncStatus.value.is_running = false
    })
  }

  async function startRemoteSync(callbackAddr: string) {
    await invoke("save_sync_config", { config: syncConfig.value })
    syncLog.value = []
    resetSyncProgress("准备双机同步")
    syncStatus.value.is_running = true
    await invoke("start_remote_sync", { remoteAddr: syncConfig.value.remote_addr, callbackAddr }).catch((e: any) => {
      syncLog.value.unshift(`❌ ${e}`)
      syncStatus.value.is_running = false
    })
    syncStatus.value = await invoke<SyncStatus>("get_sync_status")
  }

  async function toggleWatch() {
    if (syncStatus.value.is_watching) {
      await invoke("stop_watch")
      syncConfig.value.auto_watch = false
      await invoke("save_sync_config", { config: syncConfig.value })
      syncStatus.value.is_watching = false
    } else {
      if (!syncConfig.value.src || !syncConfig.value.dst) {
        syncLog.value.unshift("❌ 本地自动镜像需要同时设置“源目录”和“本地镜像目标”")
        return
      }
      syncConfig.value.auto_watch = true
      await invoke("save_sync_config", { config: syncConfig.value })
      const beforeLog = syncLog.value.length
      await invoke("start_watch").catch((e: any) => syncLog.value.unshift(`❌ ${e}`))
      if (syncLog.value.length > beforeLog && syncLog.value[0]?.startsWith("❌")) {
        syncConfig.value.auto_watch = false
        await invoke("save_sync_config", { config: syncConfig.value })
        return
      }
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
    syncProgress,
    syncLog,
    syncErrors,
    syncExcludeInput,
    loadInitialSyncState,
    onSyncEvent,
    onSyncDone,
    pickSyncSrc,
    pickSyncDst,
    saveAndSync,
    startRemoteSync,
    toggleWatch,
    addExclude,
    removeExclude,
  }
}
