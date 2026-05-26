use std::{
    backtrace::Backtrace,
    fs::OpenOptions,
    io::Write,
    net::SocketAddr,
};

use laberry_server::ws::Hub;
use tracing;
use tokio::{signal, sync::oneshot};

fn install_panic_logger() {
    std::panic::set_hook(Box::new(|info| {
        let bt = Backtrace::force_capture();
        let msg = format!("[PANIC] {}\nBacktrace:\n{}\n\n", info, bt);

        tracing::error!("{}", msg);

        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open("panic.log")
        {
            let _ = f.write_all(msg.as_bytes());
        }
    }));
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install_panic_logger();

    let (tx, rx) = oneshot::channel::<()>();

    tracing::info!("🚀 LaBerry Server starting...");

    let host = std::env::var("LB_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port = std::env::var("LB_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(5001);
    let addr = SocketAddr::from((host.parse::<std::net::IpAddr>()?, port));
    tracing::info!("📡 Server will listen on: {}", addr);

    let db_path = std::env::var("LB_DB_PATH").unwrap_or_else(|_| "./laberry.db".to_string());

    let secret = std::env::var("SECRET_KEY")
        .map_err(|_| anyhow::anyhow!("SECRET_KEY env var is required (>=32 bytes)"))?;

    let hub = Hub::new();

    tokio::spawn(async move {
        signal::ctrl_c()
            .await
            .expect("failed to listen for Ctrl+C");
        tracing::info!("🛑 Received Ctrl+C, shutting down...");
        let _ = tx.send(());
    });

    laberry_server::server::run_server(
        &db_path,
        &secret,
        addr,
        rx,
        hub,
    )
    .await
}
