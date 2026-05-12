import type { TransferEvent } from "../types/app"

export function todayStr(): string {
  const d = new Date()
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`
}

export function makePct(p: TransferEvent): number | null {
  if (!p.total_bytes) return null
  return Math.min(100, Math.round((p.bytes_done / p.total_bytes) * 100))
}

export function makeSpeed(p: TransferEvent): string {
  const bps = p.bytes_per_sec
  if (bps > 1_000_000) return `${(bps / 1_000_000).toFixed(1)} MB/s`
  if (bps > 1_000) return `${(bps / 1_000).toFixed(0)} KB/s`
  return `${bps} B/s`
}

export function makeEta(p: TransferEvent): string {
  if (!p.total_bytes || !p.bytes_per_sec || p.bytes_done >= p.total_bytes) return ""
  const secs = Math.ceil((p.total_bytes - p.bytes_done) / p.bytes_per_sec)
  if (secs < 60) return `${secs}s`
  if (secs < 3600) return `${Math.floor(secs / 60)}m${secs % 60}s`
  return `${Math.floor(secs / 3600)}h${Math.floor((secs % 3600) / 60)}m`
}
