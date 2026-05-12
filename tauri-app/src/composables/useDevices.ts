import { computed, ref } from "vue"
import { invoke } from "@tauri-apps/api/core"
import type { Device } from "../types/app"

export function useDevices() {
  const devices = ref<Device[]>([])
  const scanning = ref(false)
  const myPort = ref(0)
  const localIps = ref<string[]>([])
  const ipCopied = ref(false)

  const primaryIp = computed(() => localIps.value[0] ?? "")

  function peersOnlyFor(primaryIpValue: string) {
    return devices.value.filter((d) => d.addr && !d.addr.startsWith(`${primaryIpValue}:`))
  }

  async function initListenerAndIps() {
    myPort.value = await invoke<number>("start_listener")
    localIps.value = await invoke<string[]>("get_local_ips")
  }

  function onDeviceFound(device: Device) {
    const dev = { ...device, lastSeen: Date.now() }
    const idx = devices.value.findIndex((d) => d.name === dev.name)
    if (!dev.addr) {
      if (idx >= 0) devices.value.splice(idx, 1)
    } else if (idx >= 0) {
      devices.value[idx] = dev
    } else {
      devices.value.push(dev)
    }
  }

  async function startScan() {
    scanning.value = true
    await invoke("scan_devices")
    setTimeout(() => {
      scanning.value = false
    }, 8000)
  }

  async function copyIp(isAndroid: boolean) {
    const addr = primaryIp.value
    if (!addr) return
    if (isAndroid) {
      await navigator.clipboard?.writeText(addr).catch(() => {})
    } else {
      await invoke("write_clipboard", { text: addr }).catch(() => navigator.clipboard?.writeText(addr).catch(() => {}))
    }
    ipCopied.value = true
    setTimeout(() => {
      ipCopied.value = false
    }, 1500)
  }

  async function refreshIps() {
    localIps.value = await invoke<string[]>("get_local_ips")
  }

  return {
    devices,
    scanning,
    myPort,
    localIps,
    primaryIp,
    ipCopied,
    peersOnlyFor,
    initListenerAndIps,
    onDeviceFound,
    startScan,
    copyIp,
    refreshIps,
  }
}
