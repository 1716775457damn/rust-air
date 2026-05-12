import { ref } from "vue"
import { invoke } from "@tauri-apps/api/core"
import type {
  ClipEntryView,
  ClipSyncError,
  ClipSyncReceived,
  Device,
  SyncGroupConfig,
} from "../types/app"

export function useClipboardSync(showToast: (kind: string, message: string, device?: string) => void) {
  const clipSyncEnabled = ref(false)
  const syncGroupConfig = ref<SyncGroupConfig>({ enabled: false, peers: [] })
  const clipEntries = ref<ClipEntryView[]>([])

  async function loadInitialClipboardSyncState() {
    syncGroupConfig.value = await invoke<SyncGroupConfig>("get_sync_group")
    clipSyncEnabled.value = syncGroupConfig.value.enabled
  }

  function onClipUpdate(entries: ClipEntryView[]) {
    clipEntries.value = entries
  }

  function onClipSyncError(err: ClipSyncError) {
    const kindLabel = err.kind === "size_limit"
      ? "大小超限"
      : err.kind === "transfer_failed"
        ? "传输失败"
        : err.kind === "checksum_failed"
          ? "校验失败"
          : err.kind
    showToast(err.kind, `${kindLabel}: ${err.message}`, err.device ?? undefined)
  }

  function onClipSyncReceived(received: ClipSyncReceived) {
    showToast("sync_received", `已接收来自 ${received.source_device} 的剪贴板内容`)
  }

  async function toggleClipSync(enabled: boolean) {
    clipSyncEnabled.value = enabled
    try {
      await invoke("set_clip_sync_enabled", { enabled })
      syncGroupConfig.value = await invoke<SyncGroupConfig>("get_sync_group")
    } catch (e: any) {
      console.error("toggleClipSync:", e)
    }
  }

  function isPeerInSyncGroup(deviceName: string): boolean {
    return syncGroupConfig.value.peers.some((p) => p.device_name === deviceName)
  }

  async function toggleSyncPeer(dev: Device) {
    try {
      if (isPeerInSyncGroup(dev.name)) {
        await invoke("remove_sync_peer", { deviceName: dev.name })
      } else {
        await invoke("add_sync_peer", { deviceName: dev.name, addr: dev.addr })
      }
      syncGroupConfig.value = await invoke<SyncGroupConfig>("get_sync_group")
    } catch (e: any) {
      console.error("toggleSyncPeer:", e)
    }
  }

  return {
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
  }
}
