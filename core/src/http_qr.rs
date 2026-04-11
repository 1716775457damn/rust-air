use anyhow::Result;
use axum::{
    body::Body, extract::State, http::{header, StatusCode},
    response::Response, routing::get, Router,
};
use qrcode::{render::unicode, QrCode};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::net::TcpListener;

#[derive(Clone)]
struct QrState { path: Arc<PathBuf>, name: Arc<String> }

pub async fn serve_and_print_qr(file: PathBuf) -> Result<()> {
    let name = file.file_name().unwrap_or_default().to_string_lossy().to_string();
    let state = QrState { path: Arc::new(file), name: Arc::new(name) };
    let app = Router::new().route("/get", get(handler)).with_state(state);
    let listener = TcpListener::bind("0.0.0.0:0").await?;
    let port = listener.local_addr()?.port();
    let url = format!("http://{}:{port}/get", local_ip()?);
    print_qr(&url)?;
    println!("📱 Scan QR or open: {url}  (Ctrl-C to stop)\n");
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

async fn handler(State(s): State<QrState>) -> Result<Response, StatusCode> {
    let f = tokio::fs::File::open(s.path.as_ref()).await.map_err(|_| StatusCode::NOT_FOUND)?;
    Ok(Response::builder()
        .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{}\"", s.name))
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(Body::from_stream(tokio_util::io::ReaderStream::new(f)))
        .unwrap())
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

fn local_ip() -> Result<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0")?;
    s.connect("8.8.8.8:80")?;
    Ok(s.local_addr()?.ip().to_string())
}
