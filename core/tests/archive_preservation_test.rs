//! Preservation property tests for archive functionality.
//!
//! These tests verify baseline behavior of `stream_archive_with_entries` for
//! fully-readable directories. They MUST PASS on the current unfixed code,
//! confirming the behavior we need to preserve after the bugfix.
//!
//! **Validates: Requirements 3.1, 3.4, 3.5**

use rust_air_core::archive;
use std::fs;
use std::io::Cursor;
use std::path::Path;

/// Helper: create a unique temp directory for each test to avoid conflicts.
fn test_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("archive_preservation_{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Helper: read the full archive stream to bytes.
async fn archive_to_bytes(path: &Path) -> Vec<u8> {
    let (_, entries) = archive::walk_dir(path);
    let reader = archive::stream_archive_with_entries(path, entries)
        .expect("stream_archive_with_entries should succeed for readable dirs");
    let mut buf = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(
        &mut tokio::io::BufReader::new(reader),
        &mut buf,
    )
    .await
    .expect("reading archive stream should succeed for readable dirs");
    buf
}

/// Test: single file directory archives and unpacks correctly.
///
/// **Validates: Requirements 3.1, 3.4**
#[tokio::test]
async fn test_archive_single_file_dir() {
    let src = test_dir("single_file");
    let content = b"Hello, archive preservation test!";
    fs::write(src.join("hello.txt"), content).unwrap();

    // Archive
    let compressed = archive_to_bytes(&src).await;
    assert!(!compressed.is_empty(), "archive stream should produce bytes");

    // Unpack into a separate directory
    let dest = test_dir("single_file_out");
    archive::unpack_archive_sync(Cursor::new(&compressed), &dest)
        .expect("unpack_archive_sync should succeed");

    // The archive wraps files under the source dir name
    let dir_name = src.file_name().unwrap().to_str().unwrap();
    let unpacked_file = dest.join(dir_name).join("hello.txt");
    assert!(unpacked_file.exists(), "unpacked file should exist");
    let unpacked_content = fs::read(&unpacked_file).unwrap();
    assert_eq!(unpacked_content, content, "file content should match");

    // Cleanup
    let _ = fs::remove_dir_all(&src);
    let _ = fs::remove_dir_all(&dest);
}

/// Test: multiple files of varying sizes archive and unpack correctly.
///
/// **Validates: Requirements 3.1, 3.4, 3.5**
#[tokio::test]
async fn test_archive_multiple_files() {
    let src = test_dir("multiple_files");

    // Create files of varying sizes: 0B, 100B, 10KB, 100KB, 1MB
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("empty.bin", vec![]),
        ("small.bin", vec![0xAB; 100]),
        ("medium.bin", vec![0xCD; 10 * 1024]),
        ("large.bin", vec![0xEF; 100 * 1024]),
        ("huge.bin", vec![0x42; 1024 * 1024]),
    ];

    for (name, data) in &files {
        fs::write(src.join(name), data).unwrap();
    }

    // Archive
    let compressed = archive_to_bytes(&src).await;
    assert!(!compressed.is_empty(), "archive stream should produce bytes");

    // Unpack
    let dest = test_dir("multiple_files_out");
    archive::unpack_archive_sync(Cursor::new(&compressed), &dest)
        .expect("unpack_archive_sync should succeed");

    // Verify all files present with correct content
    let dir_name = src.file_name().unwrap().to_str().unwrap();
    for (name, expected_data) in &files {
        let unpacked_file = dest.join(dir_name).join(name);
        assert!(
            unpacked_file.exists(),
            "file {name} should exist after unpack"
        );
        let actual_data = fs::read(&unpacked_file).unwrap();
        assert_eq!(
            actual_data.len(),
            expected_data.len(),
            "file {name} size mismatch"
        );
        assert_eq!(
            actual_data, *expected_data,
            "file {name} content mismatch"
        );
    }

    // Cleanup
    let _ = fs::remove_dir_all(&src);
    let _ = fs::remove_dir_all(&dest);
}

/// Test: nested directories with files at different levels archive and unpack correctly.
///
/// **Validates: Requirements 3.1, 3.4**
#[tokio::test]
async fn test_archive_nested_dirs() {
    let src = test_dir("nested_dirs");

    // Create nested structure:
    // src/
    //   root.txt
    //   sub1/
    //     file1.txt
    //     sub1a/
    //       deep.txt
    //   sub2/
    //     file2.txt
    fs::create_dir_all(src.join("sub1").join("sub1a")).unwrap();
    fs::create_dir_all(src.join("sub2")).unwrap();

    let file_entries: Vec<(&str, &[u8])> = vec![
        ("root.txt", b"root level file"),
        ("sub1/file1.txt", b"first subdirectory file"),
        ("sub1/sub1a/deep.txt", b"deeply nested file content here"),
        ("sub2/file2.txt", b"second subdirectory file"),
    ];

    for (rel_path, data) in &file_entries {
        fs::write(src.join(rel_path), data).unwrap();
    }

    // Archive
    let compressed = archive_to_bytes(&src).await;
    assert!(!compressed.is_empty(), "archive stream should produce bytes");

    // Unpack
    let dest = test_dir("nested_dirs_out");
    archive::unpack_archive_sync(Cursor::new(&compressed), &dest)
        .expect("unpack_archive_sync should succeed");

    // Verify all files present with correct content
    let dir_name = src.file_name().unwrap().to_str().unwrap();
    for (rel_path, expected_data) in &file_entries {
        let unpacked_file = dest.join(dir_name).join(rel_path);
        assert!(
            unpacked_file.exists(),
            "file {rel_path} should exist after unpack"
        );
        let actual_data = fs::read(&unpacked_file).unwrap();
        assert_eq!(
            actual_data, *expected_data,
            "file {rel_path} content mismatch"
        );
    }

    // Cleanup
    let _ = fs::remove_dir_all(&src);
    let _ = fs::remove_dir_all(&dest);
}

/// Test: directory with only subdirectories (no files) archives without error.
///
/// **Validates: Requirements 3.1**
#[tokio::test]
async fn test_archive_empty_dir() {
    let src = test_dir("empty_dir");

    // Create subdirectories only, no files
    fs::create_dir_all(src.join("subdir_a")).unwrap();
    fs::create_dir_all(src.join("subdir_b").join("nested")).unwrap();
    fs::create_dir_all(src.join("subdir_c")).unwrap();

    // Archive — should complete without error
    let compressed = archive_to_bytes(&src).await;
    // Even with no files, the archive should produce some bytes (tar headers for dirs)
    assert!(
        !compressed.is_empty(),
        "archive of empty dirs should still produce bytes"
    );

    // Unpack should succeed without error
    let dest = test_dir("empty_dir_out");
    archive::unpack_archive_sync(Cursor::new(&compressed), &dest)
        .expect("unpack_archive_sync should succeed for directory-only archive");

    // Cleanup
    let _ = fs::remove_dir_all(&src);
    let _ = fs::remove_dir_all(&dest);
}

/// Test: walk_dir returns correct total_size and entry count.
///
/// **Validates: Requirements 3.1**
#[tokio::test]
async fn test_walk_dir_total_size() {
    let src = test_dir("walk_dir_size");

    // Create files with known sizes
    let file_sizes: Vec<(&str, usize)> = vec![
        ("a.txt", 100),
        ("b.txt", 500),
        ("sub/c.txt", 1024),
        ("sub/deep/d.txt", 2048),
    ];

    let expected_total: u64 = file_sizes.iter().map(|(_, sz)| *sz as u64).sum();

    for (rel_path, size) in &file_sizes {
        let full_path = src.join(rel_path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&full_path, vec![0u8; *size]).unwrap();
    }

    let (total_size, entries) = archive::walk_dir(&src);

    // Verify total_size matches sum of file sizes
    assert_eq!(
        total_size, expected_total,
        "walk_dir total_size should equal sum of file sizes"
    );

    // Verify entry count: files + directories (including root)
    let file_count = entries.iter().filter(|(e, _)| e.file_type().is_file()).count();
    assert_eq!(
        file_count,
        file_sizes.len(),
        "walk_dir should find all files"
    );

    // Verify directory entries are also present
    // Directories: root (src), sub, sub/deep = 3 dirs
    let dir_count = entries.iter().filter(|(e, _)| e.file_type().is_dir()).count();
    assert_eq!(dir_count, 3, "walk_dir should find root + 2 subdirectories");

    // Cleanup
    let _ = fs::remove_dir_all(&src);
}
