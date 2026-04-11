//! ChaCha20-Poly1305 streaming AEAD encryption/decryption.
//!
//! Wire frame layout per chunk:
//!   [4B plaintext_len (u32 BE)] [16B AEAD tag] [ciphertext]
//!
//! Nonce construction: 8-byte little-endian frame counter ++ 4 zero bytes.
//! Counter is monotonically increasing — nonces are never reused under the same key.
//! End-of-stream sentinel: a single frame with plaintext_len == 0.

use anyhow::Result;
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
}

impl<W: AsyncWrite + Unpin> Encryptor<W> {
    pub fn new(key: &[u8; 32], inner: W) -> Self {
        Self { cipher: ChaCha20Poly1305::new(Key::from_slice(key)), inner, counter: 0 }
    }

    /// Encrypt `plaintext` and write `[len][tag][ciphertext]` to the inner writer.
    pub async fn write_chunk(&mut self, plaintext: &[u8]) -> Result<()> {
        // Copy into buf then encrypt in-place — single allocation, no second Vec.
        let mut buf = plaintext.to_vec();
        let nonce = frame_nonce(self.counter);
        let tag = self.cipher
            .encrypt_in_place_detached(&nonce, b"", &mut buf)
            .map_err(|e| anyhow::anyhow!("encrypt frame {}: {e}", self.counter))?;
        self.counter += 1;

        // Three sequential writes; tokio/OS coalesces them in the socket send buffer.
        self.inner.write_all(&(plaintext.len() as u32).to_be_bytes()).await?;
        self.inner.write_all(tag.as_slice()).await?;
        self.inner.write_all(&buf).await?;
        Ok(())
    }

    /// Write the end-of-stream sentinel (zero-length frame) and flush.
    pub async fn shutdown(&mut self) -> Result<()> {
        self.inner.write_all(&0u32.to_be_bytes()).await?;
        self.inner.flush().await?;
        Ok(())
    }
}

// ── Decryptor ─────────────────────────────────────────────────────────────────

pub struct Decryptor<R> {
    cipher:  ChaCha20Poly1305,
    inner:   R,
    counter: u64,
}

impl<R: AsyncRead + Unpin> Decryptor<R> {
    pub fn new(key: &[u8; 32], inner: R) -> Self {
        Self { cipher: ChaCha20Poly1305::new(Key::from_slice(key)), inner, counter: 0 }
    }

    /// Read and authenticate one frame. Returns `None` on end-of-stream sentinel.
    pub async fn read_chunk(&mut self) -> Result<Option<Vec<u8>>> {
        let mut len_buf = [0u8; 4];
        self.inner.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len == 0 {
            return Ok(None);
        }

        let mut tag_buf = [0u8; 16];
        self.inner.read_exact(&mut tag_buf).await?;
        let tag = chacha20poly1305::Tag::from_slice(&tag_buf);

        let mut buf = vec![0u8; len];
        self.inner.read_exact(&mut buf).await?;

        let nonce = frame_nonce(self.counter);
        self.cipher
            .decrypt_in_place_detached(&nonce, b"", &mut buf, tag)
            .map_err(|_| anyhow::anyhow!(
                "decryption failed on frame {} — wrong key or data corrupted",
                self.counter
            ))?;
        self.counter += 1;
        Ok(Some(buf))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a 12-byte nonce from an 8-byte frame counter (little-endian) + 4 zero bytes.
/// Counter monotonically increases per session — nonces are never reused under the same key.
#[inline]
fn frame_nonce(counter: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[..8].copy_from_slice(&counter.to_le_bytes());
    Nonce::from(n)
}
