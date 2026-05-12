import { computed, ref, watch } from "vue"
import { invoke } from "@tauri-apps/api/core"
import { open } from "@tauri-apps/plugin-dialog"
import type { FileResult, SearchEvent } from "../types/app"

export function useSearch() {
  const searchPattern = ref("")
  const searchPath = ref(localStorage.getItem("searchPath") || "C:/")
  const searchMode = ref<"filename" | "text">("filename")
  const searchIgnoreCase = ref(true)
  const searchFixed = ref(false)
  const searchResults = ref<FileResult[]>([])
  const searchStatus = ref("就绪")
  const searchRunning = ref(false)
  const searchFilter = ref("")
  const searchFilterDebounced = ref("")
  let searchFilterTimer: ReturnType<typeof setTimeout> | null = null

  function onSearchFilterInput(v: string) {
    searchFilter.value = v
    if (searchFilterTimer) clearTimeout(searchFilterTimer)
    searchFilterTimer = setTimeout(() => {
      searchFilterDebounced.value = v
    }, 150)
  }

  const filteredResults = computed(() => {
    const q = searchFilterDebounced.value.trim().toLowerCase()
    return q ? searchResults.value.filter((r) => r.path.toLowerCase().includes(q)) : searchResults.value
  })

  const hlCache = new Map<string, { text: string; hl: boolean }[]>()

  function highlightSegments(line: string, ranges: [number, number][]) {
    const chars = [...line]
    const out: { text: string; hl: boolean }[] = []
    let pos = 0
    for (const [s, e] of ranges) {
      if (s > pos) out.push({ text: chars.slice(pos, s).join(""), hl: false })
      out.push({ text: chars.slice(s, e).join(""), hl: true })
      pos = e
    }
    if (pos < chars.length) out.push({ text: chars.slice(pos).join(""), hl: false })
    return out
  }

  function cachedHighlight(path: string, lineNum: number, line: string, ranges: [number, number][]) {
    const key = `${path}:${lineNum}`
    if (!hlCache.has(key)) hlCache.set(key, highlightSegments(line, ranges))
    return hlCache.get(key)!
  }

  watch(searchResults, () => hlCache.clear())

  function onSearchBatch(results: FileResult[]) {
    for (const r of results) searchResults.value.push(r)
    searchStatus.value = `搜索中… 已找到 ${searchResults.value.length} 个`
  }

  function onSearchResult(ev: SearchEvent) {
    if (ev.kind === "Done") {
      searchRunning.value = false
      searchStatus.value = searchResults.value.length === 0 ? `未找到结果 (${ev.ms}ms)` : `找到 ${ev.total} 个文件 (${ev.ms}ms)`
    } else if (ev.kind === "Error") {
      searchRunning.value = false
      searchStatus.value = `错误: ${ev.msg}`
    }
  }

  async function doSearch() {
    if (!searchPattern.value.trim()) return
    searchResults.value = []
    searchRunning.value = true
    searchStatus.value = "搜索中…"
    try {
      await invoke("start_search", {
        pattern: searchPattern.value,
        path: searchPath.value,
        ignoreCase: searchIgnoreCase.value,
        fixedString: searchFixed.value,
        mode: searchMode.value,
      })
    } catch (e: any) {
      searchRunning.value = false
      searchStatus.value = `错误: ${e}`
    }
  }

  async function stopSearch() {
    await invoke("cancel_search").catch(() => {})
    searchRunning.value = false
    searchStatus.value = "已取消"
  }

  async function pickSearchPath() {
    const r = await open({ multiple: false, directory: true })
    if (r) {
      searchPath.value = r as string
      localStorage.setItem("searchPath", searchPath.value)
    }
  }

  async function openPath(p: string) {
    await invoke("open_path", { path: p }).catch(() => {})
  }

  async function revealInFolder(p: string) {
    const dir = p.replace(/[\/\\][^\/\\]+$/, "") || p
    await openPath(dir)
  }

  async function cancelSearchOnUnmount() {
    await invoke("cancel_search").catch(() => {})
  }

  return {
    searchPattern,
    searchPath,
    searchMode,
    searchIgnoreCase,
    searchFixed,
    searchResults,
    searchStatus,
    searchRunning,
    searchFilter,
    searchFilterDebounced,
    filteredResults,
    onSearchFilterInput,
    cachedHighlight,
    onSearchBatch,
    onSearchResult,
    doSearch,
    stopSearch,
    pickSearchPath,
    openPath,
    revealInFolder,
    cancelSearchOnUnmount,
  }
}
