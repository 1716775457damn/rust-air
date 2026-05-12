import { computed, ref } from "vue"
import { invoke } from "@tauri-apps/api/core"
import { open } from "@tauri-apps/plugin-dialog"
import { makeEta, makePct, makeSpeed } from "../utils/transfer"
import type { Device, Phase, TransferEvent } from "../types/app"

export function useTransfer() {
  const sendPhase = ref<Phase>("idle")
  const sendError = ref("")
  const sendProgress = ref<TransferEvent>({ bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false })
  const sendPeer = ref("")
  const selectedPath = ref("")
  const dragOver = ref(false)
  const sendStartTime = ref(0)

  const recvPhase = ref<Phase>("idle")
  const recvError = ref("")
  const recvProgress = ref<TransferEvent>({ bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false })
  const recvPeer = ref("")
  const savedPath = ref("")
  const recvHistory = ref<{ peer: string; path: string; bytes: number }[]>([])

  const sendReconnecting = ref(false)
  const sendReconnectAttempt = ref(0)
  const sendReconnectMax = ref(5)

  const sendPct = computed(() => makePct(sendProgress.value))
  const sendSpeed = computed(() => makeSpeed(sendProgress.value))
  const sendEta = computed(() => makeEta(sendProgress.value))
  const recvPct = computed(() => makePct(recvProgress.value))
  const recvSpeed = computed(() => makeSpeed(recvProgress.value))
  const recvEta = computed(() => makeEta(recvProgress.value))
  const sendIndeterminate = computed(() => sendPhase.value === "transferring" && !sendProgress.value.total_bytes)
  const recvIndeterminate = computed(() => recvPhase.value === "transferring" && !recvProgress.value.total_bytes)
  const selectedName = computed(() => selectedPath.value.split(/[\/\\]/).pop() ?? selectedPath.value)

  async function pickFile() {
    const r = await open({ multiple: false, directory: false })
    if (r) selectedPath.value = r as string
  }

  async function pickFolder() {
    const r = await open({ multiple: false, directory: true })
    if (r) selectedPath.value = r as string
  }

  function onDrop(e: DragEvent) {
    dragOver.value = false
    const f = e.dataTransfer?.files[0]
    if (f) selectedPath.value = (f as any).path ?? f.name
  }

  async function sendToDevice(dev: Device) {
    if (!selectedPath.value) return
    sendPhase.value = "transferring"
    sendError.value = ""
    sendPeer.value = dev.addr
    sendProgress.value = { bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false }
    try {
      await invoke("send_to", { path: selectedPath.value, addr: dev.addr })
    } catch (e: any) {
      sendError.value = String(e)
      sendPhase.value = "error"
    }
  }

  function resetSend() {
    invoke("cancel_send").catch(() => {})
    sendPhase.value = "idle"
    sendPeer.value = ""
    sendError.value = ""
    sendProgress.value = { bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false }
    sendReconnecting.value = false
    sendReconnectAttempt.value = 0
  }

  async function retrySend() {
    sendPhase.value = "transferring"
    sendError.value = ""
    sendProgress.value = { bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false }
    try {
      await invoke("retry_send")
    } catch (e: any) {
      sendError.value = String(e)
      sendPhase.value = "error"
    }
  }

  function resetRecv() {
    recvPhase.value = "idle"
    recvPeer.value = ""
    recvError.value = ""
    savedPath.value = ""
    recvProgress.value = { bytes_done: 0, total_bytes: 0, bytes_per_sec: 0, done: false }
  }

  function onSendProgress(ev: TransferEvent) {
    sendProgress.value = ev
    sendPhase.value = "transferring"
    if (ev.reconnect_info) {
      sendReconnecting.value = true
      sendReconnectAttempt.value = ev.reconnect_info.attempt
      sendReconnectMax.value = ev.reconnect_info.max_attempts
    } else {
      sendReconnecting.value = false
    }
  }

  function onSendPeerConnected(peer: string) {
    sendPeer.value = peer
    sendStartTime.value = Date.now()
  }

  function onSendDone() {
    sendPhase.value = "done"
    setTimeout(() => {
      if (sendPhase.value === "done") resetSend()
    }, 4000)
  }

  function onSendError(err: string) {
    sendError.value = err
    sendPhase.value = "error"
  }

  function onRecvPeerConnected(peer: string) {
    recvPeer.value = peer
    recvPhase.value = "transferring"
  }

  function onRecvProgress(ev: TransferEvent) {
    recvProgress.value = ev
  }

  function onRecvDone(path: string) {
    savedPath.value = path ?? ""
    recvHistory.value.unshift({ peer: recvPeer.value, path: savedPath.value, bytes: recvProgress.value.bytes_done })
    recvPhase.value = "done"
    const filename = savedPath.value.split(/[\/\\]/).pop() ?? savedPath.value
    new Notification("rust-air — 文件已接收", { body: filename, silent: false })
  }

  function onRecvError(err: string) {
    recvError.value = err
    recvPhase.value = "error"
  }

  return {
    sendPhase,
    sendError,
    sendProgress,
    sendPeer,
    selectedPath,
    dragOver,
    sendStartTime,
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
  }
}
