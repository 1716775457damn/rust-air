use anyhow::Result;
use chacha20poly1305::{
    aead::{AeadInPlace, KeyInit},
    ChaCha20Poly1305, Key, Nonce,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub struct Encryptor<W> {
    cipher:  ChaCha20Poly1305,
    inner:   W,
    counter: u64,
}

impl<W: AsyncWrite + Unpin> Encryptor<W> {
    pub fn new(key: &[u8; 32], inner: W) -> Self {
        Self { cipher: ChaCha20Poly1305::new(Key::from_slice(key)), inner, counter: 0 }
    }

    pub async fn write_chunk(&mut self, plaintext: &[u8]) -> Result<()> {
        // Reuse a single allocation: copy plaintext then encrypt in-place
        let mut buf = Vec::with_capacity(plaintext.len());
        buf.extend_from_slice(plaintext);
        let nonce = nonce_from(self.counter);
        let tag = self.cipher
            .encrypt_in_place_detached(&nonce, b"", &mut buf)
            .map_err(|e| anyhow::anyhow!("encrypt: {e}"))?;
        self.counter += 1;
        // Write all three fields in one syscall-friendly sequence
        let len_bytes = (plaintext.len() as u32).to_be_bytes();
        self.inner.write_all(&len_bytes).await?;
        self.inner.write_all(&tag).await?;
        self.inner.write_all(&buf).await?;
        Ok(())
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        self.inner.write_all(&0u32.to_be_bytes()).await?;
        self.inner.flush().await?;
        Ok(())
    }
}

pub struct Decryptor<R> {
    cipher:  ChaCha20Poly1305,
    inner:   R,
    counter: u64,
}

impl<R: AsyncRead + Unpin> Decryptor<R> {
    pub fn new(key: &[u8; 32], inner: R) -> Self {
        Self { cipher: ChaCha20Poly1305::new(Key::from_slice(key)), inner, counter: 0 }
    }

    pub async fn read_chunk(&mut self) -> Result<Option<Vec<u8>>> {
        let mut len_buf = [0u8; 4];
        self.inner.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len == 0 { return Ok(None); }

        let mut tag_buf = [0u8; 16];
        self.inner.read_exact(&mut tag_buf).await?;
        let tag = chacha20poly1305::Tag::from_slice(&tag_buf);

        let mut buf = vec![0u8; len];
        self.inner.read_exact(&mut buf).await?;

        let nonce = nonce_from(self.counter);
        self.cipher
            .decrypt_in_place_detached(&nonce, b"", &mut buf, tag)
            .map_err(|_| anyhow::anyhow!("decryption failed — wrong key or corrupted data"))?;
        self.counter += 1;
        Ok(Some(buf))
    }
}

fn nonce_from(counter: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[..8].copy_from_slice(&counter.to_le_bytes());
    Nonce::from(n)
}
