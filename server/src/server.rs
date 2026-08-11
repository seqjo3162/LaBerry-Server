// ======================================================
// 📡 LaBerry Server — main Axum server entry (Debug + Logging Build)
// ======================================================
use crate::{
    auth, db,
    ws::{Hub, UserId, VoiceChannelId},
    middleware::geo_guard::GeoGuardState,
};

use axum::{
    extract::{
        DefaultBodyLimit,
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{header, HeaderMap, StatusCode, Method, HeaderValue},
    response::IntoResponse,
    routing::{any, get},
    Json,
    Router,
};

use serde::Deserialize;
use serde_json::json;
use sqlx::{Row, PgPool};
use dashmap::DashMap;
use chrono;
use tracing;
use std::{
    collections::{HashSet},
    env,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{
        Arc, atomic::{AtomicUsize, Ordering}
    },
};
use sysinfo::{Disks, System};
use tokio::{
    sync::oneshot,
    sync::Notify,
    time::{interval, Duration, Instant},
};
use tower_http::{
    catch_panic::CatchPanicLayer,
    cors::{CorsLayer, AllowOrigin},
    services::{ServeDir, ServeFile},
    trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer},
    set_header::SetResponseHeaderLayer,
    LatencyUnit,
};

// ======================================================
// 🧩 Application State
// ======================================================

#[derive(Clone)]
pub struct AdminSession {
    pub expires_at: i64,
    pub csrf: String,
}

// server.rs

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub hub: Arc<Hub>,
    pub connected_ws: Arc<AtomicUsize>,
    pub friends: Arc<DashMap<UserId, HashSet<UserId>>>,
    pub voice_states: Arc<DashMap<UserId, VoiceChannelId>>,
    pub admin_sessions: Arc<DashMap<String, AdminSession>>,
    pub geo_guard: GeoGuardState,
    pub trusted_proxies: Vec<IpAddr>,
}

// ======================================================
// 🚀 Server entry point WITHOUT TLS (оригинальный, исправленный)
// ======================================================
pub async fn run_server(
    db_path: &str,
    secret: &str,
    addr: SocketAddr,
    _shutdown_rx: oneshot::Receiver<()>,
    hub: Hub,
) -> anyhow::Result<()> {
    tracing::info!("[SERVER] Starting...");
    tracing::info!("[SERVER] Listening on {}", addr);
    match env::current_dir() {
        Ok(dir) => tracing::info!("[SERVER] CWD = {:?}", dir),
        Err(err) => tracing::error!("[SERVER] CWD error: {}", err),
    }
    
    if secret.len() < 32 {
        anyhow::bail!("SECRET_KEY must be at least 32 bytes");
    }

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| db_path.to_string());
    tracing::info!("[SERVER] Connecting DB: {}", db_url);
    std::env::set_var("DATABASE_URL", &db_url);
    std::env::set_var("SECRET_KEY", secret);

    tracing::info!("[DB] Connecting...");
    use sqlx::postgres::PgPoolOptions;
    use std::time::Duration;

    let db = PgPoolOptions::new()
        .max_connections(32)
        .min_connections(8)
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Duration::from_secs(300))
        .connect(&db_url)
        .await?;
    tracing::info!("[DB] ✅ Connected (pool: 32 max, 8 min)");

    tracing::info!("[DB] Running init...");
    db::init(&db).await?;
    tracing::info!("[DB] ✅ Init complete");

    tracing::info!("[DB] Bootstrapping global server...");
    db::bootstrap::ensure_global_server(&db).await?;
    tracing::info!("[DB] ✅ Bootstrap complete");

    let geo_guard = GeoGuardState::from_custom_file("assets/custom_blocked_cidr")?;
    // Упрощено: без Arc
    let trusted_proxies = vec![
        std::net::IpAddr::V4(std::net::Ipv4Addr::new(0, 0, 0, 0)),
        std::net::IpAddr::V6(std::net::Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0)),
    ];

    let state = AppState {
        db,
        hub: Arc::new(hub),
        connected_ws: Arc::new(AtomicUsize::new(0)),
        friends: Arc::new(DashMap::new()),
        voice_states: Arc::new(DashMap::new()),
        admin_sessions: Arc::new(DashMap::new()),
        geo_guard,
        trusted_proxies,
    };

    let shutdown = Arc::new(Notify::new());
    {
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _ = _shutdown_rx.await;
            shutdown.notify_waiters();
        });
    }

    // ------------------------------
    // Background cleanup: temporary chat files
    // ------------------------------
    {
        let cleanup_state = state.clone();
        let cleanup_shutdown = shutdown.clone();
        tokio::spawn(async move {
            crate::routes::files::cleanup_expired_files(&cleanup_state).await;
            let mut tick = interval(Duration::from_secs(60 * 60));
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        crate::routes::files::cleanup_expired_files(&cleanup_state).await;
                    }
                    _ = cleanup_shutdown.notified() => {
                        break;
                    }
                }
            }
        });
    }

    {
        let cleanup_db = state.db.clone();
        let cleanup_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let mut tick = interval(Duration::from_secs(60 * 60));
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        match crate::middleware::rate_limit::cleanup_expired_logs(&cleanup_db).await {
                            Ok(rows) => if rows > 0 { tracing::info!("[CLEANUP] Deleted {} rate limit logs", rows); },
                            Err(e) => tracing::error!("[ERROR] Failed to cleanup rate limit logs: {}", e),
                        }
                        let now = chrono::Utc::now();
                        match sqlx::query(r#"DELETE FROM csrf_tokens WHERE expires_at < $1"#)
                            .bind(now)
                            .execute(&cleanup_db)
                            .await {
                            Ok(result) => {
                                let rows = result.rows_affected();
                                if rows > 0 { tracing::info!("[CLEANUP] Deleted {} CSRF tokens", rows); }
                            },
                            Err(e) => tracing::error!("[ERROR] Failed to cleanup CSRF tokens: {}", e),
                        }
                        crate::middleware::rate_limit::cleanup_expired_buckets();
                    }
                    _ = cleanup_shutdown.notified() => break,
                }
            }
        });
    }

    // ------------------------------
    // Main (public) server
    // ------------------------------
    let tls_cert_path = env::var("LB_TLS_CERT_PATH").ok();
    let tls_key_path = env::var("LB_TLS_KEY_PATH").ok();
    let has_tls = tls_cert_path.is_some() && tls_key_path.is_some();
    if has_tls {
        tracing::info!("[SERVER] 🔐 TLS/HTTPS enabled");
    } else {
        tracing::info!("[SERVER] ⚠️  TLS/HTTPS disabled");
    }

    let app = build_router(state.clone());
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let shutdown_main = shutdown.clone();

    let mut main_handle = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(async move { shutdown_main.notified().await })
            .await
            .map_err(|e| anyhow::anyhow!("Main server error: {}", e))
    });

    // ------------------------------
    // Admin server (local-only)
    // ------------------------------
    let admin_enabled = env_bool("LB_ENABLE_ADMIN_PANEL", false)
        || std::env::var("LB_ADMIN_PASSWORD").ok().is_some()
        || std::env::var("LB_ADMIN_PASSWORD_HASH").ok().is_some();

    let mut admin_handle = None;
    if admin_enabled {
        let admin_addr = admin_bind_addr()?;
        let pwd_ok = crate::routes::admin_panel::admin_password_is_configured();
        tracing::info!(
            "[ADMIN] Enabled (password configured: {pwd_ok}). Main-app gateway: /admin/* -> admin port"
        );
        tracing::info!("[ADMIN] Building admin router...");
        let admin_app = build_admin_router(state.clone());
        let admin_listener = tokio::net::TcpListener::bind(admin_addr).await?;
        tracing::info!(
            "[ADMIN] Listening on {} (login: {}/admin/login)",
            admin_addr,
            crate::routes::pages::admin_panel_base_url()
        );
        let shutdown_admin = shutdown.clone();

        let ah = tokio::spawn(async move {
            axum::serve(admin_listener, admin_app.into_make_service_with_connect_info::<SocketAddr>())
                .with_graceful_shutdown(async move { shutdown_admin.notified().await })
                .await
                .map_err(|e| anyhow::anyhow!("Admin server error: {}", e))
        });
        admin_handle = Some(ah);
    }

    if let Some(mut ah) = admin_handle {
        tokio::select! {
            res = &mut main_handle => {
                shutdown.notify_waiters();
                res??;
                let _ = ah.await;
                Ok(())
            }
            res = &mut ah => {
                shutdown.notify_waiters();
                res??;
                let _ = main_handle.await;
                Ok(())
            }
        }
    } else {
        main_handle.await??;
        Ok(())
    }
}

// ======================================================
// 🧭 Router builder (с исправленным CORS)
// ======================================================
pub fn build_router(state: AppState) -> Router {
    let st = state.clone();
    tracing::info!("[ROUTER] Building routes...");

    let static_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("server")
        .join("static");

    tracing::info!("[ROUTER] Static dir: {:?}", static_dir);

    let state_for_middleware = state.clone();

    let admin_gateway = Router::new()
        .route("/", get(crate::routes::pages::admin_hint))
        .route("/{*rest}", any(crate::routes::pages::admin_redirect_fallback));

    let main_routes = Router::new()
        .route("/", get(crate::routes::pages::index))
        .route("/login", get(crate::routes::pages::login))
        .route("/app", get(crate::routes::pages::app))
        .route("/start", get(crate::routes::pages::start))
        .route("/cookie-agreement", get(crate::routes::pages::cookie_agreement))
        .route("/license-agreement", get(crate::routes::pages::license_agreement))
        .nest("/admin", admin_gateway)
        .route("/health", get(|| async { "OK" }))
        .route("/verify", get(crate::routes::auth::verify_token))
        .route("/api/system/status", get(system_status))
        .nest("/api/auth", crate::routes::auth::router().with_state(st.clone()))
        .nest("/api/users", crate::routes::users::router().with_state(st.clone()))
        .nest("/api/friends", crate::routes::friends::router().with_state(st.clone()))
        .nest("/api/servers", crate::routes::servers::router().with_state(st.clone()))
        .nest("/api/chats", crate::routes::chats::router().with_state(st.clone()))
        .nest("/api/dms", crate::routes::dms::router().with_state(st.clone()))
        .nest("/api/presence", crate::routes::presence::router().with_state(st.clone()))
        .nest("/api/sessions", crate::routes::sessions::router().with_state(st.clone()))
        .nest("/api/e2ee", crate::routes::e2ee::router().with_state(st.clone()))
        .nest("/api/messages", crate::routes::messages::global_router().with_state(st.clone()))
        .nest("/api/files", crate::routes::files::router().with_state(st.clone()))
        .nest("/api/gifs", crate::routes::gifs::router().with_state(st.clone()))
        .nest("/api/downloads", crate::routes::downloads::router().with_state(st.clone()))
        .nest("/api/profile-files", crate::routes::profile_files::router().with_state(st.clone()))
        .nest("/api/embeds", crate::routes::embeds::router().with_state(st.clone()))
        .nest("/api/rtc", crate::routes::rtc::router().with_state(st.clone()))
        .nest("/api/2fa", crate::routes::twofa::router().with_state(st.clone()))
        .nest(
            "/files",
            Router::new()
                .route("/{file_id}/raw", get(crate::routes::files::get_file_raw)) 
                .route("/{file_id}", get(crate::routes::files::get_file))
                .with_state(st.clone()),
        )
        .nest("/api/payments", crate::routes::payments::router().with_state(st.clone()))
        .route_service("/favicon.ico", ServeFile::new(static_dir.join("assets/favicons/favicon.ico")))
        .route_service("/apple-touch-icon.png", ServeFile::new(static_dir.join("assets/favicons/apple-touch-icon.png")))
        .nest_service(
            "/static",
            ServeDir::new(static_dir.clone())
                .fallback(ServeFile::new(static_dir.join("index.html"))),
        )
        // Все middleware применяются ТОЛЬКО к main_routes
        .layer(axum::middleware::from_fn_with_state(state_for_middleware.clone(), crate::middleware::host_guard::host_guard))
        .layer(axum::middleware::from_fn_with_state(state_for_middleware, crate::middleware::geo_guard::geo_guard))
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024))
        .layer(SetResponseHeaderLayer::if_not_present(axum::http::header::HeaderName::from_static("x-content-type-options"), HeaderValue::from_static("nosniff")))
        .layer(SetResponseHeaderLayer::if_not_present(axum::http::header::HeaderName::from_static("x-frame-options"), HeaderValue::from_static("DENY")))
        .layer(SetResponseHeaderLayer::if_not_present(axum::http::header::HeaderName::from_static("x-xss-protection"), HeaderValue::from_static("1; mode=block")))
        .layer(SetResponseHeaderLayer::if_not_present(axum::http::header::HeaderName::from_static("referrer-policy"), HeaderValue::from_static("strict-origin-when-cross-origin")))
        .layer(SetResponseHeaderLayer::if_not_present(axum::http::header::HeaderName::from_static("permissions-policy"), HeaderValue::from_static("geolocation=(), microphone=(self), camera=(), payment=()")))
        .layer(SetResponseHeaderLayer::if_not_present(axum::http::header::HeaderName::from_static("cross-origin-opener-policy"), HeaderValue::from_static("same-origin")))
        .layer(SetResponseHeaderLayer::if_not_present(axum::http::header::HeaderName::from_static("cross-origin-resource-policy"), HeaderValue::from_static("same-origin")))
        .layer({
            let tls_cert = env::var("LB_TLS_CERT_PATH").ok();
            let tls_key = env::var("LB_TLS_KEY_PATH").ok();
            if tls_cert.is_some() && tls_key.is_some() {
                SetResponseHeaderLayer::if_not_present(axum::http::header::HeaderName::from_static("strict-transport-security"), HeaderValue::from_static("max-age=31536000; includeSubDomains; preload"))
            } else {
                SetResponseHeaderLayer::if_not_present(axum::http::header::HeaderName::from_static("x-no-op"), HeaderValue::from_static("1"))
            }
        })
        .layer(SetResponseHeaderLayer::if_not_present(axum::http::header::HeaderName::from_static("cache-control"), HeaderValue::from_static("no-cache, no-store, must-revalidate, private")))
        .layer(SetResponseHeaderLayer::if_not_present(axum::http::header::HeaderName::from_static("pragma"), HeaderValue::from_static("no-cache")))
        .layer(SetResponseHeaderLayer::if_not_present(
            axum::http::header::HeaderName::from_static("content-security-policy"),
            HeaderValue::from_str(
                &env::var("LB_CSP").unwrap_or_else(|_| 
                    "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https://i.ytimg.com https://*.ytimg.com https://ru.pinterest.com https://*.pinterest.com https://player.vimeo.com https://rutube.ru; connect-src 'self' wss: ws: https://i.ytimg.com https://*.ytimg.com https://ru.pinterest.com https://*.pinterest.com https://player.vimeo.com https://rutube.ru; frame-src 'self' https://www.youtube.com https://player.vimeo.com https://rutube.ru; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'".to_string())
            ).unwrap_or_else(|_| HeaderValue::from_static("default-src 'self'")),
        ))
        .layer({
            let allowed = env::var("CORS_ALLOWED_ORIGINS").unwrap_or_default();
            let mut cors = CorsLayer::new()
                .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS])
                .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE, header::HeaderName::from_static("x-csrf-token")])
                .allow_credentials(true);

            if !allowed.trim().is_empty() {
                let a = allowed.trim();
                if a == "*" {
                    cors = cors.allow_origin(tower_http::cors::Any);
                } else {
                    let origins: Vec<HeaderValue> = a.split(',').filter_map(|o| o.trim().parse().ok()).collect();
                    if !origins.is_empty() {
                        cors = cors.allow_origin(AllowOrigin::list(origins));
                    }
                }
            }
            cors
        })
        .layer(axum::middleware::from_fn(crate::middleware::csrf_guard::csrf_guard))
        .layer(TraceLayer::new_for_http().make_span_with(DefaultMakeSpan::new().include_headers(false)).on_response(DefaultOnResponse::new().include_headers(false).latency_unit(LatencyUnit::Millis)))
        .layer(CatchPanicLayer::new());

    let ws_routes = Router::new()
        .route("/ws", get(ws_main))
        .route("/ws/health", get(ws_health));

    Router::new()
        .merge(ws_routes)
        .merge(main_routes)
        .with_state(state)
}

// ======================================================
// 🔐 Admin server (local-only) — без изменений
// ======================================================

fn env_bool(key: &str, default: bool) -> bool {
    env::var(key)
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(default)
}

fn admin_bind_addr() -> anyhow::Result<SocketAddr> {
    let host = env::var("LB_ADMIN_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = env::var("LB_ADMIN_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(5002);

    let ip: std::net::IpAddr = host.parse()?;
    if !ip.is_loopback() && !env_bool("LB_ADMIN_ALLOW_NON_LOOPBACK", false) {
        anyhow::bail!(
            "Refusing to bind admin panel to non-loopback address {}. Use 127.0.0.1/::1 or set LB_ADMIN_ALLOW_NON_LOOPBACK=1 explicitly.",
            ip
        );
    }
    Ok(SocketAddr::from((ip, port)))
}

pub fn build_admin_router(state: AppState) -> Router {
    use axum::response::Redirect;

    let static_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("server")
        .join("static");

    Router::new()
        .route("/", get(|| async { Redirect::to("/admin/login") }))
        .nest("/admin", crate::routes::admin_panel::router().with_state(state.clone()))
        .nest_service(
            "/static",
            ServeDir::new(static_dir.clone())
                .fallback(ServeFile::new(static_dir.join("index.html"))),
        )
        .layer(DefaultBodyLimit::max(512 * 1024 * 1024))
        .layer(SetResponseHeaderLayer::if_not_present(
            axum::http::header::HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            axum::http::header::HeaderName::from_static("x-frame-options"),
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            axum::http::header::HeaderName::from_static("referrer-policy"),
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            axum::http::header::HeaderName::from_static("permissions-policy"),
            HeaderValue::from_static("geolocation=(), microphone=(self), camera=()"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            axum::http::header::HeaderName::from_static("cross-origin-opener-policy"),
            HeaderValue::from_static("same-origin"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            axum::http::header::HeaderName::from_static("cross-origin-resource-policy"),
            HeaderValue::from_static("same-origin"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            axum::http::header::HeaderName::from_static("content-security-policy"),
            HeaderValue::from_str(
                &env::var("LB_ADMIN_CSP").unwrap_or_else(|_| {
                    "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'".to_string()
                }),
            )
            .unwrap_or_else(|_| HeaderValue::from_static("default-src 'self'")),
        ))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().include_headers(false))
                .on_response(
                    DefaultOnResponse::new()
                        .include_headers(false)
                        .latency_unit(LatencyUnit::Millis),
                ),
        )
        .layer(CatchPanicLayer::new())
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    fn test_state() -> AppState {
        let db = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent_pool_for_tests")
            .expect("test db");
        let hub = Hub::new();
        let geo_guard = GeoGuardState::from_custom_file("assets/custom_blocked_cidr")
            .expect("test requires geo file");

        AppState {
            db,
            hub: Arc::new(hub),
            connected_ws: Arc::new(AtomicUsize::new(0)),
            friends: Arc::new(DashMap::new()),
            voice_states: Arc::new(DashMap::new()),
            admin_sessions: Arc::new(DashMap::new()),
            geo_guard,
            trusted_proxies: vec![
                IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ],
        }
    }

    #[tokio::test]
    async fn public_router_builds_without_panicking() {
        let _router = build_router(test_state());
    }

    #[tokio::test]
    async fn admin_router_builds_without_panicking() {
        let _router = build_admin_router(test_state());
    }
}

async fn system_status() -> impl IntoResponse {
    let maintenance = std::env::var("LB_MAINTENANCE_MODE")
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(false);

    let message = std::env::var("LB_MAINTENANCE_TEXT")
        .unwrap_or_else(|_| "На сервере идут технические работы. Возможны перебои или временная недоступность.".to_string());

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "maintenance": maintenance,
            "message": message
        })),
    )
}

// ======================================================
// 💓 WS Health monitor
// ======================================================
async fn ws_health(ws: WebSocketUpgrade, State(st): State<AppState>) -> impl IntoResponse {
    let enabled = std::env::var("LB_ENABLE_WS_HEALTH")
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(false);

    if !enabled {
        return StatusCode::NOT_FOUND.into_response();
    }

    tracing::info!("[WS_HEALTH] connected");
    ws.on_upgrade(move |socket| async move {
        tracing::info!("[WS_HEALTH] upgrade success, entering loop");
        health_loop(socket, st).await;
        tracing::info!("[WS_HEALTH] loop ended");
    })
}

async fn health_loop(mut socket: WebSocket, st: AppState) {
    let mut sys = System::new_all();
    let disks = Disks::new_with_refreshed_list();
    let mut ticker = interval(Duration::from_secs(2));
    let start = Instant::now();
    loop {
        ticker.tick().await;
        sys.refresh_cpu_all();
        sys.refresh_memory();

        let cpu_usage = sys.global_cpu_usage();
        let mem_used = sys.used_memory() / 1024 / 1024;
        let mem_total = sys.total_memory() / 1024 / 1024;

        let (disk_used, disk_total) = disks
            .iter()
            .next()
            .map(|d| {
                let total = d.total_space() / 1024 / 1024 / 1024;
                let used = (d.total_space() - d.available_space()) / 1024 / 1024 / 1024;
                (used, total)
            })
            .unwrap_or((0, 0));

        let payload = json!({
            "type": "health",
            "uptime_sec": start.elapsed().as_secs(),
            "ws_connected": st.connected_ws.load(Ordering::Relaxed),
            "cpu": { "usage_percent": cpu_usage, "cores": sys.cpus().len() },
            "memory": { "used_mb": mem_used, "total_mb": mem_total },
            "disk": { "used_gb": disk_used, "total_gb": disk_total }
        });
        if let Err(err) = socket.send(Message::Text(payload.to_string().into())).await {
            tracing::info!("[WS_HEALTH] disconnected (err={})", err);
            break;
        }
    }
}

// ======================================================
// 💬 Main chat WebSocket (Stable)
// ======================================================
#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

fn ws_debug_enabled() -> bool {
    std::env::var("LB_DEBUG_WS")
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(false)
}

fn extract_token(headers: &HeaderMap, q: &TokenQuery) -> Option<String> {
    let mut token: Option<String> = None;

    if let Some(t) = &q.token {
        token = Some(t.clone());
    } else if let Some(value) = headers.get(header::AUTHORIZATION) {
        if let Ok(v) = value.to_str() {
            if let Some(bearer) = v.trim().strip_prefix("Bearer ") {
                if !bearer.is_empty() {
                    token = Some(bearer.to_string());
                }
            }
        }
    }

    let mut t = token?;

    let t_trim = t.trim();
    if let Some(rest) = t_trim.strip_prefix("Bearer ") {
        t = rest.trim().to_string();
    } else if let Some(rest) = t_trim.strip_prefix("bearer ") {
        t = rest.trim().to_string();
    } else {
        t = t_trim.to_string();
    }

    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        t = t.trim_matches('"').to_string();
    }

    if t.is_empty() { None } else { Some(t) }
}

async fn ws_main(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    State(st): State<AppState>,
) -> impl IntoResponse {
    eprintln!("[WS_RAW] Upgrade request received");
    let pre_token = extract_token(&headers, &q);
    let db = st.db.clone();
    let hub = Arc::clone(&st.hub);
    let connected = Arc::clone(&st.connected_ws);

    ws.on_upgrade(move |mut socket| async move {
        let prev = connected.fetch_add(1, Ordering::Relaxed) + 1;
        if ws_debug_enabled() {
            tracing::info!("[WS] CONNECT (total={})", prev);
        }

        let auth_token = if let Some(t) = pre_token {
            Some(t)
        } else {
            match tokio::time::timeout(std::time::Duration::from_secs(5), socket.recv()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                        if v.get("type").and_then(|x| x.as_str()) == Some("auth") {
                            v.get("token").and_then(|x| x.as_str()).map(|s| s.to_string())
                        } else { None }
                    } else { None }
                }
                Ok(Some(Ok(Message::Binary(_)))) |
                Ok(Some(Ok(Message::Ping(_)))) |
                Ok(Some(Ok(Message::Pong(_)))) |
                Ok(Some(Ok(Message::Close(_)))) |
                Ok(Some(Err(_))) |
                Ok(None) |
                Err(_) => None,
            }
        };

        let Some(token) = auth_token else {
            let _ = socket.send(Message::Text(json!({"type":"error","code":"unauthorized"}).to_string().into())).await;
            let _ = socket.send(Message::Close(None)).await;
            let after = connected.fetch_sub(1, Ordering::Relaxed) - 1;
            if ws_debug_enabled() { tracing::info!("[WS] DISCONNECT (unauthorized) (total={})", after); }
            return;
        };

        let (username, token_version) = match auth::decode_username(&token) {
            Ok(v) => v,
            Err(_) => {
                let _ = socket.send(Message::Text(json!({"type":"error","code":"invalid_token"}).to_string().into())).await;
                let _ = socket.send(Message::Close(None)).await;
                let after = connected.fetch_sub(1, Ordering::Relaxed) - 1;
                if ws_debug_enabled() { tracing::info!("[WS] DISCONNECT (invalid token) (total={})", after); }
                return;
            }
        };

        let row = sqlx::query(
            r#"
            SELECT id, token_version, is_banned
            FROM users
            WHERE username = $1
            LIMIT 1
            "#,
        )
        .bind(&username)
        .fetch_optional(&db)
        .await;

        let Ok(Some(row)) = row else {
            let _ = socket.send(Message::Text(json!({"type":"error","code":"user_not_found"}).to_string().into())).await;
            let _ = socket.send(Message::Close(None)).await;
            let after = connected.fetch_sub(1, Ordering::Relaxed) - 1;
            if ws_debug_enabled() { tracing::info!("[WS] DISCONNECT (user not found) (total={})", after); }
            return;
        };

        let is_banned: bool = row.get("is_banned");
        if is_banned {
            let _ = socket.send(Message::Text(json!({"type":"error","code":"banned"}).to_string().into())).await;
            let _ = socket.send(Message::Close(None)).await;
            let after = connected.fetch_sub(1, Ordering::Relaxed) - 1;
            if ws_debug_enabled() { tracing::info!("[WS] DISCONNECT (banned) (total={})", after); }
            return;
        }

        let db_version: i64 = row.get("token_version");
        if db_version != token_version {
            let _ = socket.send(Message::Text(json!({"type":"error","code":"token_invalidated"}).to_string().into())).await;
            let _ = socket.send(Message::Close(None)).await;
            let after = connected.fetch_sub(1, Ordering::Relaxed) - 1;
            if ws_debug_enabled() { tracing::info!("[WS] DISCONNECT (invalidated) (total={})", after); }
            return;
        }

        let user_id: i64 = row.get("id");

        if ws_debug_enabled() {
            tracing::info!("[WS] ✅ AUTH OK user_id={} username={}", user_id, username);
        }

        crate::ws::chat::handle_single_ws(socket, db.clone(), hub.clone(), user_id, username).await;

        let after = connected.fetch_sub(1, Ordering::Relaxed) - 1;
        if ws_debug_enabled() {
            tracing::info!("[WS] DISCONNECT user={} (total={})", user_id, after);
        }
    })
}