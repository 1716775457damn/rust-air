//! End-to-end integration test for file transfer over loopback TCP.
//!
//! Verifies that `send_path` and `receive_to_disk` correctly transfer a file
//! through the full pipeline (encryption, chunking, TCP, decryption, checksum)
//! and produce an identical copy on the receiver side.
//!
//! **Validates: Requirements 1.4, 2.3, 5.3, 7.1**

use rand::RngCore;
use rust_air_core::proto::ArchiveStatusCode;
use rust_air_core::transfer::{receive_to_disk, send_path};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

/// Create a unique temp directory for this test run.
fn test_dir(name: &str) -> PathBuf {
    let id: u64 = rand::random();
    let dir = std::env::temp_dir().join(format!("transfer_speed_test_{name}_{id:x}"));
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

/// End-to-end test: send a 2MB random file over loopback TCP via `Kind::File`,
/// then verify the received file is byte-identical to the original.
#[tokio::test]
async fn test_file_transfer_2mb_loopback() {
    const FILE_SIZE: usize = 2 * 1024 * 1024; // 2MB

    // Setup: create source file
    let src_dir = test_dir("src");
    let src_file = src_dir.join("random_2mb.bin");
    create_random_file(&src_file, FILE_SIZE);

    // Setup: create destination directory
    let dest_dir = test_dir("dest");

    // Start TCP listener on loopback
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Spawn sender task
    let send_file = src_file.clone();
    let send_task = tokio::spawn(async move {
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        send_path(stream, &send_file, |_| {}).await.unwrap();
    });

    // Accept connection and receive
    let (stream, _) = listener.accept().await.unwrap();
    let received_path = receive_to_disk(stream, &dest_dir, |_| {}).await.unwrap();

    // Wait for sender to finish
    send_task.await.unwrap();

    // Verify: received file exists and content matches
    assert!(received_path.path().exists(), "received file should exist");
    let original = fs::read(&src_file).unwrap();
    let received = fs::read(received_path.path()).unwrap();
    assert_eq!(
        original.len(),
        received.len(),
        "file sizes should match: expected {} got {}",
        original.len(),
        received.len()
    );
    assert_eq!(original, received, "file content should be byte-identical");

    // Cleanup
    let _ = fs::remove_dir_all(&src_dir);
    let _ = fs::remove_dir_all(&dest_dir);
}

/// End-to-end test with a larger file (4MB) to exercise multi-chunk transfer.
#[tokio::test]
async fn test_file_transfer_4mb_loopback() {
    const FILE_SIZE: usize = 4 * 1024 * 1024; // 4MB — spans multiple 1MB chunks

    let src_dir = test_dir("src_4mb");
    let src_file = src_dir.join("random_4mb.bin");
    create_random_file(&src_file, FILE_SIZE);

    let dest_dir = test_dir("dest_4mb");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let send_file = src_file.clone();
    let send_task = tokio::spawn(async move {
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        send_path(stream, &send_file, |_| {}).await.unwrap();
    });

    let (stream, _) = listener.accept().await.unwrap();
    let received_path = receive_to_disk(stream, &dest_dir, |_| {}).await.unwrap();

    send_task.await.unwrap();

    assert!(received_path.path().exists(), "received file should exist");
    let original = fs::read(&src_file).unwrap();
    let received = fs::read(received_path.path()).unwrap();
    assert_eq!(original.len(), received.len(), "file sizes should match");
    assert_eq!(original, received, "file content should be byte-identical");

    let _ = fs::remove_dir_all(&src_dir);
    let _ = fs::remove_dir_all(&dest_dir);
}

/// End-to-end test: send a directory with many small log/json files and verify
/// the full transfer pipeline preserves all expected files.
#[tokio::test]
async fn test_directory_transfer_preserves_small_log_and_json_files() {
    let src_dir = test_dir("dir_src");
    let folder = src_dir.join("payload_dir");
    fs::create_dir_all(folder.join("nested").join("deeper")).unwrap();

    let expected_files: Vec<(&str, Vec<u8>)> = vec![
        ("app.log", b"log-line-1\nlog-line-2\n".to_vec()),
        ("app.log.1", b"rotated-log\n".to_vec()),
        ("config.json", br#"{"enabled":true,"name":"rust-air"}"#.to_vec()),
        ("nested/settings.json", br#"{"theme":"dark","retries":3}"#.to_vec()),
        ("nested/deeper/trace.log", b"trace-start\ntrace-end\n".to_vec()),
        ("nested/empty.json", Vec::new()),
    ];

    for i in 0..16u32 {
        let filler_name = format!("filler_{i:02}.txt");
        let filler_content = format!("filler-file-{i}-{}", "x".repeat((i as usize % 32) + 8));
        fs::write(folder.join(filler_name), filler_content).unwrap();
    }

    for (rel_path, data) in &expected_files {
        let full_path = folder.join(rel_path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(full_path, data).unwrap();
    }

    let dest_dir = test_dir("dir_dest");
    let stale_dir = dest_dir.join("payload_dir.part");
    fs::write(&stale_dir, b"stale-partial-archive").unwrap();
    let stale_manifest = dest_dir.join("payload_dir.manifest.json");
    fs::write(
        &stale_manifest,
        serde_json::to_vec(&rust_air_core::proto::SessionManifest {
            name: "payload_dir".to_string(),
            total_size: 123,
            kind: rust_air_core::proto::Kind::Archive,
            sender_addr: String::new(),
            created_at: 1,
            archive_snapshot: None,
        }).unwrap(),
    ).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let send_folder = folder.clone();
    let send_task = tokio::spawn(async move {
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        send_path(stream, &send_folder, |_| {}).await.unwrap();
    });

    let events: Arc<Mutex<Vec<rust_air_core::proto::TransferEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let capture = events.clone();
    let (stream, _) = listener.accept().await.unwrap();
    let outcome = receive_to_disk(stream, &dest_dir, move |ev| {
        capture.lock().unwrap().push(ev);
    }).await.unwrap();

    send_task.await.unwrap();

    let captured = events.lock().unwrap();
    assert!(captured.iter().any(|ev| matches!(
        ev.archive_status.as_ref().map(|s| s.code),
        Some(ArchiveStatusCode::ResumeRejectedSafetyRestart)
    )), "directory transfer should reject stale archive resume and restart safely");

    let received_root = outcome.path().join(folder.file_name().unwrap());
    assert!(received_root.exists(), "received directory should exist");

    for (rel_path, expected_data) in &expected_files {
        let received_file = received_root.join(rel_path);
        assert!(
            received_file.exists(),
            "file {rel_path} should exist after directory transfer"
        );
        let actual_data = fs::read(&received_file).unwrap();
        assert_eq!(
            actual_data, *expected_data,
            "file {rel_path} content mismatch after directory transfer"
        );
    }

    let _ = fs::remove_dir_all(&src_dir);
    let _ = fs::remove_dir_all(&dest_dir);
}

/// End-to-end test: archive resume succeeds when snapshot fingerprint matches.
#[tokio::test]
async fn test_directory_transfer_resumes_when_snapshot_matches() {
    let src_dir = test_dir("dir_resume_src");
    let folder = src_dir.join("resume_dir");
    fs::create_dir_all(folder.join("nested")).unwrap();
    for i in 0..4u32 {
        let mut data = vec![0u8; 512 * 1024];
        rand::thread_rng().fill_bytes(&mut data);
        fs::write(folder.join(format!("blob_{i:02}.bin")), data).unwrap();
    }
    fs::write(folder.join("nested").join("trace.log"), b"trace-line\n").unwrap();

    let (total_size, entries) = rust_air_core::archive::walk_dir_checked(&folder).unwrap();
    let snapshot = rust_air_core::archive::build_archive_snapshot(&folder, &entries).unwrap();
    let compressed = {
        let reader = rust_air_core::archive::stream_archive_with_entries(&folder, entries).unwrap();
        let mut buf = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut tokio::io::BufReader::new(reader), &mut buf)
            .await
            .unwrap();
        buf
    };
    assert!(compressed.len() > rust_air_core::proto::CHUNK, "compressed archive should span at least one chunk for resume test");

    let dest_dir = test_dir("dir_resume_dest");
    let part_path = dest_dir.join("resume_dir.part");
    let partial_len = rust_air_core::proto::CHUNK;
    fs::write(&part_path, &compressed[..partial_len]).unwrap();
    let manifest_path = dest_dir.join("resume_dir.manifest.json");
    fs::write(
        &manifest_path,
        serde_json::to_vec(&rust_air_core::proto::SessionManifest {
            name: "resume_dir".to_string(),
            total_size,
            kind: rust_air_core::proto::Kind::Archive,
            sender_addr: String::new(),
            created_at: 1,
            archive_snapshot: Some(snapshot),
        }).unwrap(),
    ).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let send_folder = folder.clone();
    let send_task = tokio::spawn(async move {
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        send_path(stream, &send_folder, |_| {}).await.unwrap();
    });

    let events: Arc<Mutex<Vec<rust_air_core::proto::TransferEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let capture = events.clone();
    let (stream, _) = listener.accept().await.unwrap();
    let outcome = receive_to_disk(stream, &dest_dir, move |ev| {
        capture.lock().unwrap().push(ev);
    }).await.unwrap();

    send_task.await.unwrap();

    let captured = events.lock().unwrap();
    let restart_details: Vec<String> = captured
        .iter()
        .filter_map(|ev| ev.archive_status.as_ref())
        .filter(|s| s.code == ArchiveStatusCode::ResumeRejectedSafetyRestart)
        .filter_map(|s| s.detail.clone())
        .collect();
    assert!(restart_details.is_empty(),
        "matching archive snapshot should not force a safety restart, got: {:?}", restart_details);
    assert!(captured.iter().any(|ev| ev.resumed && ev.resume_offset as usize >= partial_len),
        "matching archive snapshot should allow resumed archive transfer");

    let received_root = outcome.path().join(folder.file_name().unwrap());
    assert!(received_root.exists(), "received resumed directory should exist");
    let trace = fs::read(received_root.join("nested").join("trace.log")).unwrap();
    assert_eq!(trace, b"trace-line\n");

    let _ = fs::remove_dir_all(&src_dir);
    let _ = fs::remove_dir_all(&dest_dir);
}
