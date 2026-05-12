import { ref } from "vue"
import { invoke } from "@tauri-apps/api/core"
import type { WhiteboardItem } from "../types/app"

export function useWhiteboard(
  isAndroid: () => boolean,
  showToast: (kind: string, message: string, device?: string) => void,
) {
  const whiteboardItems = ref<WhiteboardItem[]>([])
  const wbTextInput = ref("")
  const wbClearConfirm = ref(false)
  const wbEditingId = ref<string | null>(null)
  const wbEditingText = ref("")

  async function loadInitialWhiteboardState() {
    whiteboardItems.value = await invoke<WhiteboardItem[]>("get_whiteboard_items")
  }

  function onWhiteboardUpdate(items: WhiteboardItem[]) {
    whiteboardItems.value = items
  }

  async function addWbText() {
    const text = wbTextInput.value.trim()
    if (!text) return
    try {
      await invoke<WhiteboardItem>("add_whiteboard_text", { text })
      wbTextInput.value = ""
    } catch (e: any) {
      console.error("addWbText:", e)
    }
  }

  function startWbEdit(item: WhiteboardItem) {
    if (item.content_type !== "Text" || !item.text) return
    wbEditingId.value = item.id
    wbEditingText.value = item.text
  }

  function cancelWbEdit() {
    wbEditingId.value = null
    wbEditingText.value = ""
  }

  async function saveWbEdit() {
    const id = wbEditingId.value
    const text = wbEditingText.value.trim()
    if (!id || !text) return
    try {
      await invoke("update_whiteboard_text", { id, text })
      cancelWbEdit()
    } catch (e: any) {
      console.error("saveWbEdit:", e)
    }
  }

  async function addWbImage(b64: string) {
    try {
      await invoke<WhiteboardItem>("add_whiteboard_image", { imageB64: b64 })
    } catch (e: any) {
      console.error("addWbImage:", e)
    }
  }

  async function copyWbText(text: string) {
    if (!text) return
    try {
      if (isAndroid()) {
        await navigator.clipboard?.writeText(text).catch(() => {})
      } else {
        await invoke("write_clipboard", { text }).catch(() => navigator.clipboard?.writeText(text).catch(() => {}))
      }
      showToast("whiteboard_copy", "白板文本已复制")
    } catch (e: any) {
      console.error("copyWbText:", e)
    }
  }

  async function deleteWbItem(id: string) {
    try {
      await invoke("delete_whiteboard_item", { id })
    } catch (e: any) {
      console.error("deleteWbItem:", e)
    }
  }

  async function clearWhiteboard() {
    try {
      await invoke("clear_whiteboard")
      wbClearConfirm.value = false
    } catch (e: any) {
      console.error("clearWhiteboard:", e)
    }
  }

  function onWbPaste(e: ClipboardEvent) {
    const items = e.clipboardData?.items
    if (!items) return
    for (const item of items) {
      if (item.type.startsWith("image/")) {
        e.preventDefault()
        const blob = item.getAsFile()
        if (!blob) return
        const reader = new FileReader()
        reader.onload = () => {
          const result = reader.result as string
          const b64 = result.split(",")[1]
          if (b64) addWbImage(b64)
        }
        reader.readAsDataURL(blob)
        return
      }
    }
  }

  function fmtWbTime(ts: number): string {
    const d = new Date(ts)
    const hh = String(d.getHours()).padStart(2, "0")
    const mm = String(d.getMinutes()).padStart(2, "0")
    const MM = String(d.getMonth() + 1).padStart(2, "0")
    const DD = String(d.getDate()).padStart(2, "0")
    return `${MM}-${DD} ${hh}:${mm}`
  }

  return {
    whiteboardItems,
    wbTextInput,
    wbClearConfirm,
    wbEditingId,
    wbEditingText,
    loadInitialWhiteboardState,
    onWhiteboardUpdate,
    addWbText,
    startWbEdit,
    cancelWbEdit,
    saveWbEdit,
    addWbImage,
    copyWbText,
    deleteWbItem,
    clearWhiteboard,
    onWbPaste,
    fmtWbTime,
  }
}
