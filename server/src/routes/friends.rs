use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use crate::{auth, server::AppState};

#[derive(Deserialize)]
pub struct FriendRequestCreate {
    pub receiver_id: i64,
}

#[derive(Serialize)]
pub struct FriendRequestRow {
    pub id: i64,
    pub sender_id: i64,
    pub receiver_id: i64,
    pub status: String,
    pub created_at: String,
}

#[derive(Serialize)]
pub struct FriendshipRow {
    pub id: i64,
    pub user_id: i64,
    pub friend_id: i64,
    pub created_at: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/request", post(request_friend))
        .route("/requests/incoming", get(incoming))
        .route("/requests/outgoing", get(outgoing))
        .route("/accept/:request_id", post(accept))
        .route("/decline/:request_id", post(decline))
        .route("/:friend_id", delete(remove_friend))
        .route("/", get(list_friends))
}

/* ---------- helpers ---------- */

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::to_string)
}

async fn current_user_id(st: &AppState, token: &str) -> Result<i64, StatusCode> {
    let (username, token_version) =
        auth::decode_username(token).map_err(|_| StatusCode::UNAUTHORIZED)?;

    let row = sqlx::query(
        r#"SELECT id, token_version, is_banned
           FROM users WHERE username = ? LIMIT 1"#,
    )
    .bind(username)
    .fetch_optional(&st.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::UNAUTHORIZED)?;

    if row.get::<i64, _>("is_banned") != 0 {
        return Err(StatusCode::FORBIDDEN);
    }
    if row.get::<i64, _>("token_version") != token_version {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(row.get("id"))
}

/* ---------- handlers ---------- */

async fn request_friend(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<FriendRequestCreate>,
) -> impl IntoResponse {
    let Some(tok) = bearer_token(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let sender_id = match current_user_id(&st, &tok).await {
        Ok(id) => id,
        Err(sc) => return sc.into_response(),
    };

    if sender_id == body.receiver_id {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "detail": "Cannot add yourself" })),
        )
            .into_response();
    }

    let already = sqlx::query_scalar::<_, i64>(
        r#"SELECT 1 FROM friendships
           WHERE (user_id = ? AND friend_id = ?)
              OR (user_id = ? AND friend_id = ?)
           LIMIT 1"#,
    )
    .bind(sender_id)
    .bind(body.receiver_id)
    .bind(body.receiver_id)
    .bind(sender_id)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten()
    .is_some();

    if already {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "detail": "Already friends" })),
        )
            .into_response();
    }

    let created_at = auth::now_iso();

    if sqlx::query(
        r#"INSERT INTO friend_requests(sender_id, receiver_id, status, created_at)
           VALUES (?, ?, 'pending', ?)"#,
    )
    .bind(sender_id)
    .bind(body.receiver_id)
    .bind(created_at)
    .execute(&st.db)
    .await
    .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
}

async fn incoming(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(tok) = bearer_token(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let me = match current_user_id(&st, &tok).await {
        Ok(id) => id,
        Err(sc) => return sc.into_response(),
    };

    let rows = sqlx::query(
        r#"SELECT id, sender_id, receiver_id, status, created_at
           FROM friend_requests
           WHERE receiver_id = ? AND status = 'pending'
           ORDER BY id DESC"#,
    )
    .bind(me)
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();

    let out = rows
        .into_iter()
        .map(|r| FriendRequestRow {
            id: r.get("id"),
            sender_id: r.get("sender_id"),
            receiver_id: r.get("receiver_id"),
            status: r.get("status"),
            created_at: r.get("created_at"),
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(out)).into_response()
}

async fn outgoing(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(tok) = bearer_token(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let me = match current_user_id(&st, &tok).await {
        Ok(id) => id,
        Err(sc) => return sc.into_response(),
    };

    let rows = sqlx::query(
        r#"SELECT id, sender_id, receiver_id, status, created_at
           FROM friend_requests
           WHERE sender_id = ? AND status = 'pending'
           ORDER BY id DESC"#,
    )
    .bind(me)
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();

    let out = rows
        .into_iter()
        .map(|r| FriendRequestRow {
            id: r.get("id"),
            sender_id: r.get("sender_id"),
            receiver_id: r.get("receiver_id"),
            status: r.get("status"),
            created_at: r.get("created_at"),
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(out)).into_response()
}

async fn accept(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(request_id): Path<i64>,
) -> impl IntoResponse {
    let Some(tok) = bearer_token(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let me = match current_user_id(&st, &tok).await {
        Ok(id) => id,
        Err(sc) => return sc.into_response(),
    };

    let rq = sqlx::query(
        r#"SELECT sender_id, receiver_id, status
           FROM friend_requests WHERE id = ? LIMIT 1"#,
    )
    .bind(request_id)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten();

    let Some(rq) = rq else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if rq.get::<i64, _>("receiver_id") != me || rq.get::<String, _>("status") != "pending" {
        return StatusCode::FORBIDDEN.into_response();
    }

    let created_at = auth::now_iso();
    let sender = rq.get::<i64, _>("sender_id");
    let receiver = rq.get::<i64, _>("receiver_id");

    let _ = sqlx::query("UPDATE friend_requests SET status = 'accepted' WHERE id = ?")
        .bind(request_id)
        .execute(&st.db)
        .await;

    let _ = sqlx::query(
        r#"INSERT OR IGNORE INTO friendships(user_id, friend_id, created_at)
           VALUES (?, ?, ?)"#,
    )
    .bind(sender)
    .bind(receiver)
    .bind(&created_at)
    .execute(&st.db)
    .await;

    let _ = sqlx::query(
        r#"INSERT OR IGNORE INTO friendships(user_id, friend_id, created_at)
           VALUES (?, ?, ?)"#,
    )
    .bind(receiver)
    .bind(sender)
    .bind(&created_at)
    .execute(&st.db)
    .await;

    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
}

async fn decline(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(request_id): Path<i64>,
) -> impl IntoResponse {
    let Some(tok) = bearer_token(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let me = match current_user_id(&st, &tok).await {
        Ok(id) => id,
        Err(sc) => return sc.into_response(),
    };

    let rq = sqlx::query(
        r#"SELECT receiver_id FROM friend_requests WHERE id = ?"#,
    )
    .bind(request_id)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten();

    if rq.map(|r| r.get::<i64, _>("receiver_id")) != Some(me) {
        return StatusCode::FORBIDDEN.into_response();
    }

    let _ = sqlx::query("UPDATE friend_requests SET status = 'declined' WHERE id = ?")
        .bind(request_id)
        .execute(&st.db)
        .await;

    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
}

async fn remove_friend(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(friend_id): Path<i64>,
) -> impl IntoResponse {
    let Some(tok) = bearer_token(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let me = match current_user_id(&st, &tok).await {
        Ok(id) => id,
        Err(sc) => return sc.into_response(),
    };

    let _ = sqlx::query(
        r#"DELETE FROM friendships
           WHERE (user_id = ? AND friend_id = ?)
              OR (user_id = ? AND friend_id = ?)"#,
    )
    .bind(me)
    .bind(friend_id)
    .bind(friend_id)
    .bind(me)
    .execute(&st.db)
    .await;

    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
}

async fn list_friends(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(tok) = bearer_token(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let me = match current_user_id(&st, &tok).await {
        Ok(id) => id,
        Err(sc) => return sc.into_response(),
    };

    let rows = sqlx::query(
        r#"SELECT id, user_id, friend_id, created_at
           FROM friendships WHERE user_id = ?
           ORDER BY id DESC"#,
    )
    .bind(me)
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();

    let out = rows
        .into_iter()
        .map(|r| FriendshipRow {
            id: r.get("id"),
            user_id: r.get("user_id"),
            friend_id: r.get("friend_id"),
            created_at: r.get("created_at"),
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(out)).into_response()
}
