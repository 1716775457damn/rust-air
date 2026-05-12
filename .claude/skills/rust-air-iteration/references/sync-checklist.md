## Sync Checklist

Use this checklist whenever changing sync behavior.

### Semantics

- Is this local mirror sync or two-device sync?
- Does the UI text still match the actual behavior?
- Are buttons/actions still obvious to users?

### Correctness

- Are manifest entries based on actual file metadata?
- Are hash/size/mtime values consistent after push and pull?
- Are tombstones/delete actions revalidated before deletion?
- Can this change introduce state drift between peers?

### Performance

- Does the change increase full-directory hashing?
- Can cached hash reuse still work?
- Does this introduce extra file reads before or after transfer?

### Observability

- Are phase, stats, and action progress still emitted correctly?
- Does the sync console remain understandable?

### Validation

- `cargo test`
- `cargo check`
- `pnpm build`

If release-facing behavior changed, also verify packaged app behavior.
