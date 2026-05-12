## Updater Checklist

Use this checklist whenever changing auto-update behavior.

### Release Discovery

- Does GitHub API parsing still work?
- Does HTML fallback still find the correct tag and asset?
- Can ambiguous assets be selected accidentally?

### Download Safety

- Is proxy behavior explicit?
- Does fallback to direct GitHub work?
- Are downloaded installers validated before launch?
- For Windows, does MSI/EXE content match the expected installer signature?

### Launch Behavior

- Does Windows MSI launch with the intended reinstall flags?
- Is EXE behavior explicit about silent vs interactive install?
- Is there any path where the app exits before installer handoff is reasonably safe?

### Validation

- `cargo test`
- `cargo check`
- include updater-specific tests when helpers or launch logic change

### Release Compatibility

- Do release asset names still match updater expectations?
- Is the preferred Windows asset still the intended installer type?
