use anyhow::Result;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use clap::{Parser, Subcommand};
use rand::RngCore;
use rust_air_core::{
    clipboard, discovery, http_qr, transfer,
    proto::TransferEvent,
};
use std::path::PathBuf;
use tokio::net::{TcpListener, TcpStream};

#[derive(Parser)]
#[command(name = "rust-air", about = "LAN file transfer — AirDrop for the terminal")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Send a file or folder
    Send {
        path: PathBuf,
        #[arg(long, help = "Print QR code for phone/browser download")]
        qr: bool,
    },
    /// Send clipboard to another machine
    #[command(name = "send-clip")]
    SendClip,
    /// Receive — enter the instance name shown by the sender
    Receive {
        /// Instance name shown by sender (e.g. "rust-air-abc123")
        name: String,
        #[arg(long, default_value = ".")]
        out: PathBuf,
    },
    /// Scan LAN for available senders
    Scan,
}

#[tokio::main]
async fn main() -> Result<()> {
    // On Windows, if launched by double-click (no args), show help and wait
    #[cfg(target_os = "windows")]
    if std::env::args().len() == 1 {
        eprintln!("rust-air — LAN file transfer\n");
        eprintln!("Usage examples:");
        eprintln!("  rust-air send photo.jpg");
        eprintln!("  rust-air receive rust-air-XXXXXXXX:KEY");
        eprintln!("  rust-air scan\n");
        eprintln!("Press Enter to exit...");
        let mut s = String::new();
        let _ = std::io::stdin().read_line(&mut s);
        return Ok(());
    }

    match Cli::parse().cmd {
        Cmd::Send { path, qr }    => cmd_send(path, qr).await,
        Cmd::SendClip              => cmd_send_clip().await,
        Cmd::Receive { name, out } => cmd_receive(name, out).await,
        Cmd::Scan                  => cmd_scan().await,
    }
}

// ── Send ──────────────────────────────────────────────────────────────────────

async fn cmd_send(path: PathBuf, qr: bool) -> Result<()> {
    anyhow::ensure!(path.exists(), "path not found: {}", path.display());

    let key = random_key();
    let listener = TcpListener::bind("0.0.0.0:0").await?;
    let port = listener.local_addr()?.port();
    let instance = format!("rust-air-{}", &encode_key(&key)[..8]);

    println!("📦 Sending  : {}", path.display());
    println!("🔑 Name     : {instance}");
    println!("🔑 Key      : {}", encode_key(&key));
    println!("🔒 E2EE ChaCha20-Poly1305 + SHA-256 verify");

    if qr {
        let p = path.clone();
        tokio::spawn(async move { let _ = http_qr::serve_and_print_qr(p).await; });
    }

    let _mdns = discovery::register_sender(port, &instance)?;
    println!("⏳ Waiting for receiver…\n");

    let (stream, peer) = listener.accept().await?;
    println!("🔗 Connected: {peer}\n");

    transfer::send_path(stream, &path, &key, noop_progress).await?;
    println!("\n✅ Transfer complete!");
    Ok(())
}

async fn cmd_send_clip() -> Result<()> {
    let text = clipboard::read()?;
    println!("📋 Clipboard: {} chars", text.len());

    let key = random_key();
    let listener = TcpListener::bind("0.0.0.0:0").await?;
    let port = listener.local_addr()?.port();
    let instance = format!("rust-air-clip-{}", &encode_key(&key)[..8]);

    println!("🔑 Name: {instance}");
    println!("⏳ Waiting…\n");

    let _mdns = discovery::register_sender(port, &instance)?;
    let (stream, peer) = listener.accept().await?;
    println!("🔗 Connected: {peer}\n");

    transfer::send_clipboard(stream, text, &key).await?;
    println!("✅ Clipboard sent!");
    Ok(())
}

// ── Receive ───────────────────────────────────────────────────────────────────

async fn cmd_receive(instance_name: String, out: PathBuf) -> Result<()> {
    // Parse "name:key" format — key is passed separately for security
    let (name, key) = if let Some((n, k)) = instance_name.split_once(':') {
        let kb = URL_SAFE_NO_PAD.decode(k)?;
        anyhow::ensure!(kb.len() == 32, "key must be 32 bytes base64url");
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&kb);
        (n.to_string(), arr)
    } else {
        anyhow::bail!("format: rust-air receive <instance-name>:<base64-key>");
    };

    println!("🔍 Resolving '{name}' via mDNS…");
    let (ip, port) = discovery::resolve_sender(&name).await?;
    println!("🔗 Found at {ip}:{port}\n");

    let stream = TcpStream::connect((ip.as_str(), port)).await?;
    tokio::fs::create_dir_all(&out).await?;

    let saved = transfer::receive_to_disk(stream, &key, &out, noop_progress).await?;
    println!("\n✅ Saved to: {}", saved.display());
    Ok(())
}

async fn cmd_scan() -> Result<()> {
    println!("🔍 Scanning LAN for rust-air senders (Ctrl-C to stop)…\n");
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    tokio::spawn(discovery::browse_devices(tx));
    while let Some(dev) = rx.recv().await {
        if dev.addr.is_empty() {
            println!("  ✗ Gone   : {}", dev.name);
        } else {
            println!("  ✓ Found  : {}  @ {}  [{:?}]", dev.name, dev.addr, dev.status);
        }
    }
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn random_key() -> [u8; 32] {
    let mut k = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut k);
    k
}

fn encode_key(key: &[u8; 32]) -> String {
    URL_SAFE_NO_PAD.encode(key)
}

fn noop_progress(_: TransferEvent) {}
