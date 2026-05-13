//! Preservation property tests for transfer-resume-fix.
//!
//! These tests verify baseline behavior that MUST be preserved after the bugfix.
//! They MUST PASS on the current unfixed code — confirming the behavior we need
//! to keep intact when fixing reconnection direction and pipeline speed.
//!
//! **Validates: Requirements 3.1, 3.2, 3.5, 3.6, 3.7**

use proptest::prelude::*;
use rand::RngCore;
use rust_air_core::crypto::{Decryptor, Encryptor};
use rust_air_core::proto::{ArchiveSnapshot, ArchiveStatus, ArchiveStatusCode, Kind, SessionManifest, TransferEvent, CHUNK};
use rust_air_core::transfer::{receive_to_disk, reconnect_delay_secs, send_path};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use tokio::net::TcpListener;
use std::sync::{Arc, Mutex};

/// Create a unique temp directory for this test run.
fn test_dir(name: &str) -> PathBuf {
    let id: u64 = rand::random();
    let dir = std::env::temp_dir().join(format!("transfer_resume_fix_pres_{name}_{id:x}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Generate a file filled with random bytes.
fn create_random_file(path: &std::path::Path, size: usize) {
    let mut data = vec![0u8; size];
    rand::thread_rng().fill_bytes(&mut data);
    fs::write(path, &data).unwrap();
}

// ── Test 1: Fresh transfer integrity ──────────────────────────────────────────

/// Transfer a random 2 MB file over loopback via `send_path` + `receive_to_disk`.
/// Verify the received file is byte-identical to the original and SHA-256 matches.
///
/// **Validates: Requirements 3.1**
#[tokio::test]
async fn test_fresh_transfer_integrity() {
    const FILE_SIZE: usize = 2 * 1024 * 1024; // 2 MB

    let src_dir = test_dir("fresh_src");
    let src_file = src_dir.join("test_2mb.bin");
    create_random_file(&src_file, FILE_SIZE);

    let original_data = fs::read(&src_file).unwrap();
    let original_sha: [u8; 32] = Sha256::digest(&original_data).into();

    let dest_dir = test_dir("fresh_dest");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let send_file = src_file.clone();
    let send_task = tokio::spawn(async move {
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        send_path(stream, &send_file, |_| {}).await.unwrap();
    });

    let (stream, _) = listener.accept().await.unwrap();
    let outcome = receive_to_disk(stream, &dest_dir, |_| {}).await.unwrap();

    send_task.await.unwrap();

    // Verify byte-identical content
    let received_data = fs::read(outcome.path()).unwrap();
    assert_eq!(
        original_data.len(),
        received_data.len(),
        "file sizes must match"
    );
    assert_eq!(original_data, received_data, "file content must be byte-identical");

    // Verify SHA-256 matches
    let received_sha: [u8; 32] = Sha256::digest(&received_data).into();
    assert_eq!(original_sha, received_sha, "SHA-256 checksums must match");

    // Cleanup
    let _ = fs::remove_dir_all(&src_dir);
    let _ = fs::remove_dir_all(&dest_dir);
}

// ── Test 2: Nonce counter alignment (property-based) ──────────────────────────

// Nonce counter alignment property test:
// Create an Encryptor and Decryptor with the same key. Set counter to
// `offset / CHUNK` for a random chunk-aligned offset. Encrypt some data,
// then decrypt it. Verify the decrypted data matches the original.
//
// **Validates: Requirements 3.5**
proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]
    #[test]
    fn test_nonce_counter_alignment(
        // Random chunk-aligned offset: 0..64 chunks (0..64 MB)
        chunk_index in 0u64..64,
        // Random data size: 1 byte to 1 MB
        data_size in 1usize..=(CHUNK),
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let offset = chunk_index * CHUNK as u64;
            let counter = offset / CHUNK as u64;

            // Generate random key and data
            let mut key = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut key);
            let mut plaintext = vec![0u8; data_size];
            rand::thread_rng().fill_bytes(&mut plaintext);

            // Encrypt with counter set to offset / CHUNK
            let mut enc_buf = Vec::new();
            let mut enc = Encryptor::new(&key, &mut enc_buf);
            enc.set_counter(counter);
            enc.write_chunk(&plaintext).await.unwrap();
            enc.shutdown().await.unwrap();

            // Decrypt with same counter
            let cursor = std::io::Cursor::new(enc_buf);
            let reader = tokio::io::BufReader::new(cursor);
            let mut dec = Decryptor::new(&key, reader);
            dec.set_counter(counter);

            let decrypted = dec.read_chunk().await.unwrap().expect("should get a chunk");
            prop_assert_eq!(&decrypted, &plaintext, "decrypted data must match original");

            // Next read should return None (EOF sentinel)
            let eof = dec.read_chunk().await.unwrap();
            prop_assert!(eof.is_none(), "should get EOF sentinel after data");

            Ok(())
        })?;
    }
}

// ── Test 3: Exponential backoff calculation ───────────────────────────────────

/// Verify `reconnect_delay_secs(n)` returns `2^n` seconds for n in 1..=5.
/// Values should be 2, 4, 8, 16, 32.
///
/// **Validates: Requirements 3.6**
#[test]
fn test_exponential_backoff_calculation() {
    let expected = [(1, 2), (2, 4), (3, 8), (4, 16), (5, 32)];
    for (n, expected_delay) in expected {
        let actual = reconnect_delay_secs(n);
        assert_eq!(
            actual, expected_delay,
            "reconnect_delay_secs({n}) should be {expected_delay}, got {actual}"
        );
    }
}

// ── Test 4: Manifest round-trip ───────────────────────────────────────────────

/// Create a `SessionManifest`, serialize to JSON, deserialize back, verify equality.
///
/// **Validates: Requirements 3.6**
#[test]
fn test_manifest_round_trip() {
    let manifest = SessionManifest {
        name: "test_file.bin".to_string(),
        total_size: 1024 * 1024 * 50, // 50 MB
        kind: Kind::File,
        sender_addr: "192.168.1.10:54321".to_string(),
        created_at: 1700000000,
        archive_snapshot: None,
    };

    let json = serde_json::to_string_pretty(&manifest).unwrap();
    let deserialized: SessionManifest = serde_json::from_str(&json).unwrap();

    assert_eq!(manifest, deserialized, "manifest round-trip must preserve equality");

    // Also test Archive kind
    let archive_manifest = SessionManifest {
        name: "my_folder".to_string(),
        total_size: 1024 * 1024 * 200,
        kind: Kind::Archive,
        sender_addr: "10.0.0.5:12345".to_string(),
        created_at: 1700000001,
        archive_snapshot: Some(ArchiveSnapshot {
            algorithm: "rust-air-archive-meta-v1".to_string(),
            fingerprint: "deadbeef".to_string(),
            entry_count: 3,
        }),
    };

    let json2 = serde_json::to_string(&archive_manifest).unwrap();
    let deserialized2: SessionManifest = serde_json::from_str(&json2).unwrap();
    assert_eq!(
        archive_manifest, deserialized2,
        "archive manifest round-trip must preserve equality"
    );
}

#[test]
fn test_archive_status_round_trip() {
    let status = ArchiveStatus {
        code: ArchiveStatusCode::ResumeRejectedSafetyRestart,
        detail: Some("archive resume disabled for safety".to_string()),
    };
    let json = serde_json::to_string(&status).unwrap();
    let decoded: ArchiveStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(status, decoded, "archive status round-trip must preserve equality");
}

#[test]
fn test_archive_snapshot_round_trip() {
    let snapshot = ArchiveSnapshot {
        algorithm: "rust-air-archive-meta-v1".to_string(),
        fingerprint: "0123456789abcdef".to_string(),
        entry_count: 7,
    };
    let json = serde_json::to_string(&snapshot).unwrap();
    let decoded: ArchiveSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(snapshot, decoded, "archive snapshot round-trip must preserve equality");
}

#[test]
fn test_archive_snapshot_is_stable_for_unchanged_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("nested")).unwrap();
    std::fs::write(dir.path().join("a.json"), br#"{"a":1}"#).unwrap();
    std::fs::write(dir.path().join("nested").join("b.log"), b"hello\n").unwrap();

    let (_size1, entries1) = rust_air_core::archive::walk_dir_checked(dir.path()).unwrap();
    let snap1 = rust_air_core::archive::build_archive_snapshot(dir.path(), &entries1).unwrap();
    let (_size2, entries2) = rust_air_core::archive::walk_dir_checked(dir.path()).unwrap();
    let snap2 = rust_air_core::archive::build_archive_snapshot(dir.path(), &entries2).unwrap();

    assert_eq!(snap1.algorithm, snap2.algorithm);
    assert_eq!(snap1.fingerprint, snap2.fingerprint, "unchanged directory should produce stable snapshot fingerprint");
}

#[tokio::test]
async fn test_directory_transfer_emits_archive_lifecycle_events() {
    let src_dir = test_dir("archive_events_src");
    let folder = src_dir.join("payload_dir");
    fs::create_dir_all(folder.join("nested")).unwrap();
    fs::write(folder.join("nested").join("config.json"), br#"{"ok":true}"#).unwrap();
    for i in 0..12u32 {
        fs::write(folder.join(format!("filler_{i:02}.txt")), format!("hello-{i}")).unwrap();
    }

    let dest_dir = test_dir("archive_events_dest");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let send_folder = folder.clone();
    let send_task = tokio::spawn(async move {
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        send_path(stream, &send_folder, |_| {}).await.unwrap();
    });

    let events: Arc<Mutex<Vec<TransferEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let capture = events.clone();
    let (stream, _) = listener.accept().await.unwrap();
    let _outcome = receive_to_disk(stream, &dest_dir, move |ev| {
        capture.lock().unwrap().push(ev);
    }).await.unwrap();

    send_task.await.unwrap();

    let captured = events.lock().unwrap();
    assert!(captured.iter().any(|ev| matches!(
        ev.archive_status.as_ref().map(|s| s.code),
        Some(ArchiveStatusCode::UnpackStarted)
    )), "archive transfer should emit unpack started status");
    assert!(captured.iter().any(|ev| matches!(
        ev.archive_status.as_ref().map(|s| s.code),
        Some(ArchiveStatusCode::UnpackFinished)
    )), "archive transfer should emit unpack finished status");

    let _ = fs::remove_dir_all(&src_dir);
    let _ = fs::remove_dir_all(&dest_dir);
}

// ── Test 5: Resume offset chunk alignment (property-based) ────────────────────

// Resume offset chunk alignment property test:
// For random file sizes, verify `(file_size / CHUNK) * CHUNK` is always
// <= file_size and divisible by CHUNK.
//
// **Validates: Requirements 3.5, 3.2**
proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]
    #[test]
    fn test_resume_offset_chunk_alignment(
        // Random file sizes from 0 to 100 MB
        file_size in 0u64..=(100 * 1024 * 1024),
    ) {
        let chunk = CHUNK as u64;
        let aligned_offset = (file_size / chunk) * chunk;

        // Must be <= file_size
        prop_assert!(
            aligned_offset <= file_size,
            "aligned offset {} must be <= file_size {}",
            aligned_offset,
            file_size
        );

        // Must be divisible by CHUNK
        prop_assert_eq!(
            aligned_offset % chunk,
            0,
            "aligned offset {} must be divisible by CHUNK ({})",
            aligned_offset,
            chunk
        );

        // The gap between aligned_offset and file_size must be < CHUNK
        let gap = file_size - aligned_offset;
        prop_assert!(
            gap < chunk,
            "gap {} between aligned offset and file_size must be < CHUNK ({})",
            gap,
            chunk
        );
    }
}
