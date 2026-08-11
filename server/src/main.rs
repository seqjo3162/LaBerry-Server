use std::{
    backtrace::Backtrace,
    fs::OpenOptions,
    io::Write,
    net::SocketAddr,
};

use laberry_server::ws::Hub;
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

fn load_env_file() {
    if dotenvy::dotenv().is_err() {
        let _ = dotenvy::from_filename("../.env");
    }

    repair_admin_secrets_from_dotenv_file();

    if std::env::var("LB_HOST").is_err() {
        if let Ok(v) = std::env::var("HOST") {
            std::env::set_var("LB_HOST", v);
        }
    }
    if std::env::var("LB_PORT").is_err() {
        if let Ok(v) = std::env::var("PORT") {
            std::env::set_var("LB_PORT", v);
        }
    }
    if std::env::var("LB_DB_PATH").is_err() {
        if let Ok(v) = std::env::var("DB_PATH") {
            std::env::set_var("LB_DB_PATH", v);
        }
    }
}

fn repair_admin_secrets_from_dotenv_file() {
    let hash_ok = std::env::var("LB_ADMIN_PASSWORD_HASH")
        .ok()
        .map(|v| admin_secret_looks_valid(&v))
        .unwrap_or(false);
    if hash_ok {
        return;
    }

    for path in [".env", "../.env"] {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Some(raw) = read_raw_dotenv_value(&text, "LB_ADMIN_PASSWORD_HASH") {
                if admin_secret_looks_valid(&raw) {
                    std::env::set_var("LB_ADMIN_PASSWORD_HASH", raw);
                    tracing::info!("[ADMIN] Repaired LB_ADMIN_PASSWORD_HASH from {path}");
                    return;
                }
            }
        }
    }
}

fn admin_secret_looks_valid(value: &str) -> bool {
    let v = value.trim().trim_matches('"').trim_matches('\'');
    v.starts_with("$argon2")
}

fn read_raw_dotenv_value(content: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || !line.starts_with(&prefix) {
            continue;
        }
        let mut v = line[prefix.len()..].trim().to_string();
        if v.len() >= 2 && ((v.starts_with('\'') && v.ends_with('\'')) || (v.starts_with('"') && v.ends_with('"'))) {
            v = v[1..v.len() - 1].to_string();
        }
        if !v.is_empty() {
            return Some(v);
        }
    }
    None
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install_panic_logger();
    load_env_file();

    let (tx, rx) = oneshot::channel::<()>();

    tracing::info!("🚀 LaBerry Server starting...");

    let host = std::env::var("LB_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port = std::env::var("LB_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(5001);
        
    let ip = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => ip,
        Err(e) => return Err(anyhow::anyhow!("Failed to parse IP address '{}': {}", host, e)),
    };
    
    let addr = SocketAddr::new(ip, port);
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
