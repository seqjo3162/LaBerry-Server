use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
};
use crate::{auth, server::AppState};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row, PgPool};

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
    pub is_favorite: bool,
}

#[derive(Serialize)]
pub struct FriendshipRow {
    pub id: i64,
    pub user_id: i64,
    pub friend_id: i64,
    pub created_at: String,
}

#[derive(Serialize)]
pub struct FriendView {
    pub id: i64,
    pub username: String,
    pub is_online: bool,
    pub status: String,
    pub created_at: String,
    pub is_favorite: bool,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/request", post(request_friend))
        .route("/requests/incoming", get(incoming))
        .route("/requests/outgoing", get(outgoing))
        .route("/accept/{request_id}", post(accept))
        .route("/decline/{request_id}", post(decline))
        .route("/cancel/{request_id}", post(cancel))
        .route("/active", get(list_active_friends))
        .route("/{friend_id}/favorite", put(set_favorite))
        .route("/", get(list_friends))
        .route("/{friend_id}", delete(remove_friend))
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
           FROM users WHERE username = $1 LIMIT 1"#,
    )
    .bind(username)
    .fetch_optional(&st.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::UNAUTHORIZED)?;

    if row.get::<bool, _>("is_banned") {
        return Err(StatusCode::FORBIDDEN);
    }
    if row.get::<i64, _>("token_version") != token_version {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(row.get("id"))
}

fn default_settings_json() -> Value {
    serde_json::json!({
        "friend_requests": "everyone",
        "dms": "friends_and_server"
    })
}

async fn get_user_settings_json(db: &PgPool, user_id: i64) -> Value {
    let row = sqlx::query("SELECT settings_json FROM user_settings WHERE user_id = $1 LIMIT 1")
        .bind(user_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let Some(r) = row else {
        return default_settings_json();
    };

    let raw: String = r.get("settings_json");
    serde_json::from_str::<Value>(&raw).unwrap_or_else(|_| default_settings_json())
}

async fn have_mutual_friend(db: &PgPool, a: i64, b: i64) -> bool {
    sqlx::query_scalar::<_, i64>(
        r#"
        SELECT 1::bigint
        FROM friendships f1
        JOIN friendships f2 ON f1.friend_id = f2.friend_id
        WHERE f1.user_id = $1 AND f2.user_id = $2
        LIMIT 1
        "#,
    )
    .bind(a)
    .bind(b)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some()
}

async fn share_server(db: &PgPool, a: i64, b: i64) -> bool {
    sqlx::query_scalar::<_, i64>(
        r#"
        SELECT 1::bigint
        FROM server_members s1
        JOIN server_members s2 ON s1.server_id = s2.server_id
        WHERE s1.user_id = $1 AND s2.user_id = $2
        LIMIT 1
        "#,
    )
    .bind(a)
    .bind(b)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some()
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

    let receiver_id = body.receiver_id;

    if sender_id == receiver_id {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "detail": "Cannot add yourself" })),
        )
            .into_response();
    }

    let rec = match sqlx::query("SELECT is_banned FROM users WHERE id = $1 LIMIT 1")
        .bind(receiver_id)
        .fetch_optional(&st.db)
        .await
    {
        Ok(Some(r)) => r,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "detail": "User not found" })),
            )
                .into_response()
        }
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let is_banned: bool = rec.get(0);
    if is_banned {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "detail": "User banned" })),
        )
            .into_response();
    }

    let settings = get_user_settings_json(&st.db, receiver_id).await;
    let mode = settings
        .get("friend_requests")
        .and_then(|v| v.as_str())
        .unwrap_or("everyone")
        .to_string();

    let allowed = match mode.as_str() {
        "none" => false,
        "server_members" => share_server(&st.db, sender_id, receiver_id).await,
        "friends_of_friends" => have_mutual_friend(&st.db, sender_id, receiver_id).await,
        _ => true,
    };

    if !allowed {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "detail": "Friend requests are restricted by user settings" })),
        )
            .into_response();
    }

    let already = sqlx::query_scalar::<_, i64>(
        r#"SELECT 1::bigint FROM friendships
           WHERE (user_id = $1 AND friend_id = $2)
              OR (user_id = $3 AND friend_id = $4)
           LIMIT 1"#,
    )
    .bind(sender_id)
    .bind(receiver_id)
    .bind(receiver_id)
    .bind(sender_id)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten()
    .is_some();

    if already {
        return (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "ok", "already_friends": true })),
        )
            .into_response();
    }

    let already_pending = sqlx::query_scalar::<_, i64>(
        r#"SELECT 1::bigint FROM friend_requests
           WHERE sender_id = $1 AND receiver_id = $2 AND status = 'pending'
           LIMIT 1"#,
    )
    .bind(sender_id)
    .bind(receiver_id)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten()
    .is_some();

    if already_pending {
        return (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "ok", "dedup": true, "accepted": false })),
        )
            .into_response();
    }

    let incoming_pending = sqlx::query_scalar::<_, i64>(
        r#"SELECT 1::bigint FROM friend_requests
           WHERE sender_id = $1 AND receiver_id = $2 AND status = 'pending'
           LIMIT 1"#,
    )
    .bind(receiver_id)
    .bind(sender_id)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten()
    .is_some();

    if incoming_pending {
        return (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "ok", "incoming_pending": true })),
        )
            .into_response();
    }

    let created_at = auth::now_iso();

    let inserted = match sqlx::query(
        r#"INSERT INTO friend_requests(sender_id, receiver_id, status, created_at)
           VALUES ($1, $2, 'pending', $3) ON CONFLICT DO NOTHING"#,
    )
    .bind(sender_id)
    .bind(receiver_id)
    .bind(created_at)
    .execute(&st.db)
    .await
    {
        Ok(r) => r.rows_affected() > 0,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    if inserted {
        crate::ws::friends_events::friend_request_received(st.hub.as_ref(), receiver_id, sender_id).await;
    }
    let accepted = false;

    (
        StatusCode::OK,
        Json(serde_json::json!({ "status": "ok", "inserted": inserted, "accepted": accepted })),
    )
        .into_response()
}

async fn incoming(State(st): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let Some(tok) = bearer_token(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let me = match current_user_id(&st, &tok).await {
        Ok(id) => id,
        Err(sc) => return sc.into_response(),
    };

    let rows = sqlx::query(
        r#"
        SELECT
            MAX(id)         AS id,
            sender_id       AS sender_id,
            receiver_id     AS receiver_id,
            status          AS status,
            MAX(created_at) AS created_at
        FROM friend_requests
        WHERE receiver_id = $1 AND status = 'pending'
        GROUP BY sender_id, receiver_id, status
        ORDER BY MAX(id) DESC
        "#,
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
            created_at: r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
            is_favorite: false,
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(out)).into_response()
}

async fn outgoing(State(st): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let Some(tok) = bearer_token(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let me = match current_user_id(&st, &tok).await {
        Ok(id) => id,
        Err(sc) => return sc.into_response(),
    };

    let rows = sqlx::query(
        r#"
        SELECT
            MAX(id)         AS id,
            sender_id       AS sender_id,
            receiver_id     AS receiver_id,
            status          AS status,
            MAX(created_at) AS created_at
        FROM friend_requests
        WHERE sender_id = $1 AND status = 'pending'
        GROUP BY sender_id, receiver_id, status
        ORDER BY MAX(id) DESC
        "#,
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
            created_at: r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
            is_favorite: false,
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
           FROM friend_requests WHERE id = $1 LIMIT 1"#,
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

    let _ = sqlx::query(
        r#"UPDATE friend_requests
           SET status = 'accepted'
         WHERE sender_id = $1 AND receiver_id = $2 AND status = 'pending'"#,
    )
    .bind(sender)
    .bind(receiver)
    .execute(&st.db)
    .await;

    let _ = sqlx::query(
        r#"INSERT INTO friendships(user_id, friend_id, created_at)
           VALUES ($1, $2, $3) ON CONFLICT DO NOTHING"#,
    )
    .bind(sender)
    .bind(receiver)
    .bind(created_at)
    .execute(&st.db)
    .await;

    let _ = sqlx::query(
        r#"INSERT INTO friendships(user_id, friend_id, created_at)
           VALUES ($1, $2, $3) ON CONFLICT DO NOTHING"#,
    )
    .bind(receiver)
    .bind(sender)
    .bind(created_at)
    .execute(&st.db)
    .await;

    crate::ws::friends_events::friend_request_accepted(st.hub.as_ref(), sender, receiver).await;

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

    let rq = sqlx::query(r#"SELECT sender_id, receiver_id FROM friend_requests WHERE id = $1"#)
        .bind(request_id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten();

    let Some(rq) = rq else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let sender_id = rq.get::<i64, _>("sender_id");
    let receiver_id = rq.get::<i64, _>("receiver_id");

    if receiver_id != me {
        return StatusCode::FORBIDDEN.into_response();
    }

    let _ = sqlx::query(
        r#"UPDATE friend_requests
           SET status = 'declined'
         WHERE sender_id = $1 AND receiver_id = $2 AND status = 'pending'"#,
    )
    .bind(sender_id)
    .bind(receiver_id)
    .execute(&st.db)
    .await;

    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
}

async fn cancel(
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
           FROM friend_requests WHERE id = $1 LIMIT 1"#,
    )
    .bind(request_id)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten();

    let Some(rq) = rq else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let sender_id = rq.get::<i64, _>("sender_id");
    let receiver_id = rq.get::<i64, _>("receiver_id");
    let status = rq.get::<String, _>("status");

    if sender_id != me {
        return StatusCode::FORBIDDEN.into_response();
    }

    if status != "pending" {
        return (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "ok", "already_done": true })),
        )
            .into_response();
    }

    let _ = sqlx::query(
        r#"UPDATE friend_requests
           SET status = 'cancelled'
         WHERE sender_id = $1 AND receiver_id = $2 AND status = 'pending'"#,
    )
    .bind(sender_id)
    .bind(receiver_id)
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
           WHERE (user_id = $1 AND friend_id = $2)
              OR (user_id = $3 AND friend_id = $4)"#,
    )
    .bind(me)
    .bind(friend_id)
    .bind(friend_id)
    .bind(me)
    .execute(&st.db)
    .await;

    crate::ws::friends_events::friend_removed(st.hub.as_ref(), me, friend_id).await;

    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
}

async fn list_friends(State(st): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let Some(tok) = bearer_token(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let me = match current_user_id(&st, &tok).await {
        Ok(id) => id,
        Err(sc) => return sc.into_response(),
    };

    let rows = sqlx::query(
        r#"
        SELECT f.id as fid, f.created_at as created_at, f.is_favorite as is_favorite,
               u.id as id, u.username as username,
               CASE
                 WHEN COALESCE(p.is_online, FALSE) = FALSE THEN 0
                 WHEN p.status = 'invisible' THEN 0
                 ELSE 1
               END as is_online,
               CASE
                 WHEN COALESCE(p.is_online, FALSE) = FALSE THEN 'offline'
                 WHEN p.status = 'invisible' THEN 'offline'
                 ELSE COALESCE(p.status, 'online')
               END as status
        FROM friendships f
        JOIN users u ON u.id = f.friend_id
        LEFT JOIN user_presence p ON p.user_id = u.id
        WHERE f.user_id = $1
        ORDER BY f.is_favorite DESC, f.id DESC
        "#,
    )
    .bind(me)
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();

    let out = rows
        .into_iter()
        .map(|r| FriendView {
            id: r.get("id"),
            username: r.get("username"),
            is_online: r.get::<i64, _>("is_online") != 0,
            status: r.get::<String, _>("status"),
            created_at: r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
            is_favorite: false,
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(out)).into_response()
}

async fn list_active_friends(State(st): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let Some(tok) = bearer_token(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let me = match current_user_id(&st, &tok).await {
        Ok(id) => id,
        Err(sc) => return sc.into_response(),
    };

    let rows = sqlx::query(
        r#"
        SELECT f.id as fid, f.created_at as created_at,
               u.id as id, u.username as username,
               CASE
                 WHEN COALESCE(p.is_online, FALSE) = FALSE THEN 0
                 WHEN p.status = 'invisible' THEN 0
                 ELSE 1
               END as is_online,
               CASE
                 WHEN COALESCE(p.is_online, FALSE) = FALSE THEN 'offline'
                 WHEN p.status = 'invisible' THEN 'offline'
                 ELSE COALESCE(p.status, 'online')
               END as status
        FROM friendships f
        JOIN users u ON u.id = f.friend_id
        LEFT JOIN user_presence p ON p.user_id = u.id
        WHERE f.user_id = $1
          AND COALESCE(p.is_online, FALSE) = TRUE
          AND COALESCE(p.status, 'online') != 'invisible'
        ORDER BY f.is_favorite DESC, u.username ASC
        "#,
    )
    .bind(me)
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();

    let out = rows
        .into_iter()
        .map(|r| FriendView {
            id: r.get("id"),
            username: r.get("username"),
            is_online: r.get::<i64, _>("is_online") != 0,
            status: r.get::<String, _>("status"),
            created_at: r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
            is_favorite: false,
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(out)).into_response()
}

#[derive(Deserialize)]
pub struct SetFavoriteBody {
    pub favorite: bool,
}

async fn set_favorite(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(friend_id): Path<i64>,
    Json(body): Json<SetFavoriteBody>,
) -> impl IntoResponse {
    let Some(tok) = bearer_token(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let me = match current_user_id(&st, &tok).await {
        Ok(id) => id,
        Err(sc) => return sc.into_response(),
    };

    let q = sqlx::query(
        "UPDATE friendships SET is_favorite = $1 WHERE user_id = $2 AND friend_id = $3",
    )
    .bind(body.favorite)
    .bind(me)
    .bind(friend_id)
    .execute(&st.db)
    .await;

    if q.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    StatusCode::NO_CONTENT.into_response()
}
