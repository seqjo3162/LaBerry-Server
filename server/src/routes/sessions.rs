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
    pub created_at: String,
    pub last_seen_at: String,
    pub revoked_at: Option<String>,
    pub is_current: bool,
    pub is_active: bool,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_my))
        .route("/{session_id}/revoke", post(revoke))
}

async fn list_my(State(st): State<AppState>, me: AuthUser) -> impl IntoResponse {
    let db = &st.db;

    let rows = sqlx::query(
        r#"
        SELECT id, token_hash, user_agent, created_at, last_seen_at, revoked_at
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
        .map(|r| {
            let revoked_at: Option<String> = r.try_get("revoked_at").ok();
            let token_hash: String = r.get("token_hash");
            SessionView {
                id: r.get("id"),
                user_agent: r.try_get("user_agent").ok(),
                created_at: r.try_get("created_at").unwrap_or_else(|_| String::new()),
                last_seen_at: r.try_get("last_seen_at").unwrap_or_else(|_| String::new()),
                is_current: token_hash == me.token_hash,
                is_active: revoked_at.is_none(),
                revoked_at,
            }
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

    let session = sqlx::query(
        "SELECT user_agent, ip FROM user_sessions WHERE id = ? AND user_id = ? LIMIT 1",
    )
    .bind(session_id)
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(session) = session else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let now = crate::auth::now_iso();
    let user_agent: Option<String> = session.try_get("user_agent").ok();
    let ip: Option<String> = session.try_get("ip").ok();

    let _ = sqlx::query("UPDATE user_sessions SET revoked_at = ? WHERE id = ?")
        .bind(&now)
        .bind(session_id)
        .execute(db)
        .await;

    let _ = sqlx::query(
        r#"
        UPDATE refresh_sessions
        SET revoked_at = ?
        WHERE user_id = ?
          AND revoked_at IS NULL
          AND ((user_agent = ?) OR (user_agent IS NULL AND ? IS NULL))
          AND ((ip = ?) OR (ip IS NULL AND ? IS NULL))
        "#,
    )
    .bind(&now)
    .bind(me.id)
    .bind(user_agent.clone())
    .bind(user_agent)
    .bind(ip.clone())
    .bind(ip)
    .execute(db)
    .await;

    (StatusCode::OK, Json(serde_json::json!({"status":"ok"}))).into_response()
}
