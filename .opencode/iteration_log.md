# Iteration Log

## Iteration 1

### Scope
- Stabilize Tauri backend task/state management for send cancellation and sync runtime flags.

### Changes
- Replaced `oneshot::Sender`-based send cancellation in `tauri-app/src-tauri/src/commands.rs` with a stored `CancellationToken`.
- Wired the same `CancellationToken` through `do_send()` into `transfer::send_path_with_retry()` so user-triggered cancel now reaches the real transfer/retry loop.
- Simplified sync running state in `tauri-app/src-tauri/src/sync_commands.rs` from `Arc<Mutex<bool>>` to `Arc<AtomicBool>`.
- Kept IPC surface unchanged to avoid frontend regressions.

### Validation
- `cargo check -p tauri-app`
- `cargo test -p tauri-app --test update_preservation_test -- --nocapture`
- `cargo test -p rust-air-core --lib -- --nocapture`

### Stability Impact
- User cancel now interrupts the actual transfer retry loop instead of only cancelling the outer wait layer.
- Reduced cross-thread lock contention and state inconsistency risk for sync running status.

### Remaining Issues
- `start_watch()` still uses nested `std::thread::spawn` pipelines with no explicit join/ownership model.
- `App.vue` remains oversized and couples many state domains into a single file.
- Update tests still partially mirror command-generation behavior rather than fully exercising current launcher logic.
- README protocol details are out of sync with implementation constants in at least one place (`CHUNK` size).

### Recommended Next Step
- Continue iteration 2 with sync watcher lifecycle hardening and event-forwarding cleanup.

## Iteration 2

### Scope
- Harden auto-update logic by extracting testable helpers and aligning integration tests with the real launcher behavior.

### Changes
- Exposed `update_commands` module for integration-test access.
- Extracted reusable pure helpers in `tauri-app/src-tauri/src/update_commands.rs`:
  - `apply_download_proxy()`
  - `expected_download_size()`
  - `windows_installer_command()`
- Updated update-related integration tests to validate the real helper output instead of hand-maintained mirror logic.
- Synced Windows EXE tests with the current detached execution model instead of the historical `start ""` assumption.

### Validation
- `cargo test -p tauri-app --test update_preservation_test -- --nocapture`
- `cargo test -p tauri-app --test msi_upgrade_bug_exploration -- --nocapture`
- `cargo check -p tauri-app`

### Stability Impact
- Reduced the risk of update tests passing while the real installer launch path drifts.
- Improved confidence in Windows installer command generation and download size handling.

### Remaining Issues
- `sync_commands.rs` watcher lifecycle still relies on nested blocking threads with limited ownership/termination control.
- `App.vue` remains oversized and should be split into composables/components before further UI feature growth.
- README protocol/performance documentation still has drift from actual constants/behavior.

### Recommended Next Step
- Iteration 3: harden sync watcher lifecycle and event-forwarding ownership.

## Iteration 3

### Scope
- Harden sync watcher lifecycle and stop semantics in the Tauri backend.

### Changes
- Replaced bare `watcher: Mutex<Option<notify::RecommendedWatcher>>` state with an owned `WatchSession`.
- Added explicit watcher session resources:
  - watcher handle
  - stop flag
  - worker join handle
- Updated `start_watch()` to stop any previous watch session before starting a new one.
- Updated `stop_watch()` to explicitly signal stop, drop the watcher, and join the worker thread.
- Added stop checks inside the watch worker and event-forwarding loop to reduce orphaned background activity.

### Validation
- `cargo check -p tauri-app`
- `cargo test -p tauri-app --test update_preservation_test -- --nocapture`
- `cargo test -p rust-air-core --lib -- --nocapture`

### Stability Impact
- Reduced risk of leaked watcher workers after repeated start/stop cycles.
- Prevented multiple concurrent watch sessions from stacking silently.
- Made watcher ownership and teardown more explicit and production-safe.

### Remaining Issues
- `start_sync()` and watcher processing still use blocking threads and `std::sync::mpsc`; they are more stable now, but not yet unified under a single async task model.
- `App.vue` remains monolithic and is now the largest maintainability risk for future iterations.
- README still contains implementation drift and should be reconciled with current protocol/runtime behavior.

### Recommended Next Step
- Iteration 4: split `App.vue` into composables/components while preserving current IPC and event semantics.

## Iteration 4

### Scope
- Begin decomposing the monolithic `App.vue` by extracting shared frontend types and transfer-formatting helpers.

### Changes
- Added `tauri-app/src/types/app.ts` to centralize frontend domain types used by Tauri IPC and UI state.
- Added `tauri-app/src/utils/transfer.ts` for reusable transfer progress formatting helpers:
  - `todayStr()`
  - `makePct()`
  - `makeSpeed()`
  - `makeEta()`
- Updated `tauri-app/src/App.vue` to import the extracted types/utilities and removed duplicated local declarations.

### Validation
- `pnpm install`
- `pnpm exec vue-tsc --noEmit`
- `pnpm build`

### Stability Impact
- Reduced `App.vue` inline type duplication and formatting logic density without changing event registration or IPC wiring.
- Created a safer base for future composable/component extraction.

### Remaining Issues
- `App.vue` is still the dominant complexity hotspot; only low-risk extraction was completed this round.
- Frontend package installation required `pnpm` because local `npm` was unstable in this environment.
- README and protocol docs still need consistency cleanup.

### Recommended Next Step
- Iteration 5: extract sync/update/search state handling from `App.vue` into composables while preserving the current event subscription order.

## Iteration 5

### Scope
- Extract sync and update state/commands from `App.vue` into composables while preserving current IPC names and event subscription order.

### Changes
- Added `tauri-app/src/composables/useSync.ts`.
- Added `tauri-app/src/composables/useUpdate.ts`.
- Moved sync state, initialization, event handlers, config actions, and watch toggling into `useSync()`.
- Moved update state, initialization, event handlers, manual check, install start, and settings persistence into `useUpdate()`.
- Rewired `App.vue` to consume the composables without changing `listen()` ordering or existing Tauri command names.

### Validation
- `pnpm exec vue-tsc --noEmit`
- `pnpm build`

### Stability Impact
- Reduced `App.vue` orchestration density while keeping event semantics stable.
- Created clearer ownership boundaries for sync and update UI state.

### Remaining Issues
- `App.vue` still contains search, transfer, todo, clipboard sync, whiteboard, and shared UI helpers.
- Frontend dependency setup required `pnpm`; local `npm` remained unreliable in this environment.
- Documentation drift and release-note automation remain open.

### Recommended Next Step
- Iteration 6: extract transfer/search state and handlers from `App.vue`, then consider splitting visual sections into presentational components.

## Iteration 6

### Scope
- Extract transfer and search state/handlers from `App.vue` into composables while preserving current event order and IPC semantics.

### Changes
- Added `tauri-app/src/composables/useTransfer.ts`.
- Added `tauri-app/src/composables/useSearch.ts`.
- Moved send/receive state, progress derivations, drag/drop handling, retry/reset logic, and transfer event handlers into `useTransfer()`.
- Moved search state, debounce/filter logic, search result handlers, path picking, and highlight caching into `useSearch()`.
- Rewired `App.vue` to use the new composables without changing Tauri event names or registration order.

### Validation
- `pnpm exec vue-tsc --noEmit`
- `pnpm build`

### Stability Impact
- Further reduced `App.vue` orchestration density.
- Isolated transfer and search state ownership, which lowers regression risk for future iterations.

### Remaining Issues
- `App.vue` still contains todo, clipboard sync, whiteboard, device scan, and general shell/UI helpers.
- Presentational sections are still inline in one large template.
- Documentation drift and release-note automation remain unresolved.

### Recommended Next Step
- Iteration 7: extract device scan / clipboard sync state or begin splitting large template sections into presentational components.

## Iteration 7

### Scope
- Extract device scan/IP state and clipboard sync state from `App.vue` into composables while preserving current event ordering and IPC behavior.

### Changes
- Added `tauri-app/src/composables/useDevices.ts`.
- Added `tauri-app/src/composables/useClipboardSync.ts`.
- Moved device discovery state, scanning, local IP refresh, and copy-IP behavior into `useDevices()`.
- Moved clipboard sync state, peer membership checks, peer toggling, and clipboard sync event handling into `useClipboardSync()`.
- Rewired `App.vue` event handlers (`device-found`, `clip-update`, `clip-sync-error`, `clip-sync-received`) to delegate to composables without changing listener order.

### Validation
- `pnpm exec vue-tsc --noEmit`
- `pnpm build`

### Stability Impact
- Reduced `App.vue` state density again while keeping runtime behavior stable.
- Centralized device/clipboard sync behavior for safer future iteration.

### Remaining Issues
- `App.vue` still owns todo, whiteboard, keyboard shortcuts, and a large combined template.
- Presentational sections are still inline and would benefit from component extraction.
- Non-code project concerns remain: docs drift, release-note automation, and environment-specific npm instability.

### Recommended Next Step
- Iteration 8: begin extracting large template sections into presentational components, starting with devices/search/sync panels.

## Iteration 8

### Scope
- Extract large devices/search/sync template sections into presentational components.
- Perform an adversarial review after the extraction.

### Changes
- Added `tauri-app/src/components/DevicesPanel.vue`.
- Added `tauri-app/src/components/SearchPanel.vue`.
- Added `tauri-app/src/components/SyncPanel.vue`.
- Replaced the corresponding inline template blocks in `App.vue` with prop/event-driven presentation components.
- Added adapter functions in `App.vue` for `search` and `sync` field updates.

### Validation
- `pnpm exec vue-tsc --noEmit`
- `pnpm build`

### Stability Impact
- Reduced template density in `App.vue` significantly while preserving runtime wiring.
- Improved separation between presentation and orchestration layers.

### Adversarial Review Findings
- `core/src/transfer.rs`: `dir_entries.unwrap()` is still a latent panic point even if upstream logic currently guarantees `Some`; replacing it with an explicit error path would make the sender path more production-hard.
- `tauri-app/src/App.vue`: there are still multiple long-lived timers (`setInterval` / `setTimeout`) without centralized lifecycle ownership; they work today but remain easy to regress as more composables appear.
- `tauri-app/src/composables/useUpdate.ts`: user-facing update failures still rely on `alert()`, which is functional but weak for production UX and difficult to integrate into a unified notification/error strategy.
- `README.md`: protocol/performance documentation still drifts from implementation details, which is now one of the larger non-code correctness risks.

### Remaining Issues
- Todo and whiteboard sections are still inline in `App.vue`.
- Release/publish documentation and release note generation remain manually maintained.
- Several test files intentionally use `unwrap`/`expect`, acceptable for tests but worth keeping isolated from production paths.

### Recommended Next Step
- Iteration 9: harden production paths found in adversarial review, starting with panic removal in `core/src/transfer.rs` and cleanup of long-lived UI timers/notifications.

## Iteration 9

### Scope
- Address the top production risks identified during adversarial review.

### Changes
- Replaced the production `dir_entries.unwrap()` in `core/src/transfer.rs` with an explicit error path.
- Updated `tauri-app/src/composables/useUpdate.ts` to use non-blocking toast notifications instead of `alert()`.
- Added explicit ownership for the whiteboard flush timer in `App.vue` and clear it during unmount.

### Validation
- `cargo test -p rust-air-core --lib -- --nocapture`
- `cargo check -p tauri-app`
- `pnpm exec vue-tsc --noEmit`
- `pnpm build`

### Stability Impact
- Removed one latent panic point from the transfer sender path.
- Improved update UX and testability by eliminating blocking browser alerts.
- Prevented whiteboard background flush timer leaks across component unmounts.

### Remaining Issues
- Some long-lived `setTimeout` usage still remains in composables and may be worth centralizing later if lifecycle complexity grows.
- README / implementation drift still needs correction.
- Todo and whiteboard state can still be decomposed further if UI growth continues.

### Recommended Next Step
- Prepare a cleanup/release pass: reconcile README, review remaining production `unwrap` sites, and then package these iterations for upstream push/release.
