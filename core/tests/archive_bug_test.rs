//! Bug condition exploration test for archive transfer premature completion.
//!
//! This test demonstrates the bug: when a file disappears between `walk_dir`
//! and `compress_entries`, the archive stream silently returns EOF instead of
//! propagating an error. On UNFIXED code, this test is EXPECTED TO FAIL
//! (i.e., the assertion that read returns Err will fail because the buggy code
//! returns Ok with partial data).
//!
//! **Validates: Requirements 1.1, 2.1**

use rust_air_core::archive;
use std::fs;

/// Creates a temp directory with two files, walks it, then deletes one file
/// before calling `stream_archive_with_entries`. This simulates a file that
/// becomes unreadable between walk and compress (works reliably on Windows).
///
/// On UNFIXED code: the reader returns Ok (silent EOF with partial data).
/// On FIXED code: the reader returns Err (error propagated from compression thread).
#[tokio::test]
async fn stream_archive_returns_error_when_file_disappears_after_walk() {
    // 1. Create a temp directory with two files
    let tmp_dir = std::env::temp_dir().join("archive_bug_test_disappearing_file");
    let _ = fs::remove_dir_all(&tmp_dir); // clean up from previous runs
    fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let good_file = tmp_dir.join("good.txt");
    let bad_file = tmp_dir.join("bad.txt");

    fs::write(&good_file, "hello world - this file is readable").expect("write good.txt");
    fs::write(&bad_file, "this file will disappear before compression").expect("write bad.txt");

    // 2. Call walk_dir to get entries (both files are present at this point)
    let (_total_size, entries) = archive::walk_dir(&tmp_dir);

    // Verify both files were found
    let file_count = entries.iter().filter(|(e, _)| e.file_type().is_file()).count();
    assert_eq!(file_count, 2, "walk_dir should find both files");

    // 3. Delete bad.txt AFTER walk_dir but BEFORE stream_archive_with_entries
    //    This simulates a file that becomes unreadable between walk and compress.
    fs::remove_file(&bad_file).expect("delete bad.txt to simulate disappearing file");

    // 4. Call stream_archive_with_entries with the stale entries
    let reader = archive::stream_archive_with_entries(&tmp_dir, entries)
        .expect("stream_archive_with_entries should return Ok (it creates the pipe)");

    // 5. Read the AsyncRead to completion
    let mut buf = Vec::new();
    let result = tokio::io::AsyncReadExt::read_to_end(&mut tokio::io::BufReader::new(reader), &mut buf).await;

    // 6. Assert that reading returns an Err (not Ok with partial data)
    //
    // On UNFIXED code: this assertion FAILS because the buggy code silently
    // swallows the compression error and returns EOF (Ok with partial data).
    //
    // On FIXED code: this assertion PASSES because the error is propagated
    // through the AsyncRead as an io::Error.
    assert!(
        result.is_err(),
        "BUG CONFIRMED: stream_archive_with_entries returned Ok({} bytes) instead of Err \
         when a file disappeared after walk_dir. The compression error was silently swallowed.",
        buf.len()
    );

    // Cleanup
    let _ = fs::remove_dir_all(&tmp_dir);
}
