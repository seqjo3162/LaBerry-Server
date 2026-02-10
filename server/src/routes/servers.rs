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
pub struct CreateServerBody {
    pub name: String,
}

#[derive(Serialize)]
pub struct ServerRow {
    pub id: i64,
    pub name: String,
    pub owner_id: i64,
    pub created_at: String,
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
        .route("/", post(create).get(list))
        .route("/:server_id/join", post(join))
        .route("/:server_id/chats", get(list_chats))
        .nest(
            "/:server_id/chats/:chat_id/messages",
            crate::routes::messages::router(),
        )
}

async fn create(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<CreateServerBody>,
) -> impl IntoResponse {
    let db = &st.db;
    let created_at = auth::now_iso();

    let res = sqlx::query(
        "INSERT INTO servers(name, owner_id, created_at) VALUES(?, ?, ?)",
    )
    .bind(&body.name)
    .bind(me.id)
    .bind(&created_at)
    .execute(db)
    .await;

    let Ok(r) = res else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    let server_id = r.last_insert_rowid();

    let _ = sqlx::query(
        "INSERT OR IGNORE INTO server_members(server_id, user_id, role) VALUES(?, ?, 'admin')",
    )
    .bind(server_id)
    .bind(me.id)
    .execute(db)
    .await;

    (StatusCode::OK, Json(serde_json::json!({ "id": server_id }))).into_response()
}

async fn join(
    State(st): State<AppState>,
    me: AuthUser,
    Path(server_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    let exists = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM servers WHERE id = ? LIMIT 1",
    )
    .bind(server_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some();

    if !exists {
        return StatusCode::NOT_FOUND.into_response();
    }

    if sqlx::query(
        "INSERT OR IGNORE INTO server_members(server_id, user_id) VALUES(?, ?)",
    )
    .bind(server_id)
    .bind(me.id)
    .execute(db)
    .await
    .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
}

async fn list(
    State(st): State<AppState>,
    me: AuthUser,
) -> impl IntoResponse {
    let db = &st.db;

    let rows = sqlx::query(
        r#"
        SELECT s.id, s.name, s.owner_id, s.created_at
        FROM servers s
        JOIN server_members m ON m.server_id = s.id
        WHERE m.user_id = ?
        ORDER BY s.id DESC
        "#,
    )
    .bind(me.id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let servers = rows
        .into_iter()
        .map(|r| ServerRow {
            id: r.get("id"),
            name: r.get("name"),
            owner_id: r.get("owner_id"),
            created_at: r.get("created_at"),
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(servers)).into_response()
}

async fn list_chats(
    State(st): State<AppState>,
    me: AuthUser,
    Path(server_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

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
        SELECT id, name, server_id, is_private, created_at
        FROM chats
        WHERE server_id = ?
        ORDER BY id DESC
        "#,
    )
    .bind(server_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let chats = rows
        .into_iter()
        .map(|r| ChatRow {
            id: r.get("id"),
            name: r.get("name"),
            server_id: r.get("server_id"),
            is_private: r.get("is_private"),
            created_at: r.get("created_at"),
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(chats)).into_response()
}
