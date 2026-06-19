use axum::{
    extract::{Query, State, Path},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use crate::server::AppState;
use crate::middleware::auth_guard::AuthUser;

#[derive(Deserialize)]
pub struct SaveRoomKeyBody {
    pub encrypted_key: String,
    pub nonce: String,
}

#[derive(Serialize)]
pub struct GetRoomKeyResponse {
    pub encrypted_key: String,
    pub nonce: String,
}

#[derive(Deserialize)]
pub struct GetRoomKeysQuery {
    pub chat_id: Option<i64>,
}

#[derive(Serialize)]
pub struct RoomKeyResp {
    pub chat_id: i64,
    pub encrypted_key: String,
    pub nonce: String,
}

pub async fn save_room_key(
    State(st): State<AppState>,
    me: AuthUser,
    Path(chat_id): Path<i64>,
    Json(body): Json<SaveRoomKeyBody>,
) -> impl IntoResponse {
    let is_participant = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM chat_participants WHERE chat_id = ? AND user_id = ? LIMIT 1"
    )
    .bind(chat_id)
    .bind(me.id)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten()
    .is_some();
    
    if !is_participant {
        return StatusCode::FORBIDDEN.into_response();
    }
    
    let now = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "INSERT INTO e2ee_room_keys (user_id, chat_id, encrypted_key, nonce, created_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(user_id, chat_id) DO UPDATE SET
             encrypted_key = excluded.encrypted_key,
             nonce = excluded.nonce,
             created_at = excluded.created_at"
    )
    .bind(me.id)  // ← только свой user_id!
    .bind(chat_id)
    .bind(&body.encrypted_key)
    .bind(&body.nonce)
    .bind(&now)
    .execute(&st.db)
    .await;
    
    match result {
        Ok(_) => StatusCode::OK.into_response(),
        Err(e) => {
            tracing::error!("save_room_key failed: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn get_room_key(
    State(st): State<AppState>,
    me: AuthUser,
    Path(chat_id): Path<i64>,
) -> impl IntoResponse {
    let row = sqlx::query(
        "SELECT encrypted_key, nonce FROM e2ee_room_keys WHERE user_id = ? AND chat_id = ? LIMIT 1"
    )
    .bind(me.id)
    .bind(chat_id)
    .fetch_optional(&st.db)
    .await;
    
    match row {
        Ok(Some(r)) => (StatusCode::OK, Json(GetRoomKeyResponse {
            encrypted_key: r.get("encrypted_key"),
            nonce: r.get("nonce"),
        })).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("get_room_key failed: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Получить все room_keys для текущего пользователя (опционально фильтруя по chat_id)
pub async fn get_room_keys(
    State(st): State<AppState>,
    me: AuthUser,
    Query(q): Query<GetRoomKeysQuery>,
) -> impl IntoResponse {
    let rows = if let Some(chat_id) = q.chat_id {
        sqlx::query_as::<_, (i64, String, String)>(
            "SELECT chat_id, encrypted_key, nonce FROM e2ee_room_keys WHERE user_id = ? AND chat_id = ?"
        )
        .bind(me.id)
        .bind(chat_id)
        .fetch_all(&st.db)
        .await
    } else {
        sqlx::query_as::<_, (i64, String, String)>(
            "SELECT chat_id, encrypted_key, nonce FROM e2ee_room_keys WHERE user_id = ?"
        )
        .bind(me.id)
        .fetch_all(&st.db)
        .await
    };
    
    match rows {
        Ok(rows) => {
            let keys: Vec<RoomKeyResp> = rows.into_iter().map(|(chat_id, encrypted_key, nonce)| {
                RoomKeyResp { chat_id, encrypted_key, nonce }
            }).collect();
            (StatusCode::OK, Json(keys)).into_response()
        }
        Err(e) => {
            tracing::error!("get_room_keys failed: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/room-keys/{chat_id}", post(save_room_key).get(get_room_key))
        .route("/room-keys", get(get_room_keys))
}