// ======================================================
// 📡 LaBerry Server — main Axum server entry (Debug + Logging Build)
// ======================================================
use crate::{
    auth, db,
    ws::{Hub, UserId, VoiceChannelId},
};

use axum::{
    extract::{
        DefaultBodyLimit,
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{header, HeaderMap, StatusCode, Method, HeaderValue},
    response::IntoResponse,
    routing::get,
    Json,
    Router,
};

use serde::Deserialize;
use serde_json::json;
use sqlx::{Row, SqlitePool};
use dashmap::DashMap;
use std::{
    collections::{HashMap, HashSet},
    env,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
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
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
    trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer},
    set_header::SetResponseHeaderLayer,
    LatencyUnit,
};

// ======================================================
// 🧩 Application State
#[derive(Clone)]
pub struct AdminSession {
    pub expires_at: i64,
    pub csrf: String,
}

// ======================================================
#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub hub: Arc<Hub>,
    pub connected_ws: Arc<AtomicUsize>,
    pub friends: HashMap<UserId, HashSet<UserId>>,
    pub voice_states: HashMap<UserId, VoiceChannelId>,
    pub admin_sessions: Arc<DashMap<String, AdminSession>>,
}

// ======================================================
// 🚀 Server entry point WITHOUT TLS (оригинальный)
// ======================================================

pub async fn run_server(
    db_path: &str,
    secret: &str,
    addr: SocketAddr,
    _shutdown_rx: oneshot::Receiver<()>,
    hub: Hub,
) -> anyhow::Result<()> {
    println!("[SERVER] Starting...");
    println!("[SERVER] Listening on {}", addr);
    match env::current_dir() {
        Ok(dir) => println!("[SERVER] CWD = {:?}", dir),
        Err(err) => eprintln!("[SERVER] CWD error: {}", err),
    }
    
    if secret.as_bytes().len() < 32 {
        anyhow::bail!("SECRET_KEY must be at least 32 bytes");
    }

    let db_url = format!("sqlite:{}?mode=rwc", db_path);
    println!("[SERVER] Connecting DB: {}", db_url);
    std::env::set_var("DATABASE_URL", &db_url);
    std::env::set_var("SECRET_KEY", secret);

    println!("[DB] Connecting...");
    let db = SqlitePool::connect(&db_url).await?;
    println!("[DB] ✅ Connected");

    println!("[DB] Running init...");
    db::init(&db).await?;
    println!("[DB] ✅ Init complete");

    println!("[DB] Bootstrapping global server...");
    db::bootstrap::ensure_global_server(&db).await?;
    println!("[DB] ✅ Bootstrap complete");

    let state = AppState {
        db,
        hub: Arc::new(hub),
        connected_ws: Arc::new(AtomicUsize::new(0)),
        friends: HashMap::new(),
        voice_states: HashMap::new(),
        admin_sessions: Arc::new(DashMap::new()),
    };

    // One shutdown signal for both servers.
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
            // Run once on startup, then hourly.
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

    // ------------------------------
    // Main (public) server
    // ------------------------------
    println!("[SERVER] Building main router...");
    let app = build_router(state.clone());
    println!("[SERVER] Main router built ✅");

    println!("[SERVER] Binding main TCP listener...");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("[SERVER] ✅ Main bound to {}", addr);

    let shutdown_main = shutdown.clone();
    let mut main_handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                shutdown_main.notified().await;
            })
            .await
            .map_err(anyhow::Error::from)
    });

    // ------------------------------
    // Admin server (local-only)
    // ------------------------------
    let admin_enabled = env_bool("LB_ENABLE_ADMIN_PANEL", false)
        || std::env::var("LB_ADMIN_PASSWORD").ok().is_some()
        || std::env::var("LB_ADMIN_PASSWORD_HASH").ok().is_some();
    println!("[ADMIN] env LB_ENABLE_ADMIN_PANEL={:?} LB_ADMIN_HOST={:?} LB_ADMIN_PORT={:?} -> enabled={}",
        std::env::var("LB_ENABLE_ADMIN_PANEL").ok(),
        std::env::var("LB_ADMIN_HOST").ok(),
        std::env::var("LB_ADMIN_PORT").ok(),
        admin_enabled
    );
    let mut admin_handle = None;

    if admin_enabled {
        let admin_addr = admin_bind_addr()?;

        println!("[ADMIN] Building admin router...");
        let admin_app = build_admin_router(state);
        println!("[ADMIN] Router built ✅");

        println!("[ADMIN] Binding admin TCP listener...");
        let admin_listener = tokio::net::TcpListener::bind(admin_addr).await?;
        println!("[ADMIN] ✅ Admin bound to {}", admin_addr);

        let shutdown_admin = shutdown.clone();
        admin_handle = Some(tokio::spawn(async move {
            axum::serve(admin_listener, admin_app)
                .with_graceful_shutdown(async move {
                    shutdown_admin.notified().await;
                })
                .await
                .map_err(anyhow::Error::from)
        }));
    }

    // Wait until one of servers stops, then stop the other.
    if let Some(mut ah) = admin_handle {
        tokio::select! {
            res = &mut main_handle => {
                shutdown.notify_waiters();
                let r = res??;
                let _ = ah.await;
                Ok(r)
            }
            res = &mut ah => {
                shutdown.notify_waiters();
                let r = res??;
                let _ = main_handle.await;
                Ok(r)
            }
        }
    } else {
        let r = main_handle.await??;
        Ok(r)
    }
}

// ======================================================
// 🧭 Router builder
// ======================================================
pub fn build_router(state: AppState) -> Router {
    println!("[ROUTER] Building routes...");

    let static_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("server")
        .join("static");

    println!("[ROUTER] Static dir: {:?}", static_dir);

    let router = Router::new()
        .route("/", get(crate::routes::pages::index))
        .route("/app", get(crate::routes::pages::app))
        .route("/start", get(crate::routes::pages::start))
        .route("/admin", get(crate::routes::pages::admin_hint))
        .route("/admin/", get(crate::routes::pages::admin_hint))
        .route("/health", get(|| async { "OK" }))
        .route("/ws", get(ws_main))
        .route("/ws/health", get(ws_health))
        .route("/verify", get(crate::routes::auth::verify_token))
        .route("/api/system/status", get(system_status))
        .nest("/api/auth", crate::routes::auth::router())
        .nest("/api/users", crate::routes::users::router())
        .nest("/api/friends", crate::routes::friends::router())
        .nest("/api/servers", crate::routes::servers::router())
        .nest("/api/chats", crate::routes::chats::router())
        .nest("/api/dms", crate::routes::dms::router())
        .nest("/api/presence", crate::routes::presence::router())
        .nest("/api/sessions", crate::routes::sessions::router())
        .nest("/api/messages", crate::routes::messages::global_router())
        .nest("/api/files", crate::routes::files::router())
        .nest("/api/profile-files", crate::routes::profile_files::router())
        .nest("/api/embeds", crate::routes::embeds::router())
        .nest("/api/rtc", crate::routes::rtc::router())
        .nest(
            "/files",
            Router::new()
                .route("/:file_id/raw", get(crate::routes::files::get_file_raw))
                .route("/:file_id", get(crate::routes::files::get_file))
                .with_state(state.clone()),
        )
        .nest_service(
            "/static",
            ServeDir::new(static_dir.clone())
                .fallback(ServeFile::new(static_dir.join("index.html"))),
        )
        ;

    // Admin panel is NOT exposed on the public server.
    // It is served by a dedicated local-only listener (see run_server).

    let router = router.with_state(state)
        // Protect server from huge request bodies (uploads, etc.)
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024))

// Basic security headers (can be customized via env)
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
        &env::var("LB_CSP").unwrap_or_else(|_| "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self' ws: wss:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'".to_string())
    ).unwrap_or_else(|_| HeaderValue::from_static("default-src 'self'")),
))

        .layer({
    // CORS: deny-by-default. Configure explicit origins via CORS_ALLOWED_ORIGINS env (comma-separated) or "*".
    let allowed = env::var("CORS_ALLOWED_ORIGINS").unwrap_or_default();
    let mut cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

    if !allowed.trim().is_empty() {
        let a = allowed.trim();
        if a == "*" {
            cors = cors.allow_origin(tower_http::cors::Any);
        } else {
            let mut origins: Vec<axum::http::HeaderValue> = Vec::new();
            for o in a.split(',') {
                let o = o.trim();
                if o.is_empty() { continue; }
                if let Ok(v) = o.parse::<axum::http::HeaderValue>() {
                    origins.push(v);
                }
            }
            if !origins.is_empty() {
                cors = cors.allow_origin(origins);
            }
        }
    }
    cors
})
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().include_headers(false))
                .on_response(
                    DefaultOnResponse::new()
                        .include_headers(false)
                        .latency_unit(LatencyUnit::Millis),
                ),
        )
        .layer(CatchPanicLayer::new());

    println!("[ROUTER] ✅ Routes ready");
    router
}

// ======================================================
// 🔐 Admin server (local-only)
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
        // convenience: open http://127.0.0.1:<LB_ADMIN_PORT>/ and land in /admin/
        .route("/", get(|| async { Redirect::to("/admin/") }))
        .route("/admin", get(|| async { Redirect::to("/admin/") }))
        .nest("/admin/", crate::routes::admin_panel::router())
        .nest_service(
            "/static",
            ServeDir::new(static_dir.clone())
                .fallback(ServeFile::new(static_dir.join("index.html"))),
        )
        .with_state(state)
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        // Security headers
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

    println!("[WS_HEALTH] connected");
    ws.on_upgrade(move |socket| async move {
        println!("[WS_HEALTH] upgrade success, entering loop");
        health_loop(socket, st).await;
        println!("[WS_HEALTH] loop ended");
    })
}

async fn health_loop(mut socket: WebSocket, st: AppState) {
    let mut sys = System::new_all();
    let disks = Disks::new_with_refreshed_list();
    let mut ticker = interval(Duration::from_secs(2));
    let start = Instant::now();
    loop {
        ticker.tick().await;
        sys.refresh_cpu();
        sys.refresh_memory();

        let cpu_usage = sys.global_cpu_info().cpu_usage();
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
        if let Err(err) = socket.send(Message::Text(payload.to_string())).await {
            println!("[WS_HEALTH] disconnected (err={})", err);
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
            // Typical: "Bearer <jwt>"
            if let Some(bearer) = v.trim().strip_prefix("Bearer ") {
                if !bearer.is_empty() {
                    token = Some(bearer.to_string());
                }
            }
        }
    }

    let mut t = token?;

    // Some clients may accidentally pass "Bearer <jwt>" via query.
    let t_trim = t.trim();
    if let Some(rest) = t_trim.strip_prefix("Bearer ") {
        t = rest.trim().to_string();
    } else if let Some(rest) = t_trim.strip_prefix("bearer ") {
        t = rest.trim().to_string();
    } else {
        t = t_trim.to_string();
    }

    // Strip accidental surrounding quotes.
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        t = t.trim_matches('"').to_string();
    }

    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

async fn ws_main(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    State(st): State<AppState>,
) -> impl IntoResponse {
    // Token can be provided in query/header (legacy) OR via first WS message (preferred).
    let pre_token = extract_token(&headers, &q);

    let db = st.db.clone();
    let hub = Arc::clone(&st.hub);
    let connected = st.connected_ws.clone();

    ws.on_upgrade(move |mut socket| async move {
        let prev = connected.fetch_add(1, Ordering::Relaxed) + 1;
        if ws_debug_enabled() {
            println!("[WS] CONNECT (total={})", prev);
        }

        // Authenticate
        let auth_token = if let Some(t) = pre_token {
            Some(t)
        } else {
            // wait for {type:\"auth\", token:\"...\"}
            match tokio::time::timeout(Duration::from_secs(5), socket.recv()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                        if v.get("type").and_then(|x| x.as_str()) == Some("auth") {
                            v.get("token").and_then(|x| x.as_str()).map(|s| s.to_string())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                _ => None,
            }
        };

        let Some(token) = auth_token else {
            let _ = socket.send(Message::Text(json!({"type":"error","code":"unauthorized"}).to_string())).await;
            let _ = socket.send(Message::Close(None)).await;
            let after = connected.fetch_sub(1, Ordering::Relaxed) - 1;
            if ws_debug_enabled() {
                println!("[WS] DISCONNECT (unauthorized) (total={})", after);
            }
            return;
        };

        let (username, token_version) = match auth::decode_username(&token) {
            Ok(v) => v,
            Err(_) => {
                let _ = socket.send(Message::Text(json!({"type":"error","code":"invalid_token"}).to_string())).await;
                let _ = socket.send(Message::Close(None)).await;
                let after = connected.fetch_sub(1, Ordering::Relaxed) - 1;
                if ws_debug_enabled() {
                    println!("[WS] DISCONNECT (invalid token) (total={})", after);
                }
                return;
            }
        };

        let row = sqlx::query(
            r#"
            SELECT id, token_version, is_banned
            FROM users
            WHERE username = ?
            LIMIT 1
            "#,
        )
        .bind(&username)
        .fetch_optional(&db)
        .await;

        let Ok(Some(row)) = row else {
            let _ = socket.send(Message::Text(json!({"type":"error","code":"user_not_found"}).to_string())).await;
            let _ = socket.send(Message::Close(None)).await;
            let after = connected.fetch_sub(1, Ordering::Relaxed) - 1;
            if ws_debug_enabled() {
                println!("[WS] DISCONNECT (user not found) (total={})", after);
            }
            return;
        };

        let is_banned: i64 = row.get("is_banned");
        if is_banned != 0 {
            let _ = socket.send(Message::Text(json!({"type":"error","code":"banned"}).to_string())).await;
            let _ = socket.send(Message::Close(None)).await;
            let after = connected.fetch_sub(1, Ordering::Relaxed) - 1;
            if ws_debug_enabled() {
                println!("[WS] DISCONNECT (banned) (total={})", after);
            }
            return;
        }

        let db_version: i64 = row.get("token_version");
        if db_version != token_version {
            let _ = socket.send(Message::Text(json!({"type":"error","code":"token_invalidated"}).to_string())).await;
            let _ = socket.send(Message::Close(None)).await;
            let after = connected.fetch_sub(1, Ordering::Relaxed) - 1;
            if ws_debug_enabled() {
                println!("[WS] DISCONNECT (invalidated) (total={})", after);
            }
            return;
        }

        let user_id: i64 = row.get("id");

        if ws_debug_enabled() {
            println!("[WS] ✅ AUTH OK user_id={} username={}", user_id, username);
        }

        crate::ws::chat::handle_single_ws(socket, db.clone(), hub.clone(), user_id, username).await;

        let after = connected.fetch_sub(1, Ordering::Relaxed) - 1;
        if ws_debug_enabled() {
            println!("[WS] DISCONNECT user={} (total={})", user_id, after);
        }
    })
}
