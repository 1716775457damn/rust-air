//! Minimal HTTP server for phone/browser file download + terminal QR code.

use anyhow::Result;
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderValue, StatusCode},
    response::Response,
    routing::get,
    Router,
};
use qrcode::{render::unicode, QrCode};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::net::TcpListener;

#[derive(Clone)]
struct QrState {
    path:      Arc<PathBuf>,
    /// ASCII-safe filename for the Content-Disposition header.
    safe_name: Arc<String>,
    file_size: u64,
}

/// Bind a random port, print a QR code pointing to the download URL, and serve
/// the file until the process exits (Ctrl-C).
pub async fn serve_and_print_qr(file: PathBuf) -> Result<()> {
    let file_size = tokio::fs::metadata(&file).await?.len();
    let raw_name  = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
    // Strip non-ASCII characters to keep the Content-Disposition header valid.
    let safe_name = raw_name.chars().filter(|c| c.is_ascii() && *c != '"').collect::<String>();

    let state = QrState {
        path:      Arc::new(file),
        safe_name: Arc::new(safe_name),
        file_size,
    };

    let app = Router::new().route("/get", get(download_handler)).with_state(state);
    let listener = TcpListener::bind("0.0.0.0:0").await?;
    let port = listener.local_addr()?.port();
    let url  = format!("http://{}:{port}/get", local_ip()?);

    print_qr(&url)?;
    println!("📱 Scan QR or open: {url}  (Ctrl-C to stop)\n");

    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

async fn download_handler(State(s): State<QrState>) -> Result<Response, StatusCode> {
    let f = tokio::fs::File::open(s.path.as_ref())
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    let disposition = format!("attachment; filename=\"{}\"", s.safe_name);

    Response::builder()
        .header(header::CONTENT_DISPOSITION, HeaderValue::from_str(&disposition)
            .unwrap_or_else(|_| HeaderValue::from_static("attachment")))
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, s.file_size)
        .body(Body::from_stream(tokio_util::io::ReaderStream::new(f)))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn print_qr(url: &str) -> Result<()> {
    let img = QrCode::new(url.as_bytes())?
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Dark)
        .light_color(unicode::Dense1x2::Light)
        .build();
    println!("\n{img}\n");
    Ok(())
}

/// Determine the local LAN IP — delegates to discovery to avoid duplication.
fn local_ip() -> Result<String> {
    crate::discovery::local_lan_ip().ok_or_else(|| anyhow::anyhow!("no LAN IP found"))
}
