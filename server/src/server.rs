// ======================================================
// 📡 LaBerry Server — main Axum server entry (Debug + Logging Build)
// ======================================================
use crate::{
    auth, db,
    ws::{Hub, UserId, VoiceChannelId},
};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;
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
    time::{interval, Duration, Instant},
};
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
    trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer},
    LatencyUnit,
};

// ======================================================
// 🧩 Application State
// ======================================================
#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub hub: Arc<Hub>,
    pub connected_ws: Arc<AtomicUsize>,
    pub friends: HashMap<UserId, HashSet<UserId>>,
    pub voice_states: HashMap<UserId, VoiceChannelId>,
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
    println!("[SERVER] CWD = {:?}", env::current_dir().unwrap());
    
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
    };

    println!("[SERVER] Building router...");
    let app = build_router(state);
    println!("[SERVER] Router built ✅");

    println!("[SERVER] Binding TCP listener...");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("[SERVER] ✅ Bound to {}", addr);

    println!("[SERVER] 🚀 Serving app...");
    axum::serve(listener, app).await?;
    println!("[SERVER] ❌ Server stopped (serve returned)");

    Ok(())
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
        .route("/health", get(|| async { "OK" }))
        .route("/ws", get(ws_main))
        .route("/ws/health", get(ws_health))
        .route("/verify", get(crate::routes::auth::verify_token))
        .nest("/api/auth", crate::routes::auth::router())
        .nest("/api/users", crate::routes::users::router())
        .nest("/api/friends", crate::routes::friends::router())
        .nest("/api/servers", crate::routes::servers::router())
        .nest("/api/chats", crate::routes::chats::router())
        .nest("/api/presence", crate::routes::presence::router())
        .nest("/api/files", crate::routes::files::router())
        .nest(
            "/files",
            Router::new()
                .route("/:file_id", get(crate::routes::files::get_file))
                .with_state(state.clone()),
        )
        .nest_service(
            "/static",
            ServeDir::new(static_dir.clone())
                .fallback(ServeFile::new(static_dir.join("index.html"))),
        )
        .with_state(state)
        .layer(CorsLayer::permissive())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().include_headers(false))
                .on_response(
                    DefaultOnResponse::new()
                        .include_headers(false)
                        .latency_unit(LatencyUnit::Millis),
                ),
        );

    println!("[ROUTER] ✅ Routes ready");
    router
}

// ======================================================
// 💓 WS Health monitor
// ======================================================
async fn ws_health(ws: WebSocketUpgrade, State(st): State<AppState>) -> impl IntoResponse {
    println!("[WS_HEALTH] connected");
    ws.on_upgrade(move |socket| async move {
        println!("[WS_HEALTH] upgrade success, entering loop");
        health_loop(socket, st).await;
        println!("[WS_HEALTH] loop ended");
    })
}

async fn health_loop(mut socket: WebSocket, st: AppState) {
    println!("[WS_HEALTH] initializing system monitor...");
    let mut sys = System::new_all();
    let disks = Disks::new_with_refreshed_list();
    let mut ticker = interval(Duration::from_secs(2));
    let start = Instant::now();
    println!("[WS_HEALTH] loop start");

    loop {
        ticker.tick().await;
        println!("[WS_HEALTH] tick");

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

        println!("[WS_HEALTH] sending payload...");
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

fn extract_token(headers: &HeaderMap, q: &TokenQuery) -> Option<String> {
    println!("[WS] Extracting token...");
    if let Some(t) = &q.token {
        println!("[WS] token from query");
        return Some(t.clone());
    }

    if let Some(value) = headers.get(header::AUTHORIZATION) {
        if let Ok(v) = value.to_str() {
            if let Some(bearer) = v.trim().strip_prefix("Bearer ") {
                if !bearer.is_empty() {
                    println!("[WS] token from header");
                    return Some(bearer.to_string());
                }
            }
        }
    }

    println!("[WS] no token found");
    None
}

async fn ws_main(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    State(st): State<AppState>,
) -> impl IntoResponse {
    println!("[WS] upgrade request");

    let Some(token) = extract_token(&headers, &q) else {
        println!("[WS] ❌ Unauthorized: no token");
        return StatusCode::UNAUTHORIZED.into_response();
    };

    println!("[WS] token extracted, decoding...");
    let (username, user_id) = match auth::decode_username(&token) {
        Ok(v) => {
            println!("[WS] ✅ decoded user_id={}", v.1);
            v
        }
        Err(err) => {
            eprintln!("[WS] ❌ Invalid token: {}", err);
            return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
        }
    };

    println!("[WS] ✅ AUTH OK user_id={} username={}", user_id, username);
    println!("[WS] 🚀 Upgrading...");

    let db = st.db.clone();
    let hub = Arc::clone(&st.hub);
    let connected = st.connected_ws.clone();

    ws.on_upgrade(move |socket| async move {
        let prev = connected.fetch_add(1, Ordering::Relaxed) + 1;
        println!("[WS] CONNECT user={} (total={})", user_id, prev);

        let db_c = db.clone();
        let hub_c = Arc::clone(&hub);
        let username_c = username.clone();
        let connected_c = connected.clone();

        println!("[WS] spawning handler...");
        
        tokio::spawn(async move {
            println!("[WS] 🧠 handler start");
            
            crate::ws::chat::handle_single_ws(socket, db_c, hub_c, user_id, username_c).await;
            
            println!("[WS] 🧠 handler end");
            
            let after = connected_c.fetch_sub(1, Ordering::Relaxed) - 1;
            println!("[WS] DISCONNECT user={} (total={})", user_id, after);
        });
    })
}