//! ChaCha20-Poly1305 streaming AEAD encryption/decryption.
//!
//! Wire frame layout per chunk:
//!   [4B plaintext_len (u32 BE)] [16B AEAD tag] [ciphertext]
//!
//! Nonce construction: 8-byte little-endian frame counter ++ 4 zero bytes.
//! Counter is monotonically increasing — nonces are never reused under the same key.
//! End-of-stream sentinel: a single frame with plaintext_len == 0.
//!
//! Optimization: header + tag + ciphertext are written in a single write_all call
//! to minimize system call overhead on high-frequency small chunks.

use anyhow::Result;
use crate::proto::CHUNK;
use chacha20poly1305::{
    aead::{AeadInPlace, KeyInit},
    ChaCha20Poly1305, Key, Nonce,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

// ── Encryptor ─────────────────────────────────────────────────────────────────

pub struct Encryptor<W> {
    cipher:  ChaCha20Poly1305,
    inner:   W,
    counter: u64,
    /// Reusable frame buffer: [4B len][16B tag][ciphertext] — avoids per-chunk allocation.
    frame_buf: Vec<u8>,
}

impl<W: AsyncWrite + Unpin> Encryptor<W> {
    pub fn new(key: &[u8; 32], inner: W) -> Self {
        use crate::proto::CHUNK;
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(key)),
            inner,
            counter: 0,
            frame_buf: Vec::with_capacity(4 + 16 + CHUNK),
        }
    }

    /// Encrypt `plaintext` and write `[len][tag][ciphertext]` in a single syscall.
    /// Reuses an internal buffer to avoid per-chunk heap allocation.
    pub async fn write_chunk(&mut self, plaintext: &[u8]) -> Result<()> {
        // Build frame in reusable buffer: [4B len][16B tag placeholder][ciphertext]
        self.frame_buf.clear();
        self.frame_buf.extend_from_slice(&(plaintext.len() as u32).to_be_bytes());
        self.frame_buf.extend_from_slice(&[0u8; 16]); // tag placeholder
        self.frame_buf.extend_from_slice(plaintext);

        let nonce = frame_nonce(self.counter);
        let ciphertext_start = 4 + 16;
        let tag = self.cipher
            .encrypt_in_place_detached(&nonce, b"", &mut self.frame_buf[ciphertext_start..])
            .map_err(|e| anyhow::anyhow!("encrypt frame {}: {e}", self.counter))?;
        self.frame_buf[4..20].copy_from_slice(tag.as_slice());
        self.counter += 1;

        self.inner.write_all(&self.frame_buf).await?;
        Ok(())
    }

    /// Write the end-of-stream sentinel (zero-length frame) and flush.
    pub async fn shutdown(&mut self) -> Result<()> {
        self.inner.write_all(&0u32.to_be_bytes()).await?;
        self.inner.flush().await?;
        Ok(())
    }

    /// Write trailing bytes after the EOF sentinel (e.g. SHA-256 checksum).
    /// Must be called after `shutdown()`.
    pub async fn write_trailing(&mut self, data: &[u8]) -> Result<()> {
        self.inner.write_all(data).await?;
        self.inner.flush().await?;
        Ok(())
    }
}

// ── Decryptor ─────────────────────────────────────────────────────────────────

pub struct Decryptor<R> {
    cipher:    ChaCha20Poly1305,
    inner:     R,
    counter:   u64,
    /// Reusable ciphertext buffer — decrypted in-place, ownership transferred to caller.
    data_buf:  Vec<u8>,
}

impl<R: AsyncRead + Unpin> Decryptor<R> {
    pub fn new(key: &[u8; 32], inner: R) -> Self {
        Self {
            cipher:   ChaCha20Poly1305::new(Key::from_slice(key)),
            inner,
            counter:  0,
            data_buf: Vec::with_capacity(CHUNK),
        }
    }

    pub async fn read_trailing(&mut self) -> Result<[u8; 32]> {
        let mut buf = [0u8; 32];
        self.inner.read_exact(&mut buf).await?;
        Ok(buf)
    }

    /// Read and authenticate one frame. Returns `None` on end-of-stream sentinel.
    /// Tag and ciphertext are read into separate fixed buffers — zero shift, zero copy.
    pub async fn read_chunk(&mut self) -> Result<Option<Vec<u8>>> {
        let mut len_buf = [0u8; 4];
        self.inner.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len == 0 { return Ok(None); }

        // Read tag (16 B) into a stack buffer — no heap allocation.
        let mut tag_bytes = [0u8; 16];
        self.inner.read_exact(&mut tag_bytes).await?;
        let tag = chacha20poly1305::Tag::from(tag_bytes);

        // Read ciphertext into reusable buffer, decrypt in-place.
        self.data_buf.resize(len, 0);
        self.inner.read_exact(&mut self.data_buf[..len]).await?;

        let nonce = frame_nonce(self.counter);
        self.cipher
            .decrypt_in_place_detached(&nonce, b"", &mut self.data_buf[..len], &tag)
            .map_err(|_| anyhow::anyhow!(
                "decryption failed on frame {} — wrong key or data corrupted",
                self.counter
            ))?;
        self.counter += 1;

        // Transfer ownership of the plaintext buffer; replace with a fresh one.
        let mut out = std::mem::replace(&mut self.data_buf, Vec::with_capacity(CHUNK));
        out.truncate(len);
        Ok(Some(out))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a 12-byte nonce from an 8-byte frame counter (little-endian) + 4 zero bytes.
#[inline]
fn frame_nonce(counter: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[..8].copy_from_slice(&counter.to_le_bytes());
    Nonce::from(n)
}
