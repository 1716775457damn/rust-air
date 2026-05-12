## Release Checklist

Use this checklist whenever preparing or publishing a release.

### Versioning

Verify version alignment in:

- `cli/Cargo.toml`
- `core/Cargo.toml`
- `tauri-app/src-tauri/Cargo.toml`
- `tauri-app/src-tauri/tauri.conf.json`
- `Cargo.lock`

### Workflow

- Does `.github/workflows/release.yml` describe the real change set?
- Do asset names in release notes match actual generated artifacts?
- Are updater expectations aligned with released Windows assets?

### Local Verification

Run:

```text
cargo test
cargo check
pnpm build
```

For Windows/UI/release work, also run:

```text
pnpm tauri build
```

### GitHub Follow-through

- commit only after verification
- create annotated tag
- push branch and tag
- watch `Release App` workflow
- confirm release page and asset uploads

### Final Handoff

Always report:

- release version
- release URL
- key Windows asset URLs
- workflow result
