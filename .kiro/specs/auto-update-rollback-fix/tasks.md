# Implementation Plan

## Overview

This plan addresses the auto-update rollback bug where Windows MSI installations cause version rollback instead of proper upgrade. The fix involves correcting MSI installation flags and adjusting application exit timing.

## Bug Condition Summary

- **C(X)**: `platform == "windows" AND installerType == "msi" AND msiFlags CONTAINS "REINSTALLMODE=omus" AND appExitCalled IMMEDIATELY_AFTER spawn`
- **Expected Behavior**: MSI installer uses `REINSTALLMODE=vomus` with `REINSTALL=ALL`, ensuring proper version upgrade

## Tasks

- [x] 1. Write bug condition exploration test
  - **Property 1: Bug Condition** - MSI Version Upgrade Flags
  - **CRITICAL**: This test MUST FAIL on unfixed code - failure confirms the bug exists
  - **DO NOT attempt to fix the test or the code when it fails**
  - **NOTE**: This test encodes the expected behavior - it will validate the fix when it passes after implementation
  - **GOAL**: Surface counterexamples that demonstrate the bug exists
  - **Scoped PBT Approach**: Test that `launch_installer` generates MSI command with correct flags for version upgrade
  - Test that the MSI command string contains `REINSTALLMODE=vomus` (not `omus`)
  - Test that the MSI command string contains `REINSTALL=ALL`
  - Test that ping delay is at least 5 seconds (not 3 seconds)
  - Run test on UNFIXED code
  - **EXPECTED OUTCOME**: Test FAILS (confirms bug exists - wrong flags are used)
  - Document the actual command string generated to understand root cause
  - Mark task complete when test is written, run, and failure is documented
  - _Requirements: 2.1, 2.2_

- [x] 2. Write preservation property tests (BEFORE implementing fix)
  - **Property 2: Preservation** - Non-Windows Platform Behavior
  - **IMPORTANT**: Follow observation-first methodology
  - Observe behavior on UNFIXED code for non-buggy inputs (non-Windows platforms, EXE installers)
  - Write property-based tests capturing observed behavior patterns:
    - macOS DMG: Uses `open` command unchanged
    - Linux deb: Uses `pkexec dpkg -i` unchanged
    - Linux AppImage: Sets executable permissions and runs directly
    - Windows EXE: Uses `start ""` command unchanged
    - Download proxy: Uses xgn.io prefix with GitHub fallback
    - Settings: `UpdateSettings` load/save behavior unchanged
  - Run tests on UNFIXED code
  - **EXPECTED OUTCOME**: Tests PASS (confirms baseline behavior to preserve)
  - Mark task complete when tests are written, run, and passing on unfixed code
  - _Requirements: 3.1, 3.2, 3.3_

- [x] 3. Fix for MSI version rollback

  - [x] 3.1 Update MSI reinstall mode flag
    - Change `REINSTALLMODE=omus` to `REINSTALLMODE=vomus` in `launch_installer` function
    - The `v` flag runs from source and re-caches the package for proper version upgrades
    - File: `tauri-app/src-tauri/src/update_commands.rs`
    - _Bug_Condition: isBugCondition(input) where msiFlags CONTAINS "REINSTALLMODE=omus"_
    - _Expected_Behavior: MSI command contains "REINSTALLMODE=vomus"_
    - _Requirements: 2.1_

  - [x] 3.2 Add REINSTALL property to MSI command
    - Add `REINSTALL=ALL` property to ensure all features are reinstalled during upgrade
    - Combined with `REINSTALLMODE=vomus`, ensures complete upgrade installation
    - File: `tauri-app/src-tauri/src/update_commands.rs`
    - _Bug_Condition: isBugCondition(input) where MSI lacks REINSTALL property_
    - _Expected_Behavior: MSI command contains "REINSTALL=ALL"_
    - _Requirements: 2.1, 2.2_

  - [x] 3.3 Increase pre-install delay from 3 to 5 seconds
    - Change `ping -n 3` to `ping -n 5` in the MSI command
    - Gives more time for the app to fully terminate before MSI starts
    - Reduces chance of file locks or process conflicts
    - File: `tauri-app/src-tauri/src/update_commands.rs`
    - _Bug_Condition: isBugCondition(input) where appExitCalled IMMEDIATELY_AFTER spawn_
    - _Expected_Behavior: Delay increased to 5 seconds for safer installation_
    - _Requirements: 2.3_

  - [x] 3.4 Add version verification logging
    - Add logging of MSI command being executed
    - Add logging of current version before installation
    - Helps diagnose future issues with version upgrades
    - File: `tauri-app/src-tauri/src/update_commands.rs`
    - _Requirements: 2.2_

  - [x] 3.5 Verify bug condition exploration test now passes
    - **Property 1: Expected Behavior** - MSI Version Upgrade Flags
    - **IMPORTANT**: Re-run the SAME test from task 1 - do NOT write a new test
    - The test from task 1 encodes the expected behavior
    - When this test passes, it confirms the expected behavior is satisfied
    - Run bug condition exploration test from step 1
    - **EXPECTED OUTCOME**: Test PASSES (confirms bug is fixed)
    - _Requirements: 2.1, 2.2_

  - [x] 3.6 Verify preservation tests still pass
    - **Property 2: Preservation** - Non-Windows Platform Behavior
    - **IMPORTANT**: Re-run the SAME tests from task 2 - do NOT write new tests
    - Run preservation property tests from step 2
    - **EXPECTED OUTCOME**: Tests PASS (confirms no regressions)
    - Confirm all tests still pass after fix (no regressions)

- [x] 4. Checkpoint - Ensure all tests pass
  - Ensure all unit tests pass: `cargo test --manifest-path tauri-app/src-tauri/Cargo.toml`
  - Verify build succeeds: `cargo build --manifest-path tauri-app/src-tauri/Cargo.toml`
  - Review any warnings and address if necessary
  - Ask user if questions arise during verification
  - _Requirements: All_
