use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{auth, server::AppState};
use crate::middleware::auth_guard::AuthUser;

#[derive(Deserialize)]
pub struct CreateChatBody {
    pub name: Option<String>,
    pub server_id: Option<i64>,
    pub is_private: Option<bool>,
    pub participant_ids: Option<Vec<i64>>,
}

#[derive(Serialize)]
pub struct ChatRow {
    pub id: i64,
    pub name: Option<String>,
    pub server_id: Option<i64>,
    pub is_private: i64,
    pub created_at: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create).get(list_my))
        .route("/:chat_id", get(get_one))
        .route("/:chat_id/join", post(join)) // join теперь безопасный
}

async fn create(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<CreateChatBody>,
) -> impl IntoResponse {
    let db = &st.db;
    let created_at = auth::now_iso();
    let is_private = body.is_private.unwrap_or(false);

    // server chat: creator MUST be server member
    if let Some(server_id) = body.server_id {
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
    }

    let res = sqlx::query(
        r#"
        INSERT INTO chats(name, server_id, is_private, created_at)
        VALUES(?, ?, ?, ?)
        "#,
    )
    .bind(&body.name)
    .bind(body.server_id)
    .bind(if is_private { 1 } else { 0 })
    .bind(&created_at)
    .execute(db)
    .await;

    let Ok(r) = res else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    let chat_id = r.last_insert_rowid();

    // creator is always participant
    let _ = sqlx::query(
        r#"INSERT INTO chat_participants(chat_id, user_id)
           VALUES(?, ?)"#,
    )
    .bind(chat_id)
    .bind(me.id)
    .execute(db)
    .await;

    // private chat: explicitly listed participants only
    if is_private {
        if let Some(ids) = body.participant_ids {
            for uid in ids {
                // skip creator duplication
                if uid == me.id {
                    continue;
                }

                // ensure user exists & not banned
                let ok = sqlx::query_scalar::<_, i64>(
                    "SELECT 1 FROM users WHERE id = ? AND is_banned = 0 LIMIT 1",
                )
                .bind(uid)
                .fetch_optional(db)
                .await
                .ok()
                .flatten()
                .is_some();

                if ok {
                    let _ = sqlx::query(
                        r#"INSERT OR IGNORE INTO chat_participants(chat_id, user_id)
                           VALUES(?, ?)"#,
                    )
                    .bind(chat_id)
                    .bind(uid)
                    .execute(db)
                    .await;
                }
            }
        }
    }

    (StatusCode::OK, Json(serde_json::json!({ "id": chat_id }))).into_response()
}

async fn join(
    State(st): State<AppState>,
    me: AuthUser,
    Path(chat_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    let row = sqlx::query(
        "SELECT is_private, server_id FROM chats WHERE id = ? LIMIT 1",
    )
    .bind(chat_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(r) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let is_private: i64 = r.get("is_private");
    let server_id: Option<i64> = r.get("server_id");

    // ❌ private chats cannot be joined
    if is_private == 1 {
        return StatusCode::FORBIDDEN.into_response();
    }

    // server chat: must be server member
    if let Some(sid) = server_id {
        let member = sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM server_members WHERE server_id = ? AND user_id = ? LIMIT 1",
        )
        .bind(sid)
        .bind(me.id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .is_some();

        if !member {
            return StatusCode::FORBIDDEN.into_response();
        }
    }

    let _ = sqlx::query(
        r#"INSERT OR IGNORE INTO chat_participants(chat_id, user_id)
           VALUES(?, ?)"#,
    )
    .bind(chat_id)
    .bind(me.id)
    .execute(db)
    .await;

    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
}

async fn get_one(
    State(st): State<AppState>,
    me: AuthUser,
    Path(chat_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    let member = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM chat_participants WHERE chat_id = ? AND user_id = ? LIMIT 1",
    )
    .bind(chat_id)
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some();

    if !member {
        return StatusCode::FORBIDDEN.into_response();
    }

    let row = sqlx::query(
        r#"
        SELECT id, name, server_id, is_private, created_at
        FROM chats
        WHERE id = ?
        LIMIT 1
        "#,
    )
    .bind(chat_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(r) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let chat = ChatRow {
        id: r.get("id"),
        name: r.get("name"),
        server_id: r.get("server_id"),
        is_private: r.get("is_private"),
        created_at: r.get("created_at"),
    };

    (StatusCode::OK, Json(chat)).into_response()
}

async fn list_my(
    State(st): State<AppState>,
    me: AuthUser,
) -> impl IntoResponse {
    let db = &st.db;

    let rows = sqlx::query(
        r#"
        SELECT c.id, c.name, c.server_id, c.is_private, c.created_at
        FROM chats c
        JOIN chat_participants p ON p.chat_id = c.id
        WHERE p.user_id = ?
        ORDER BY c.id DESC
        "#,
    )
    .bind(me.id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let out = rows
        .into_iter()
        .map(|r| ChatRow {
            id: r.get("id"),
            name: r.get("name"),
            server_id: r.get("server_id"),
            is_private: r.get("is_private"),
            created_at: r.get("created_at"),
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(out)).into_response()
}
