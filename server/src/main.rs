use std::net::SocketAddr;
use tokio::{sync::oneshot, signal};
use laberry_server::ws::Hub;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // создаём канал завершения
    let (tx, rx) = oneshot::channel::<()>();

    println!("🚀 LaBerry Server starting...");
    let addr = SocketAddr::from(([0, 0, 0, 0], 5001));
    println!("📡 Server will listen on: {}", addr);

    let hub = Hub::new();

    // graceful shutdown через Ctrl+C
    tokio::spawn(async move {
        signal::ctrl_c().await.expect("failed to listen for Ctrl+C");
        println!("🛑 Received Ctrl+C, shutting down...");
        let _ = tx.send(());
    });

    println!("🔓 Starting server without TLS...");

    // Запускаем сервер БЕЗ TLS
    laberry_server::server::run_server(
        "./laberry.db",
        "12345678901234567890123456789012",
        addr,
        rx,
        hub,
    )
    .await
}