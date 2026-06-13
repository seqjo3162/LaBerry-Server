use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    routing::{post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use crate::server::AppState;

#[derive(Debug, Serialize, Deserialize)]
pub struct SaveRoomKeyReq {
    pub chat_id: i64,
    pub encrypted_key: String,
    pub nonce: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RoomKeyResp {
    pub chat_id: i64,
    pub encrypted_key: String,
    pub nonce: String,
}

async fn get_user_id_from_token(headers: &HeaderMap, db: &sqlx::SqlitePool) -> Result<i64, StatusCode> {
    let token = headers.get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let (username, _) = crate::auth::decode_username(token).map_err(|_| StatusCode::UNAUTHORIZED)?;

    let row = sqlx::query("SELECT id FROM users WHERE username = ? AND is_banned = 0")
        .bind(&username)
        .fetch_optional(db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::UNAUTHORIZED)?;

    Ok(row.get::<i64, _>("id"))
}

async fn save_room_key(
    headers: HeaderMap,
    State(st): State<AppState>,
    Json(req): Json<SaveRoomKeyReq>,
) -> Result<StatusCode, StatusCode> {
    let user_id = get_user_id_from_token(&headers, &st.db).await?;

    let is_member = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM chat_participants WHERE chat_id = ? AND user_id = ? LIMIT 1"
    )
    .bind(req.chat_id)
    .bind(user_id)
    .fetch_optional(&st.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .is_some();

    if !is_member { return Err(StatusCode::FORBIDDEN); }

    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        r#"INSERT INTO e2ee_room_keys (user_id, chat_id, encrypted_key, nonce, created_at)
           VALUES (?, ?, ?, ?, ?)
           ON CONFLICT(user_id, chat_id) DO UPDATE SET
               encrypted_key = excluded.encrypted_key,
               nonce = excluded.nonce,
               created_at = excluded.created_at"#
    )
    .bind(user_id).bind(req.chat_id).bind(&req.encrypted_key).bind(&req.nonce).bind(&now)
    .execute(&st.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::OK)
}

#[derive(Debug, Deserialize)]
pub struct GetRoomKeysQuery { pub chat_id: Option<i64> }

async fn get_room_keys(
    headers: HeaderMap,
    State(st): State<AppState>,
    Query(q): Query<GetRoomKeysQuery>,
) -> Result<Json<Vec<RoomKeyResp>>, StatusCode> {
    let user_id = get_user_id_from_token(&headers, &st.db).await?;

    let rows = if let Some(chat_id) = q.chat_id {
        sqlx::query_as::<_, (i64, String, String)>(
            "SELECT chat_id, encrypted_key, nonce FROM e2ee_room_keys WHERE user_id = ? AND chat_id = ?"
        ).bind(user_id).bind(chat_id).fetch_all(&st.db).await
    } else {
        sqlx::query_as::<_, (i64, String, String)>(
            "SELECT chat_id, encrypted_key, nonce FROM e2ee_room_keys WHERE user_id = ?"
        ).bind(user_id).fetch_all(&st.db).await
    }.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let keys = rows.into_iter().map(|(chat_id, encrypted_key, nonce)| RoomKeyResp {
        chat_id, encrypted_key, nonce,
    }).collect();

    Ok(Json(keys))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/keys", post(save_room_key).get(get_room_keys))
}