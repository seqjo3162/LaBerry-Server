use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{server::AppState, auth};
use crate::middleware::auth_guard::AuthUser;
use crate::middleware::rate_limit;

use super::{sanitize_report_message, sanitize_report_reason, trim_chars};
use uuid;

#[derive(Serialize)]
pub struct BlockView {
    pub user_id: i64,
    pub username: String,
    pub created_at: String,
}

#[derive(Deserialize, Default)]
pub struct ReportUserBody {
    pub reason: Option<String>,
    pub message: Option<String>,
    pub message_id: Option<i64>,
}

#[derive(Serialize)]
pub struct ReportUserResponse {
    pub ok: bool,
    pub id: i64,
}

#[derive(Deserialize, Default)]
pub struct SuggestionBody {
    pub title: Option<String>,
    #[serde(default)]
    pub message: String,
}

#[derive(Serialize)]
pub struct SuggestionResponse {
    pub ok: bool,
    pub id: i64,
}

#[derive(Serialize)]
pub struct SuggestionTicketView {
    pub id: i64,
    pub title: String,
    pub message: String,
    pub status: String,
    pub created_at: String,
    pub reviewed_at: Option<String>,
    pub admin_note: String,
}

#[derive(Deserialize)]
pub struct DeleteMeBody {
    pub username: String,
}

pub async fn list_blocks(State(st): State<AppState>, me: AuthUser) -> impl IntoResponse {
    let rows = sqlx::query(
        r#"
        SELECT b.blocked_id AS user_id, u.username, b.created_at
        FROM user_blocks b
        JOIN users u ON u.id = b.blocked_id
        WHERE b.blocker_id = $1
        ORDER BY b.created_at DESC
        LIMIT 200
        "#,
    )
    .bind(me.id)
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();

    let out = rows
        .into_iter()
        .map(|r| BlockView {
            user_id: r.get("user_id"),
            username: r.get("username"),
            created_at: r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(out)).into_response()
}

pub async fn block_user(State(st): State<AppState>, me: AuthUser, Path(user_id): Path<i64>) -> impl IntoResponse {
    if user_id == me.id {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let exists = sqlx::query_scalar::<_, i64>("SELECT 1::bigint FROM users WHERE id = $1 LIMIT 1")
        .bind(user_id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten()
        .is_some();

    if !exists {
        return StatusCode::NOT_FOUND.into_response();
    }

    let now = auth::now_iso();
    let _ = sqlx::query(
        "INSERT INTO user_blocks(blocker_id, blocked_id, created_at) VALUES($1, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(me.id)
    .bind(user_id)
    .bind(now)
    .execute(&st.db)
    .await;

    (StatusCode::OK, Json(serde_json::json!({"status":"ok"}))).into_response()
}

pub async fn unblock_user(State(st): State<AppState>, me: AuthUser, Path(user_id): Path<i64>) -> impl IntoResponse {
    let _ = sqlx::query("DELETE FROM user_blocks WHERE blocker_id = $1 AND blocked_id = $2")
        .bind(me.id)
        .bind(user_id)
        .execute(&st.db)
        .await;

    (StatusCode::OK, Json(serde_json::json!({"status":"ok"}))).into_response()
}

pub async fn list_my_suggestions(
    State(st): State<AppState>,
    me: AuthUser,
) -> impl IntoResponse {
    let rows = sqlx::query(
        r#"
        SELECT id, title, message, status, created_at, reviewed_at, admin_note
        FROM user_suggestions
        WHERE user_id = $1
        ORDER BY id DESC
        LIMIT 100
        "#,
    )
    .bind(me.id)
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();

    let out = rows
        .into_iter()
        .map(|r| SuggestionTicketView {
            id: r.get("id"),
            title: r.try_get("title").unwrap_or_default(),
            message: r.try_get("message").unwrap_or_default(),
            status: r.try_get("status").unwrap_or_else(|_| "open".to_string()),
            created_at: r.try_get::<chrono::DateTime<chrono::Utc>, _>("created_at").ok().map(|d| d.to_rfc3339()).unwrap_or_default(),
            reviewed_at: r.try_get::<chrono::DateTime<chrono::Utc>, _>("reviewed_at").ok().map(|d| d.to_rfc3339()),
            admin_note: r.try_get("admin_note").unwrap_or_default(),
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(out)).into_response()
}

pub async fn create_suggestion(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<SuggestionBody>,
) -> impl IntoResponse {
    let title = trim_chars(body.title.as_deref().unwrap_or(""), 80);
    let message = trim_chars(&body.message, 2000);

    if message.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"detail":"Suggestion text is required"})),
        )
            .into_response();
    }

    const SUGGESTION_COOLDOWN_SEC: i64 = 5 * 60;
    let last_age_sec = sqlx::query_scalar::<_, Option<i64>>(
        r#"
        SELECT EXTRACT(EPOCH FROM now())::bigint - EXTRACT(EPOCH FROM created_at)::bigint
        FROM user_suggestions
        WHERE user_id = $1
        ORDER BY id DESC
        LIMIT 1
        "#,
    )
    .bind(me.id)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten()
    .flatten();

    if let Some(age) = last_age_sec {
        if (0..SUGGESTION_COOLDOWN_SEC).contains(&age) {
            let retry = SUGGESTION_COOLDOWN_SEC - age;
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "detail":"Suggestion slow mode",
                    "retry_after_sec": retry
                })),
            )
                .into_response();
        }
    }

    let rl_key = format!("user_suggestion:{}", me.id);
    if !rate_limit::allow(&rl_key, 8, 3600) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"detail":"Too many suggestions"})),
        )
            .into_response();
    }

    let created_at = auth::now_iso();
    let res = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO user_suggestions(user_id, title, message, status, created_at)
        VALUES($1, $2, $3, 'open', $4) RETURNING id
        "#,
    )
    .bind(me.id)
    .bind(&title)
    .bind(&message)
    .bind(created_at)
    .fetch_one(&st.db)
    .await;

    let Ok(id) = res else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    (
        StatusCode::OK,
        Json(SuggestionResponse {
            ok: true,
            id,
        }),
    )
        .into_response()
}

pub async fn report_user(
    State(st): State<AppState>,
    me: AuthUser,
    Path(target_user_id): Path<i64>,
    Json(body): Json<ReportUserBody>,
) -> impl IntoResponse {
    if target_user_id <= 0 || target_user_id == me.id {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"detail":"Bad target user"})),
        )
            .into_response();
    }

    let rl_key = format!("user_report:{}:{}", me.id, target_user_id);
    if !rate_limit::allow(&rl_key, 5, 3600) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"detail":"Too many reports"})),
        )
            .into_response();
    }

    let exists = sqlx::query_scalar::<_, i64>("SELECT 1::bigint FROM users WHERE id = $1 LIMIT 1")
        .bind(target_user_id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten()
        .is_some();
    if !exists {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"detail":"User not found"})),
        )
            .into_response();
    }

    let reason = sanitize_report_reason(body.reason);
    let message = sanitize_report_message(body.message);
    let message_id = body.message_id.filter(|id| *id > 0);
    let created_at = auth::now_iso();

    let res = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO user_reports(reporter_id, target_user_id, message_id, reason, message, status, created_at)
        VALUES($1, $2, $3, $4, $5, 'open', $6) RETURNING id
        "#,
    )
    .bind(me.id)
    .bind(target_user_id)
    .bind(message_id)
    .bind(&reason)
    .bind(&message)
    .bind(created_at)
    .fetch_one(&st.db)
    .await;

    let Ok(id) = res else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    (
        StatusCode::OK,
        Json(ReportUserResponse {
            ok: true,
            id,
        }),
    )
        .into_response()
}

pub async fn delete_me(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<DeleteMeBody>,
) -> impl IntoResponse {
    let db = &st.db;

    let row = sqlx::query("SELECT username FROM users WHERE id = $1 LIMIT 1")
        .bind(me.id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let Some(r) = row else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let current_username: String = r.get("username");
    if current_username != body.username {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"detail":"username_mismatch"})),
        )
            .into_response();
    }

    let mut tx = match db.begin().await {
        Ok(t) => t,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let _ = sqlx::query("DELETE FROM user_sessions WHERE user_id = $1")
        .bind(me.id)
        .execute(&mut *tx)
        .await;

    let _ = sqlx::query("DELETE FROM friendships WHERE user_id = $1 OR friend_id = $2")
        .bind(me.id)
        .bind(me.id)
        .execute(&mut *tx)
        .await;

    let _ = sqlx::query("DELETE FROM friend_requests WHERE sender_id = $1 OR receiver_id = $2")
        .bind(me.id)
        .bind(me.id)
        .execute(&mut *tx)
        .await;

    let _ = sqlx::query("DELETE FROM server_members WHERE user_id = $1")
        .bind(me.id)
        .execute(&mut *tx)
        .await;

    let new_username = format!("deleted_{}", me.id);
    let new_pwd = format!("deleted:{}", uuid::Uuid::new_v4());

    let _ = sqlx::query(
        r#"
        UPDATE users
        SET username = $1,
            email = NULL,
            email_pending = NULL,
            email_verified = FALSE,
            password_hash = $2,
            token_version = token_version + 1,
            public_encryption_key = NULL,
            is_banned = TRUE
        WHERE id = $3
        "#,
    )
    .bind(&new_username)
    .bind(&new_pwd)
    .bind(me.id)
    .execute(&mut *tx)
    .await;

    let _ = sqlx::query("UPDATE user_presence SET is_online = FALSE, status = 'offline', updated_at = $1 WHERE user_id = $2")
        .bind(auth::now_iso())
        .bind(me.id)
        .execute(&mut *tx)
        .await;

    if tx.commit().await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    (StatusCode::OK, Json(serde_json::json!({"detail":"deleted"}))).into_response()
}
