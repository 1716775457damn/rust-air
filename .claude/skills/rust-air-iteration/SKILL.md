---
name: rust-air-iteration
description: Iteration standard for the rust-air project. Use this skill proactively whenever the user asks to modify, fix, optimize, review, release, or investigate anything in rust-air, especially sync, updater, Windows installer, Tauri GUI, CLI, release workflow, performance, or UX. Also use it whenever the conversation touches sync semantics, auto-update failures, release assets, GitHub releases, MSI/EXE behavior, or cross-platform packaging, even if the user does not explicitly ask for a "workflow" or "standard".
---

# rust-air Iteration Standard

This skill defines the default execution standard for the `rust-air` repository.

## Core Rule

Do not treat `rust-air` as a generic coding task. Every change must be evaluated through the project's risk areas:

- sync correctness
- updater/download/install safety
- Windows installer behavior
- release/version consistency
- UX clarity between local mirror and two-device sync
- verification before handoff

## Required Workflow

Follow this sequence unless the user explicitly asks for a narrower action.

1. Read the current implementation before deciding on changes.
2. Identify the change type:
   - sync
   - updater
   - UI/UX
   - release
   - performance
   - docs
3. Apply the smallest correct fix first.
4. Verify impacted paths, not just the edited file.
5. Run validation commands before finishing.
6. If the user asked for release/publish, verify versioning and asset flow end-to-end.

## Risk Domains

### Sync

Always check:

- local mirror vs two-device sync semantics
- manifest/hash/mtime consistency
- delete/tombstone safety
- push/pull state drift
- progress and observability
- persisted sync state correctness

If sync logic changes, read `references/sync-checklist.md`.

### Updater

Always check:

- GitHub release lookup behavior
- asset selection logic
- proxy/fallback behavior
- installer file validation
- Windows MSI/EXE launch semantics
- post-download safety before app exit

If updater logic changes, read `references/updater-checklist.md`.

### Release

Always check:

- version consistency across Rust crates and Tauri config
- release workflow body accuracy
- build output names vs updater expectations
- GitHub release asset naming stability
- local verification before tag/push

If release flow changes, read `references/release-checklist.md`.

## Validation Requirements

Default validation set for meaningful repository changes:

```text
cargo test
cargo check
pnpm build
```

Add more when relevant:

- `pnpm tauri build` for installer/release/UI packaging work
- `gh run list --workflow "Release App" --limit 3` for release follow-through
- local asset existence checks for MSI/NSIS outputs when release work is involved

Do not skip validation silently. If something could not be run, say exactly why.

## Output Standard

When finishing work, structure the answer around:

1. What changed
2. Why it changed
3. Files touched
4. Validation run
5. Remaining risks or next logical step

When reviewing, lead with findings first.

## Project Defaults

- Prefer MSI as the primary Windows GUI installer artifact.
- Keep sync wording explicit: local mirror is not two-device continuous sync.
- Prefer minimal protocol changes unless both sender and receiver paths are updated together.
- Treat updater failures on Windows as high severity.
- Treat release asset naming drift as high risk.

## Escalation Rules

Pause and call out the issue if you see:

- a UI change that hides or renames a primary user action ambiguously
- updater behavior that may launch a non-installer payload
- sync deletion behavior that can remove newer data
- a release flow that changes versioning or asset naming without end-to-end verification

## Reference Files

- `references/sync-checklist.md`
- `references/updater-checklist.md`
- `references/release-checklist.md`
