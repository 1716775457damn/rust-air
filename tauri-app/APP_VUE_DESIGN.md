# App.vue 设计文档

## Tabs
- send: 发送文件
- receive: 接收文件
- devices: 局域网设备扫描
- search: 文件搜索（替换原剪切板 tab）
- sync: 文件同步

## Header
- 左：✈️ rust-air 标题
- 右：本机 IP 显示（点击复制）+ 刷新按钮

## Script 状态变量

### 传输相关
- tab: Tab 类型
- phase: idle/waiting/transferring/done/error
- errorMsg: string
- dragOver, selectedPath, session (SendSession)
- receiveInput, outDir
- progress (TransferEvent), connectedPeer

### 设备
- devices: Device[]

### 搜索（新增，替换剪切板）
- searchPattern: string
- searchPath: string (默认 "C:/")
- searchMode: "filename" | "text"
- searchIgnoreCase: boolean (默认 true)
- searchFixed: boolean
- searchResults: FileResult[]
- searchStatus: string
- searchRunning: boolean
- searchFilter: string
- filteredResults: computed

### 同步
- syncConfig, syncStatus, syncLog, syncExcludeInput

### IP
- localIps, primaryIp (computed), ipCopied

## Interfaces
- Device: name, addr, status
- TransferEvent: bytes_done, total_bytes, bytes_per_sec, done
- SendSession: instance_name, key_b64
- MatchLine: line_num, line, ranges
- FileResult: path, icon, matches
- SearchEvent: kind, path?, icon?, matches?, ms?, total?, msg?
- SyncConfig, SyncStatus, SyncEventPayload

## Events listened
- search-result: SearchEvent → 更新 searchResults/searchStatus/searchRunning
- transfer-progress, transfer-peer-connected, transfer-done, transfer-error
- device-found
- sync-event, sync-done

## Invoke commands
- start_search(pattern, path, ignoreCase, fixedString, mode)
- cancel_search()
- start_send, cancel_send, start_receive, scan_devices
- read_clipboard, write_clipboard, get_local_ips
- get_sync_config, save_sync_config, get_sync_status, get_default_excludes
- start_sync, sync_done, start_watch, stop_watch

## Search Tab UI
- 工具栏：路径输入 + 📂选择 + 模式下拉 + 关键词输入 + 大小写/纯文本复选框 + 搜索/取消按钮
- 状态栏：searchStatus + 过滤输入框
- 结果列表：每条显示 icon + path + 匹配数，文本模式显示前5行匹配内容

## ProgressRing 组件
- props: pct, speed, doneBytes, totalBytes, peer
- SVG 环形进度条，cyan 色

## 样式
- 背景: #0d1117
- 侧边栏/Header: #161b22
- 主色: cyan-500
- 字体: font-sans
