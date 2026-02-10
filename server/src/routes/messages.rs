use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::auth;
use crate::middleware::auth_guard::AuthUser;
use crate::server::AppState;

#[derive(Serialize)]
pub struct MessageRow {
    pub id: i64,
    pub chat_id: i64,
    pub sender_id: i64,
    pub sender_username: String,
    pub content: String,
    pub timestamp: String,
}

#[derive(Deserialize)]
pub struct SendMessageBody {
    pub content: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list).post(send))
}

async fn list(
    State(st): State<AppState>,
    me: AuthUser,
    Path((server_id, chat_id)): Path<(i64, i64)>,
) -> impl IntoResponse {
    let db = &st.db;

    // проверка: пользователь состоит в сервере
    let member = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM server_members WHERE server_id = ? AND user_id = ? LIMIT 1",
    )
    .bind(server_id)
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some();

    if !member {
        return StatusCode::FORBIDDEN.into_response();
    }

    let rows = sqlx::query(
        r#"
        SELECT m.id,
               m.chat_id,
               m.sender_id,
               u.username AS sender_username,
               m.content,
               m.timestamp
        FROM messages m
        JOIN users u ON u.id = m.sender_id
        WHERE m.chat_id = ?
        ORDER BY m.id ASC
        LIMIT 200
        "#,
    )
    .bind(chat_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let messages = rows
        .into_iter()
        .map(|r| MessageRow {
            id: r.get("id"),
            chat_id: r.get("chat_id"),
            sender_id: r.get("sender_id"),
            sender_username: r.get("sender_username"),
            content: r.get("content"),
            timestamp: r.get("timestamp"),
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(messages)).into_response()
}

async fn send(
    State(st): State<AppState>,
    me: AuthUser,
    Path((server_id, chat_id)): Path<(i64, i64)>,
    Json(body): Json<SendMessageBody>,
) -> impl IntoResponse {
    let db = &st.db;

    let content = body.content.trim();
    if content.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    // проверка: пользователь состоит в сервере
    let member = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM server_members WHERE server_id = ? AND user_id = ? LIMIT 1",
    )
    .bind(server_id)
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some();

    if !member {
        return StatusCode::FORBIDDEN.into_response();
    }

    let timestamp = auth::now_iso();

    let res = sqlx::query(
        r#"
        INSERT INTO messages (chat_id, sender_id, content, timestamp)
        VALUES (?, ?, ?, ?)
        "#,
    )
    .bind(chat_id)
    .bind(me.id)
    .bind(content)
    .bind(&timestamp)
    .execute(db)
    .await;

    let Ok(r) = res else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    let message_id = r.last_insert_rowid();

    (
        StatusCode::OK,
        Json(serde_json::json!({ "id": message_id })),
    )
        .into_response()
}
