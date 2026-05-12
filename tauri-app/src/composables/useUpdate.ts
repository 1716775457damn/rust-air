import { ref } from "vue"
import { invoke } from "@tauri-apps/api/core"
import type { UpdateInfo, UpdateProgress, UpdateSettings } from "../types/app"

export function useUpdate(showToast: (kind: string, message: string, device?: string) => void) {
  const currentVersion = ref("")
  const updateInfo = ref<UpdateInfo | null>(null)
  const updateProgress = ref<UpdateProgress | null>(null)
  const updateChecking = ref(false)
  const updateSettings = ref<UpdateSettings>({ auto_check: true, auto_install: false })

  async function loadInitialUpdateState() {
    currentVersion.value = await invoke<string>("get_app_version")
    updateSettings.value = await invoke<UpdateSettings>("get_update_settings")
  }

  function onUpdateAvailable(info: UpdateInfo) {
    updateInfo.value = info
  }

  function onUpdateProgress(progress: UpdateProgress) {
    updateProgress.value = progress
  }

  async function manualCheckUpdate() {
    updateChecking.value = true
    try {
      const info = await invoke<UpdateInfo | null>("check_update")
      if (info) updateInfo.value = info
      else showToast("update_info", "已是最新版本 ✅")
    } catch (e: any) {
      showToast("update_error", `检查更新失败: ${e}`)
    } finally {
      updateChecking.value = false
    }
  }

  async function startInstall() {
    if (!updateInfo.value) return
    updateProgress.value = { downloaded: 0, total: updateInfo.value.size, done: false }
    try {
      await invoke("download_and_install", { url: updateInfo.value.url, size: updateInfo.value.size, digest: updateInfo.value.digest ?? null })
    } catch (e: any) {
      updateProgress.value = null
      showToast("update_error", `更新失败: ${e}`)
    }
  }

  async function saveUpdateSettings() {
    await invoke("save_update_settings", { settings: updateSettings.value })
  }

  return {
    currentVersion,
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
  }
}
