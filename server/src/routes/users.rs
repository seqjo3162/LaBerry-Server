use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::server::AppState;
use crate::middleware::auth_guard::AuthUser;

#[derive(Serialize)]
pub struct UserPublic {
    pub id: i64,
    pub username: String,
    pub email: Option<String>,
    pub public_encryption_key: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateMeBody {
    pub email: Option<String>,
    pub public_encryption_key: Option<String>,
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub query: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me", get(me).put(update_me))
        .route("/", get(list_users))
        .route("/search", get(search))
        .route("/:id", get(get_by_id))
}

async fn me(
    State(st): State<AppState>,
    me: AuthUser,
) -> impl IntoResponse {
    let db = &st.db;

    let row = sqlx::query(
        r#"SELECT id, username, email, public_encryption_key
           FROM users WHERE id = ? LIMIT 1"#,
    )
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(r) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let u = UserPublic {
        id: r.get("id"),
        username: r.get("username"),
        email: r.get("email"),
        public_encryption_key: r.get("public_encryption_key"),
    };

    (StatusCode::OK, Json(u)).into_response()
}

async fn update_me(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<UpdateMeBody>,
) -> impl IntoResponse {
    let db = &st.db;

    if sqlx::query(
        r#"UPDATE users
           SET email = ?, public_encryption_key = ?
           WHERE id = ?"#,
    )
    .bind(body.email)
    .bind(body.public_encryption_key)
    .bind(me.id)
    .execute(db)
    .await
    .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
}

async fn list_users(
    State(st): State<AppState>,
) -> impl IntoResponse {
    let db = &st.db;

    tracing::info!("list_users: start");

    let rows = sqlx::query(
        r#"SELECT id, username, email, public_encryption_key
           FROM users
           ORDER BY id DESC
           LIMIT 200"#,
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();

    tracing::info!("list_users: after db");
    
    let users: Vec<UserPublic> = rows
        .into_iter()
        .map(|r| UserPublic {
            id: r.get("id"),
            username: r.get("username"),
            email: r.get("email"),
            public_encryption_key: r.get("public_encryption_key"),
        })
        .collect();

    (StatusCode::OK, Json(users)).into_response()
}

async fn search(
    State(st): State<AppState>,
    Query(q): Query<SearchQuery>,
) -> impl IntoResponse {
    let db = &st.db;
    let pat = format!("%{}%", q.query);

    let rows = sqlx::query(
        r#"SELECT id, username, email, public_encryption_key
           FROM users
           WHERE username LIKE ?
           ORDER BY id DESC
           LIMIT 50"#,
    )
    .bind(pat)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let users: Vec<UserPublic> = rows
        .into_iter()
        .map(|r| UserPublic {
            id: r.get("id"),
            username: r.get("username"),
            email: r.get("email"),
            public_encryption_key: r.get("public_encryption_key"),
        })
        .collect();

    (StatusCode::OK, Json(users)).into_response()
}

async fn get_by_id(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    let row = sqlx::query(
        r#"SELECT id, username, email, public_encryption_key
           FROM users WHERE id = ? LIMIT 1"#,
    )
    .bind(id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(r) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let u = UserPublic {
        id: r.get("id"),
        username: r.get("username"),
        email: r.get("email"),
        public_encryption_key: r.get("public_encryption_key"),
    };

    (StatusCode::OK, Json(u)).into_response()
}
