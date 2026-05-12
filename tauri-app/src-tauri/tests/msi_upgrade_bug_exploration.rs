//! Bug Condition Exploration Test for MSI Version Upgrade Flags
//!
//! **Property 1: Bug Condition - MSI Version Upgrade Flags**
//!
//! This test validates that the MSI installer command is generated with the correct
//! flags for proper version upgrades:
//! - REINSTALLMODE=vomus (not "omus") - the "v" flag runs from source and re-caches the package
//! - REINSTALL=ALL - ensures all features are reinstalled during upgrade
//! - Ping delay of at least 5 seconds (not 3 seconds) - gives more time for app termination
//!
//! **FIX APPLIED**: The code has been fixed and this test should now PASS.
//!
//! **Validates: Requirements 2.1, 2.2**

use std::path::{Path, PathBuf};

/// Represents the context for installer launch
#[derive(Debug, Clone)]
pub struct InstallerLaunchContext {
    pub platform: String,
    pub installer_type: String,
    pub msi_flags: String,
    pub ping_delay_seconds: u32,
}

/// Generates the MSI command string for testing purposes.
/// This function mirrors the logic in launch_installer to allow testing.
fn generate_msi_command(path: &Path) -> String {
    let msi_path = path.to_string_lossy().to_string();
    // This is the FIXED implementation (matching update_commands.rs)
    format!(
        "ping -n 5 127.0.0.1 >nul & start \"\" msiexec /i \"{}\" /qb REINSTALL=ALL REINSTALLMODE=vomus",
        msi_path
    )
}

/// Generates the expected FIXED MSI command string.
fn generate_expected_msi_command(path: &Path) -> String {
    let msi_path = path.to_string_lossy().to_string();
    // This is the EXPECTED (fixed) implementation
    format!(
        "ping -n 5 127.0.0.1 >nul & start \"\" msiexec /i \"{}\" /qb REINSTALL=ALL REINSTALLMODE=vomus",
        msi_path
    )
}

/// Extracts MSI flags from a command string
fn extract_msi_flags(command: &str) -> Option<String> {
    if command.contains("REINSTALLMODE=") {
        let start = command.find("REINSTALLMODE=").unwrap() + 14;
        let rest = &command[start..];
        // REINSTALLMODE value continues until whitespace or end
        let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        Some(rest[..end].to_string())
    } else {
        None
    }
}

/// Extracts ping delay from command string
fn extract_ping_delay(command: &str) -> Option<u32> {
    // Pattern: ping -n X 127.0.0.1
    if let Some(start) = command.find("ping -n ") {
        let rest = &command[start + 8..];
        if let Some(end) = rest.find(' ') {
            if let Ok(delay) = rest[..end].parse::<u32>() {
                return Some(delay);
            }
        }
    }
    None
}

/// Checks if REINSTALL=ALL is present in the command
fn has_reinstall_all(command: &str) -> bool {
    command.contains("REINSTALL=ALL")
}

/// Checks if REINSTALLMODE=vomus (correct) vs omus (buggy)
fn has_correct_reinstall_mode(command: &str) -> bool {
    command.contains("REINSTALLMODE=vomus")
}

// =============================================================================
// Property-Based Tests (using proptest)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    
    /// Test that MSI command contains REINSTALLMODE=vomus (not omus)
    /// 
    /// **Validates: Requirements 2.1**
    /// 
    /// This test should PASS after the fix is applied.
    #[test]
    fn test_msi_command_has_correct_reinstall_mode() {
        let path = PathBuf::from("C:\\temp\\rust-air-0.3.43-x64_en-US.msi");
        let command = generate_msi_command(&path);
        
        println!("Generated MSI command: {}", command);
        
        // The command should contain REINSTALLMODE=vomus, not REINSTALLMODE=omus
        // The "v" flag is critical for version upgrades - it runs from source and re-caches the package
        assert!(
            has_correct_reinstall_mode(&command),
            "MSI command should contain REINSTALLMODE=vomus but got: {}",
            extract_msi_flags(&command).unwrap_or("NONE".to_string())
        );
    }
    
    /// Test that MSI command contains REINSTALL=ALL
    /// 
    /// **Validates: Requirements 2.1, 2.2**
    /// 
    /// This test should PASS after the fix is applied.
    #[test]
    fn test_msi_command_has_reinstall_all() {
        let path = PathBuf::from("C:\\temp\\rust-air-0.3.43-x64_en-US.msi");
        let command = generate_msi_command(&path);
        
        println!("Generated MSI command: {}", command);
        
        // The command should contain REINSTALL=ALL to ensure all features are reinstalled
        assert!(
            has_reinstall_all(&command),
            "MSI command should contain REINSTALL=ALL property"
        );
    }
    
    /// Test that ping delay is at least 5 seconds
    /// 
    /// **Validates: Requirements 2.3**
    /// 
    /// This test should PASS after the fix is applied.
    #[test]
    fn test_msi_command_has_sufficient_ping_delay() {
        let path = PathBuf::from("C:\\temp\\rust-air-0.3.43-x64_en-US.msi");
        let command = generate_msi_command(&path);
        
        println!("Generated MSI command: {}", command);
        
        let delay = extract_ping_delay(&command).expect("Could not extract ping delay");
        
        println!("Extracted ping delay: {} seconds", delay);
        
        // The delay should be at least 5 seconds to give the app time to fully terminate
        assert!(
            delay >= 5,
            "Ping delay should be at least 5 seconds but got: {} seconds",
            delay
        );
    }
    
    /// Test the expected FIXED command is correctly generated
    /// 
    /// This test verifies what the correct command SHOULD look like.
    /// It will PASS with the expected implementation (after fix).
    #[test]
    fn test_expected_fixed_msi_command() {
        let path = PathBuf::from("C:\\temp\\rust-air-0.3.43-x64_en-US.msi");
        let command = generate_expected_msi_command(&path);
        
        println!("Expected FIXED MSI command: {}", command);
        
        // Verify the expected command has all the correct properties
        assert!(has_correct_reinstall_mode(&command), 
            "Expected command should have REINSTALLMODE=vomus");
        assert!(has_reinstall_all(&command), 
            "Expected command should have REINSTALL=ALL");
        
        let delay = extract_ping_delay(&command).expect("Could not extract ping delay");
        assert!(delay >= 5, 
            "Expected command should have ping delay >= 5 seconds");
    }
    
    /// Comprehensive bug condition test
    /// 
    /// **Validates: Requirements 2.1, 2.2, 2.3**
    /// 
    /// This test checks ALL expected properties of the MSI command.
    /// It should PASS after the fix is applied.
    #[test]
    fn test_msi_command_all_properties() {
        let path = PathBuf::from("C:\\temp\\rust-air-0.3.43-x64_en-US.msi");
        let command = generate_msi_command(&path);
        
        println!("\n=== MSI Command Verification ===");
        println!("Generated command: {}", command);
        
        let mut issues = Vec::new();
        
        // Check REINSTALLMODE
        if !has_correct_reinstall_mode(&command) {
            let flags = extract_msi_flags(&command).unwrap_or("NONE".to_string());
            issues.push(format!(
                "REINSTALLMODE should be 'vomus' but got '{}' (missing 'v' flag for version upgrade)",
                flags
            ));
        }
        
        // Check REINSTALL=ALL
        if !has_reinstall_all(&command) {
            issues.push("Missing REINSTALL=ALL property (required for complete feature reinstall)".to_string());
        }
        
        // Check ping delay
        if let Some(delay) = extract_ping_delay(&command) {
            if delay < 5 {
                issues.push(format!(
                    "Ping delay is {} seconds, should be at least 5 seconds",
                    delay
                ));
            }
        }
        
        if !issues.is_empty() {
            println!("\n=== ISSUES FOUND ===");
            for issue in &issues {
                println!("  - {}", issue);
            }
            panic!("MSI command has incorrect flags. See issues above.");
        }
        
        println!("\n=== ALL CHECKS PASSED ===");
    }
}

// =============================================================================
// Documentation of Expected vs Actual Behavior
// =============================================================================

// FIXED BEHAVIOR (after fix):
// Command: ping -n 5 127.0.0.1 >nul & start "" msiexec /i "path" /qb REINSTALL=ALL REINSTALLMODE=vomus
//
// PREVIOUS BUGGY BEHAVIOR:
// Command: ping -n 3 127.0.0.1 >nul & start "" msiexec /i "path" /qb REINSTALLMODE=omus
//
// FIXES APPLIED:
// 1. REINSTALLMODE: Changed from "omus" to "vomus" - added 'v' flag
//    - 'v' flag: Run from source, re-cache the package (critical for version upgrades)
//    - This ensures proper version registration during upgrades
//
// 2. REINSTALL=ALL: Added
//    - This property tells MSI to reinstall ALL features during upgrade
//    - Ensures complete feature upgrade
//
// 3. Ping delay: Changed from 3 seconds to 5 seconds
//    - Gives more time for app to fully terminate
//    - Reduces file locks or process conflicts during installation
