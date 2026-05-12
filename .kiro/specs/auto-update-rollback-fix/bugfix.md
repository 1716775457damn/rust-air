# Bugfix Requirements Document

## Introduction

This document describes a bug in the auto-update functionality of the rust-air Tauri application where the software version reverts to the original version after an update. This issue prevents users from successfully upgrading to newer versions through the automatic update mechanism.

## Bug Analysis

### Current Behavior (Defect)

1.1 WHEN the MSI installer is launched by the auto-update function THEN the system uses `REINSTALLMODE=omus` flag which may cause version downgrade instead of upgrade

1.2 WHEN the MSI installer executes with `REINSTALLMODE=omus` THEN the system overwrites existing files but may not properly handle version comparison, leading to version rollback

1.3 WHEN the auto-update process completes on Windows THEN the system exits the application immediately via `app.exit(0)` without ensuring the MSI installation has fully completed its version registration

### Expected Behavior (Correct)

2.1 WHEN the MSI installer is launched by the auto-update function THEN the system SHALL use appropriate MSI flags that prevent version downgrade and ensure proper upgrade installation

2.2 WHEN the MSI installer executes THEN the system SHALL properly register the new version number and ensure the upgraded application reflects the new version

2.3 WHEN the auto-update process completes on Windows THEN the system SHALL ensure the MSI installation has finished before the application exits, or use a mechanism that does not interfere with the installation process

### Unchanged Behavior (Regression Prevention)

3.1 WHEN the auto-update downloads an installer file THEN the system SHALL CONTINUE TO use the xgn.io proxy with fallback to direct GitHub URL for download acceleration

3.2 WHEN the auto-update checks for updates on startup THEN the system SHALL CONTINUE TO respect the `auto_check` and `auto_install` settings from the user configuration

3.3 WHEN the auto-update runs on macOS or Linux THEN the system SHALL CONTINUE TO use the existing platform-specific installation mechanisms (DMG for macOS, deb/AppImage for Linux)

3.4 WHEN cleaning up old installer files on startup THEN the system SHALL CONTINUE TO remove installer files that do not match the current version from the temp directory
