//! End-to-end integration test for file transfer over loopback TCP.
//!
//! Verifies that `send_path` and `receive_to_disk` correctly transfer a file
//! through the full pipeline (encryption, chunking, TCP, decryption, checksum)
//! and produce an identical copy on the receiver side.
//!
//! **Validates: Requirements 1.4, 2.3, 5.3, 7.1**

use rand::RngCore;
use rust_air_core::transfer::{receive_to_disk, send_path};
use std::fs;
use std::path::PathBuf;
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
