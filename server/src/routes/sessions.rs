use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use sqlx::Row;

use crate::middleware::auth_guard::AuthUser;
use crate::server::AppState;

#[derive(Serialize)]
pub struct SessionView {
    pub id: i64,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub created_at: String,
    pub last_seen_at: String,
    pub revoked_at: Option<String>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_my))
        .route("/:session_id/revoke", post(revoke))
}

async fn list_my(State(st): State<AppState>, me: AuthUser) -> impl IntoResponse {
    let db = &st.db;

    let rows = sqlx::query(
        r#"
        SELECT id, user_agent, ip, created_at, last_seen_at, revoked_at
        FROM user_sessions
        WHERE user_id = ?
        ORDER BY COALESCE(revoked_at, '') ASC, last_seen_at DESC
        LIMIT 200
        "#,
    )
    .bind(me.id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let out = rows
        .into_iter()
        .map(|r| SessionView {
            id: r.get("id"),
            user_agent: r.try_get("user_agent").ok(),
            ip: r.try_get("ip").ok(),
            created_at: r.get("created_at"),
            last_seen_at: r.get("last_seen_at"),
            revoked_at: r.try_get("revoked_at").ok(),
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(out)).into_response()
}

async fn revoke(
    State(st): State<AppState>,
    me: AuthUser,
    Path(session_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    let owned = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM user_sessions WHERE id = ? AND user_id = ? LIMIT 1",
    )
    .bind(session_id)
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some();

    if !owned {
        return StatusCode::NOT_FOUND.into_response();
    }

    let now = crate::auth::now_iso();

    let _ = sqlx::query("UPDATE user_sessions SET revoked_at = ? WHERE id = ?")
        .bind(&now)
        .bind(session_id)
        .execute(db)
        .await;

    (StatusCode::OK, Json(serde_json::json!({"status":"ok"}))).into_response()
}
