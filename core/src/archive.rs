use anyhow::Result;
use std::path::Path;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

pub fn stream_archive(path: &Path) -> Result<impl AsyncRead + Send + Unpin + 'static> {
    let (pipe_reader, pipe_writer) = os_pipe::pipe()?;
    let path = path.to_path_buf();

    std::thread::spawn(move || {
        let r = (|| -> Result<()> {
            let enc = zstd::Encoder::new(pipe_writer, 3)?;
            let mut tar = tar::Builder::new(enc);
            if std::fs::metadata(&path)?.is_dir() {
                tar.append_dir_all(path.file_name().unwrap_or_default(), &path)?;
            } else {
                tar.append_path_with_name(&path, path.file_name().unwrap_or_default())?;
            }
            tar.into_inner()?.finish()?;
            Ok(())
        })();
        if let Err(e) = r { eprintln!("archive error: {e}"); }
    });

    let (async_writer, async_reader) = tokio::io::duplex(4 * 1024 * 1024);
    tokio::spawn(pump(pipe_reader, async_writer));
    Ok(async_reader)
}

async fn pump(mut src: os_pipe::PipeReader, mut dst: impl AsyncWrite + Unpin) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        use std::io::Read;
        match src.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => { if dst.write_all(&buf[..n]).await.is_err() { break; } }
        }
    }
    let _ = dst.shutdown().await;
}

pub fn unpack_archive_sync(reader: impl std::io::Read, dest: &Path) -> Result<()> {
    let mut tar = tar::Archive::new(zstd::Decoder::new(reader)?);
    tar.unpack(dest)?;
    Ok(())
}
