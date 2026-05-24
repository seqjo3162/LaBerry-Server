use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Serialize;
use sqlx::Row;
use std::sync::atomic::Ordering;

use crate::server::AppState;
use crate::middleware::auth_guard::AuthUser;

#[derive(Serialize)]
pub struct OnlineUser {
    pub user_id: i64,
}

#[derive(Serialize)]
pub struct OnlineStats {
    pub online_count: usize,
    pub connection_count: usize,
    pub updated_at: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/online", get(online))
        .route("/stats", get(stats))
}

async fn stats(State(st): State<AppState>) -> impl IntoResponse {
    let online_count = st.hub.presence.len();
    let connection_count = st.connected_ws.load(Ordering::Relaxed);

    (StatusCode::OK, Json(OnlineStats {
        online_count,
        connection_count,
        updated_at: chrono::Utc::now().to_rfc3339(),
    })).into_response()
}

async fn online(
    State(st): State<AppState>,
    _me: AuthUser,
) -> impl IntoResponse {
    let rows = sqlx::query(
        "SELECT user_id FROM user_presence WHERE is_online = 1 ORDER BY user_id",
    )
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();

    let users: Vec<OnlineUser> = rows
        .into_iter()
        .map(|row| OnlineUser {
            user_id: row.get::<i64, _>("user_id"),
        })
        .collect();

    (StatusCode::OK, Json(users)).into_response()
}
