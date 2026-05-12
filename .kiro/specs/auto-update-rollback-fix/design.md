# Auto-Update Rollback Fix Design

## Overview

This design addresses a bug in the auto-update functionality where Windows MSI installations cause version rollback instead of proper upgrade. The bug occurs due to two main issues: (1) the `REINSTALLMODE=omus` MSI flag can cause version downgrade, and (2) the application exits immediately after launching the MSI installer, potentially interfering with the installation process.

The fix involves correcting the MSI installation flags to ensure proper version upgrade behavior and adjusting the application exit timing to avoid interfering with the MSI installation process.

## Glossary

- **Bug_Condition (C)**: The condition that triggers the version rollback - when MSI installer is launched with `REINSTALLMODE=omus` flag and the application exits immediately after launching
- **Property (P)**: The desired behavior - MSI installer should use correct upgrade flags that prevent version downgrade, and the application should not interfere with the installation process
- **Preservation**: Existing update download mechanism, proxy usage, settings persistence, and cross-platform support that must remain unchanged
- **launch_installer**: The function in `tauri-app/src-tauri/src/update_commands.rs` that spawns the installer process
- **download_and_install**: The function that downloads the installer and triggers the installation
- **REINSTALLMODE**: MSI installer property that controls how files are reinstalled. The `omus` value means: o=overwrite older files, m=rewrite all machine registry entries, u=rewrite all user registry entries, s=ignore default file version checks

## Bug Details

### Bug Condition

The bug manifests when the auto-update system launches the MSI installer on Windows. The `launch_installer` function uses `REINSTALLMODE=omus` which may cause version downgrade, and immediately calls `app.exit(0)` which may interfere with the MSI installation process before it completes version registration.

**Formal Specification:**
```
FUNCTION isBugCondition(input)
  INPUT: input of type InstallerLaunchContext
  OUTPUT: boolean
  
  RETURN input.platform == "windows"
         AND input.installerType == "msi"
         AND input.msiFlags CONTAINS "REINSTALLMODE=omus"
         AND input.appExitCalled IMMEDIATELY_AFTER spawn
END FUNCTION
```

### Examples

- **Example 1**: User runs v0.1.0 and auto-update downloads v0.2.0 MSI. The installer launches with `REINSTALLMODE=omus`. After installation, app still shows v0.1.0 instead of v0.2.0.
- **Example 2**: User updates from v0.2.0 to v0.3.0. The MSI installation appears to succeed, but the version remains at v0.2.0 due to improper version handling.
- **Example 3**: User attempts multiple updates in succession. Each update appears to complete but version never advances beyond the first installed version.
- **Edge Case**: User has v0.1.0 installed. MSI with same version v0.1.0 is launched - should not reinstall but `REINSTALLMODE=omus` forces reinstallation.

## Expected Behavior

### Preservation Requirements

**Unchanged Behaviors:**
- The xgn.io proxy with fallback to direct GitHub URL must continue to work for download acceleration
- The `auto_check` and `auto_install` settings from user configuration must continue to be respected
- macOS and Linux installation mechanisms (DMG for macOS, deb/AppImage for Linux) must remain unchanged
- Cleanup of old installer files on startup must continue to work
- Download progress events must continue to be emitted to the frontend
- Installer file verification (size check, minimum size validation) must remain in place

**Scope:**
All inputs that do NOT involve Windows MSI installation should be completely unaffected by this fix. This includes:
- Windows NSIS installer (.exe) handling
- macOS DMG installation
- Linux deb/AppImage installation
- Download proxy mechanism
- Update check and version comparison logic

## Hypothesized Root Cause

Based on the bug description and code analysis, the most likely issues are:

1. **Incorrect MSI Reinstall Mode**: The `REINSTALLMODE=omus` flag may not properly handle version upgrades
   - The `o` flag (overwrite older files) seems correct for upgrades
   - However, `omus` may not properly register the new version in the Windows Installer database
   - MSI best practices recommend `REINSTALLMODE=vomus` for major upgrades (v=run from source, re-cache package)

2. **Missing ProductCode Handling**: MSI upgrades typically require either:
   - Major upgrade with new ProductCode and `UPGRADINGPRODUCTGUID` property
   - Minor upgrade with proper `REINSTALL` and `REINSTALLMODE` properties

3. **Application Exit Timing**: The `app.exit(0)` is called immediately after `spawn()` returns
   - MSI installation may still be initializing when the app exits
   - The `ping -n 3` delay (3 seconds) may be insufficient for MSI to complete version registration
   - Rapid exit may prevent MSI from properly writing to the registry

4. **Detached Process Creation**: While `DETACHED_PROCESS` flag is used, the MSI process may still have dependencies on the parent process environment that get cleaned up too early

## Correctness Properties

Property 1: Bug Condition - MSI Version Upgrade

_For any_ Windows MSI installation triggered by auto-update where a newer version installer is launched, the fixed `launch_installer` function SHALL use MSI flags that ensure the new version is properly registered and the installed application reflects the upgraded version number.

**Validates: Requirements 2.1, 2.2**

Property 2: Preservation - Non-Windows Platform Behavior

_For any_ installation on macOS or Linux platforms, the fixed code SHALL produce exactly the same behavior as the original code, preserving all existing platform-specific installation mechanisms.

**Validates: Requirements 3.3**

Property 3: Preservation - Download Mechanism

_For any_ installer download operation, the fixed code SHALL continue to use the xgn.io proxy with fallback to direct GitHub URL, preserving the download acceleration mechanism.

**Validates: Requirements 3.1**

Property 4: Preservation - Settings and Configuration

_For any_ update check operation, the fixed code SHALL continue to respect `auto_check` and `auto_install` settings, preserving the existing configuration behavior.

**Validates: Requirements 3.2**

## Fix Implementation

### Changes Required

**File**: `tauri-app/src-tauri/src/update_commands.rs`

**Function**: `launch_installer`

**Specific Changes**:

1. **Fix MSI Reinstall Mode Flag**: Change `REINSTALLMODE=omus` to `REINSTALLMODE=vomus`
   - The `v` flag runs from source and re-caches the package, which is proper for version upgrades
   - This ensures the new version's MSI package is properly registered in the Windows Installer cache
   - Implementation: Modify the format string in the MSI command

2. **Add REINSTALL Property**: Add `REINSTALL=ALL` property to ensure all features are reinstalled
   - This tells MSI to reinstall all features during the upgrade
   - Combined with `REINSTALLMODE=vomus`, ensures complete upgrade installation

3. **Increase Pre-Install Delay**: Increase the ping delay from 3 to 5 seconds
   - Gives more time for the app to fully terminate before MSI starts
   - Reduces chance of file locks or process conflicts
   - Implementation: Change `ping -n 3` to `ping -n 5`

4. **Remove Immediate Exit**: Modify `download_and_install` to not call `app.exit(0)` immediately
   - Instead, let the installer spawn complete naturally
   - Use a longer delay or rely on the user to close the app manually after the installer UI appears
   - Alternative: Add a delay before exit using `std::thread::sleep`

5. **Add Version Verification Logging**: Add logging to help diagnose future issues
   - Log the version before and after installation
   - Log the MSI command being executed

### Implementation Code Changes

```rust
// In launch_installer function, change MSI command from:
// msiexec /i "path" /qb REINSTALLMODE=omus

// To:
// msiexec /i "path" /qb REINSTALL=ALL REINSTALLMODE=vomus

// And change ping delay from:
// ping -n 3 127.0.0.1

// To:
// ping -n 5 127.0.0.1
```

## Testing Strategy

### Validation Approach

The testing strategy follows a two-phase approach: first, surface counterexamples that demonstrate the bug on unfixed code, then verify the fix works correctly and preserves existing behavior.

### Exploratory Bug Condition Checking

**Goal**: Surface counterexamples that demonstrate the version rollback bug BEFORE implementing the fix. Confirm or refute the root cause analysis.

**Test Plan**: Create integration tests that simulate the MSI installation process and verify version registration. Since we cannot easily mock MSI installation in a unit test, we will create manual test procedures and verification scripts.

**Test Cases**:
1. **Version Comparison Test**: Test `is_newer()` function with various version strings to ensure version comparison is correct (this is NOT the bug, but good to verify)
2. **MSI Command String Test**: Verify the MSI command string generation produces correct flags
3. **Manual Upgrade Test**: Perform actual upgrade from v0.1.0 to v0.2.0 on Windows and verify version after installation (manual test)
4. **Clean Install Test**: Verify MSI installation on clean system works correctly (baseline test)

**Expected Counterexamples**:
- After MSI installation, application version remains at old version
- Registry shows old version number in `HKLM\Software\Microsoft\Windows\CurrentVersion\Uninstall`
- MSI log file (if enabled) shows version downgrade or file overwrite without version update

### Fix Checking

**Goal**: Verify that for all inputs where the bug condition holds, the fixed function produces the expected behavior.

**Pseudocode:**
```
FOR ALL windowsInstall WHERE isBugCondition(windowsInstall) DO
  result := launch_installer_fixed(windowsInstall)
  ASSERT msiFlagsContain(result, "REINSTALLMODE=vomus")
  ASSERT msiFlagsContain(result, "REINSTALL=ALL")
  ASSERT installedVersionEquals(expectedNewVersion)
END FOR
```

### Preservation Checking

**Goal**: Verify that for all inputs where the bug condition does NOT hold, the fixed function produces the same result as the original function.

**Pseudocode:**
```
FOR ALL input WHERE NOT isBugCondition(input) DO
  ASSERT launch_installer_original(input) = launch_installer_fixed(input)
END FOR
```

**Testing Approach**: Unit tests for preservation checking verify that:
- macOS installation command remains unchanged
- Linux installation commands remain unchanged
- Windows NSIS (.exe) installation command remains unchanged
- Download proxy mechanism remains unchanged
- Settings loading/saving remains unchanged

**Test Cases**:
1. **macOS DMG Preservation**: Verify `launch_installer` on macOS still uses `open` command
2. **Linux deb Preservation**: Verify `launch_installer` on Linux still uses `pkexec dpkg -i`
3. **Linux AppImage Preservation**: Verify AppImage still gets executable permissions and runs directly
4. **Download Proxy Preservation**: Verify download URL is still prefixed with `https://xgn.io/`
5. **Settings Preservation**: Verify `UpdateSettings` load/save behavior unchanged

### Unit Tests

- Test MSI command string generation with correct flags
- Test EXE installer command generation (should remain unchanged)
- Test version comparison function
- Test installer asset picking logic for each platform

### Property-Based Tests

- Generate random version strings and verify `is_newer` comparison is transitive
- Generate random download URLs and verify proxy prefix is applied correctly
- Generate random installer paths and verify correct command generation per platform

### Integration Tests

- Test full download and install flow on Windows (requires actual MSI)
- Test cleanup of old installer files on startup
- Test settings persistence across application restarts
- Manual test: Perform actual upgrade installation on Windows VM and verify version

## Manual Test Procedure

Since MSI installation cannot be easily automated in unit tests, follow this manual test procedure:

1. **Setup**: Install old version (e.g., v0.1.0) on Windows VM
2. **Trigger Update**: Launch app with auto-update enabled
3. **Observe Installation**: Watch the MSI installer execute
4. **Verify Version**: After installation completes, check:
   - App version in UI shows new version
   - Registry shows new version
   - App executable properties show new version
5. **Rollback Test**: Try updating to an older version (should be prevented by version check)
