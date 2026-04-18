use anyhow::Result;
use clap::{Parser, Subcommand};
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
    /// Send a file or folder to a device on the LAN
    Send {
        path: PathBuf,
        /// Target device address "ip:port" (from `scan`), or omit to wait for connection
        #[arg(long)]
        to: Option<String>,
        #[arg(long, help = "Print QR code for phone/browser download")]
        qr: bool,
    },
    /// Send clipboard to another machine
    #[command(name = "send-clip")]
    SendClip {
        /// Target device address "ip:port" (from `scan`)
        to: String,
    },
    /// Listen for incoming files (auto-receive mode)
    Receive {
        #[arg(long, default_value = ".")]
        out: PathBuf,
    },
    /// Scan LAN for available rust-air devices
    Scan,
}

#[tokio::main]
async fn main() -> Result<()> {
    #[cfg(target_os = "windows")]
    if std::env::args().len() == 1 {
        eprintln!("rust-air — LAN file transfer\n");
        eprintln!("Usage examples:");
        eprintln!("  rust-air scan");
        eprintln!("  rust-air send photo.jpg --to 192.168.1.5:49821");
        eprintln!("  rust-air receive --out ~/Downloads\n");
        eprintln!("Press Enter to exit...");
        let mut s = String::new();
        let _ = std::io::stdin().read_line(&mut s);
        return Ok(());
    }

    match Cli::parse().cmd {
        Cmd::Send { path, to, qr } => cmd_send(path, to, qr).await,
        Cmd::SendClip { to }       => cmd_send_clip(to).await,
        Cmd::Receive { out }       => cmd_receive(out).await,
        Cmd::Scan                  => cmd_scan().await,
    }
}

// ── Send ──────────────────────────────────────────────────────────────────────

async fn cmd_send(path: PathBuf, to: Option<String>, qr: bool) -> Result<()> {
    anyhow::ensure!(path.exists(), "path not found: {}", path.display());

    if qr {
        let p = path.clone();
        tokio::spawn(async move { let _ = http_qr::serve_and_print_qr(p).await; });
    }

    let addr = match to {
        Some(a) => a,
        None => {
            // No target specified: scan and let user pick
            println!("🔍 Scanning LAN for rust-air devices…\n");
            let devs = scan_once(3).await?;
            if devs.is_empty() {
                anyhow::bail!("no devices found — make sure the receiver is running rust-air");
            }
            for (i, d) in devs.iter().enumerate() {
                println!("  [{}] {}  @ {}", i + 1, d.0, d.1);
            }
            print!("\nSelect device (1-{}): ", devs.len());
            use std::io::{Write, BufRead};
            std::io::stdout().flush()?;
            let mut line = String::new();
            std::io::stdin().lock().read_line(&mut line)?;
            let idx: usize = line.trim().parse::<usize>()?.saturating_sub(1);
            devs.get(idx).map(|d| d.1.clone())
                .ok_or_else(|| anyhow::anyhow!("invalid selection"))?
        }
    };

    println!("📦 Sending  : {}", path.display());
    println!("🔗 Connecting to {addr}…");
    let stream = TcpStream::connect(&addr).await?;
    println!("🔒 E2EE ChaCha20-Poly1305 + SHA-256 verify\n");

    transfer::send_path(stream, &path, noop_progress).await?;
    println!("\n✅ Transfer complete!");
    Ok(())
}

async fn cmd_send_clip(to: String) -> Result<()> {
    let text = clipboard::read()?;
    println!("📋 Clipboard: {} chars", text.len());
    println!("🔗 Connecting to {to}…");
    let stream = TcpStream::connect(&to).await?;
    println!("🔒 E2EE ChaCha20-Poly1305 + SHA-256 verify\n");
    transfer::send_clipboard(stream, &text, noop_progress).await?;
    println!("\n✅ Clipboard sent!");
    Ok(())
}

// ── Receive ───────────────────────────────────────────────────────────────────

async fn cmd_receive(out: PathBuf) -> Result<()> {
    let listener = TcpListener::bind("0.0.0.0:0").await?;
    let port = listener.local_addr()?.port();
    let name = device_name();

    let _handle = discovery::register_self(port, &name)?;
    println!("📥 Listening on port {port}  (registered as '{name}')");
    println!("⏳ Waiting for sender… (Ctrl-C to stop)\n");

    loop {
        let (stream, peer) = listener.accept().await?;
        println!("🔗 Connected: {peer}");
        tokio::fs::create_dir_all(&out).await?;
        let out2 = out.clone();
        tokio::spawn(async move {
            match transfer::receive_to_disk(stream, &out2, noop_progress).await {
                Ok(p)  => println!("\n✅ Saved to: {}", p.display()),
                Err(e) => eprintln!("\n❌ Error: {e}"),
            }
        });
    }
}

// ── Scan ──────────────────────────────────────────────────────────────────────

async fn cmd_scan() -> Result<()> {
    println!("🔍 Scanning LAN for rust-air devices (5s)…\n");
    let devs = scan_once(5).await?;
    if devs.is_empty() {
        println!("  No devices found.");
    } else {
        for (name, addr) in &devs {
            println!("  ✓  {}  @ {}", name, addr);
        }
    }
    Ok(())
}

/// Scan for `secs` seconds and return (name, addr) pairs.
async fn scan_once(secs: u64) -> Result<Vec<(String, String)>> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let handle = discovery::browse_devices_sync(tx)?;
    let mut devs: Vec<(String, String)> = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(dev)) if !dev.addr.is_empty() => {
                let short = dev.name.split('.').next().unwrap_or(&dev.name).to_string();
                if !devs.iter().any(|(_, a)| a == &dev.addr) {
                    devs.push((short, dev.addr));
                }
            }
            _ => break,
        }
    }
    drop(handle); // shutdown daemon immediately
    Ok(devs)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn device_name() -> String {
    discovery::safe_device_name()
}

fn noop_progress(_: TransferEvent) {}
