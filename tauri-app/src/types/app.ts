export interface Device {
  name: string
  addr: string
  status: "Idle" | "Busy"
  lastSeen?: number
}

export interface ReconnectInfo {
  attempt: number
  max_attempts: number
}

export interface TransferEvent {
  bytes_done: number
  total_bytes: number
  bytes_per_sec: number
  done: boolean
  resumed?: boolean
  resume_offset?: number
  reconnect_info?: ReconnectInfo
  error?: string
}

export interface MatchLine {
  line_num: number
  line: string
  ranges: [number, number][]
}

export interface FileResult {
  path: string
  icon: string
  matches: MatchLine[]
}

export interface SearchEvent {
  kind: string
  path?: string
  icon?: string
  matches?: MatchLine[]
  ms?: number
  total?: number
  msg?: string
}

export interface SyncConfig {
  src: string
  dst: string
  remote_addr: string
  delete_removed: boolean
  excludes: string[]
  auto_watch: boolean
}

export interface SyncStatus {
  last_sync: string | null
  total_files: number
  total_bytes: string
  is_running: boolean
  is_watching: boolean
}

export interface SyncStatsView {
  label: string
  scanned_files: number
  hashed_files: number
  cached_files: number
}

export interface SyncProgressView {
  phase: string
  detail: string
  tone: "idle" | "running" | "done" | "error"
  current: number
  total: number
  push_count: number
  pull_count: number
  delete_count: number
  stats: SyncStatsView[]
}

export interface SyncEventPayload {
  kind: string
  phase?: string
  detail?: string
  current?: number
  push_count?: number
  pull_count?: number
  delete_count?: number
  label?: string
  msg?: string
  rel?: string
  bytes?: number
  err?: string
  scanned?: number
  total?: number
  hashed_files?: number
  cached_files?: number
  scanned_files?: number
  total_files?: number
  total_bytes?: number
}

export interface UpdateInfo {
  version: string
  url: string
  size: number
  release_notes: string
}

export interface UpdateProgress {
  downloaded: number
  total: number
  done: boolean
}

export interface UpdateSettings {
  auto_check: boolean
  auto_install: boolean
}

export interface AppVersionInfo {
  version: string
}

export interface TodoItem {
  id: number
  title: string
  date: string
  completed: boolean
}

export interface SyncPeer {
  device_name: string
  addr: string
  last_seen: number
  online: boolean
}

export interface SyncGroupConfig {
  enabled: boolean
  peers: SyncPeer[]
}

export interface ClipSyncError {
  kind: string
  message: string
  device?: string
}

export interface ClipSyncReceived {
  source_device: string
  content_type: string
}

export interface ClipEntryView {
  id: number
  kind: string
  preview: string
  stats: string
  time_str: string
  pinned: boolean
  char_count: number
  image_b64?: string
  source_device?: string
}

export interface WhiteboardItem {
  id: string
  content_type: "Text" | "Image"
  text?: string
  image_b64?: string
  timestamp: number
  source_device: string
}

export interface WhiteboardError {
  kind: string
  message: string
  device?: string
}

export interface CalendarDay {
  day: number
  current: boolean
  dateStr: string
}

export type Tab =
  | "send"
  | "receive"
  | "devices"
  | "search"
  | "sync"
  | "todo"
  | "whiteboard"
  | "settings"

export type Phase = "idle" | "transferring" | "done" | "error"
