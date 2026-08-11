use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{server::AppState, auth};
use crate::middleware::auth_guard::AuthUser;
use crate::middleware::rate_limit;
use tracing;

use super::{
    default_settings, env_bool,
    sanitize_email, sanitize_email_purpose,
    sanitize_settings, sanitize_status,
    send_email_code_by_command, UserConnection,
};

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct UserSettings {
    pub theme: String,
    pub locale: String,
    pub show_header_status: bool,
    pub compact_mode: bool,
    pub show_timestamps: bool,

    pub font_scale: f32,

    pub connections: Vec<UserConnection>,

    pub friend_requests: String,
    pub dms: String,

    pub notify_desktop: bool,
    pub notify_sounds: bool,
    pub notify_dms: bool,
    pub notify_mentions: bool,

    pub voice_input_device_id: String,
    pub voice_video_device_id: String,

    pub developer_mode: bool,
}

impl Default for UserSettings {
    fn default() -> Self {
        default_settings()
    }
}

#[derive(Deserialize)]
pub struct ChangePasswordBody {
    pub old_password: String,
    pub new_password: String,
}

#[derive(Serialize)]
pub struct MyStatus {
    pub status: String,
    pub is_online: bool,
    pub updated_at: Option<String>,
}

#[derive(Deserialize)]
pub struct SetStatusBody {
    pub status: String,
}

#[derive(Deserialize, Default)]
pub struct CookieConsentBody {
    pub accepted: bool,
    pub agreement_version: Option<String>,
}

#[derive(Serialize)]
pub struct CookieConsentResponse {
    pub ok: bool,
    pub cookie_consent_status: String,
    pub trust_factor: i64,
    pub trust_review_status: String,
    pub trust_review_reason: Option<String>,
}

#[derive(Deserialize)]
pub struct ChangeUsernameBody {
    pub username: String,
}

#[derive(Serialize)]
pub struct ChangeUsernameResponse {
    pub status: String,
    pub username: String,
    pub reauth: bool,
}

#[derive(Deserialize)]
pub struct RequestEmailCodeBody {
    pub email: String,
    pub purpose: Option<String>,
}

#[derive(Serialize)]
pub struct RequestEmailCodeResp {
    pub ok: bool,
    pub debug_code: Option<String>,
    pub expires_in_sec: i64,
    pub mail_sent: bool,
    pub delivery: String,
    pub delivery_error: Option<String>,
}

#[derive(Deserialize)]
pub struct ConfirmEmailCodeBody {
    pub code: String,
    pub purpose: Option<String>,
}

pub async fn load_user_settings(db: &sqlx::PgPool, user_id: i64) -> Option<UserSettings> {
    let row = sqlx::query("SELECT settings_json FROM user_settings WHERE user_id = $1 LIMIT 1")
        .bind(user_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    if let Some(r) = row {
        let raw: String = r.get("settings_json");
        if let Ok(v) = serde_json::from_str::<UserSettings>(&raw) {
            return Some(sanitize_settings(v));
        }
    }

    None
}

pub async fn get_my_settings(
    State(st): State<AppState>,
    me: AuthUser,
) -> impl IntoResponse {
    let db = &st.db;

    if let Some(s) = load_user_settings(db, me.id).await {
        return (StatusCode::OK, Json(s)).into_response();
    }

    let def = default_settings();
    let now = auth::now_iso();
    let raw = serde_json::to_string(&def).unwrap_or_else(|_| "{}".to_string());
    let _ = sqlx::query(
        r#"
        INSERT INTO user_settings(user_id, settings_json, updated_at)
        VALUES($1, $2, $3)
        ON CONFLICT(user_id) DO UPDATE SET
          settings_json = excluded.settings_json,
          updated_at = excluded.updated_at
        "#,
    )
    .bind(me.id)
    .bind(raw)
    .bind(&now)
    .execute(db)
    .await;

    (StatusCode::OK, Json(def)).into_response()
}

pub async fn update_my_settings(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<UserSettings>,
) -> impl IntoResponse {
    let db = &st.db;
    let now = auth::now_iso();
    let s = sanitize_settings(body);
    let raw = match serde_json::to_string(&s) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    let q = sqlx::query(
        r#"
        INSERT INTO user_settings(user_id, settings_json, updated_at)
        VALUES($1, $2, $3)
        ON CONFLICT(user_id) DO UPDATE SET
          settings_json = excluded.settings_json,
          updated_at = excluded.updated_at
        "#,
    )
    .bind(me.id)
    .bind(raw)
    .bind(&now)
    .execute(db)
    .await;

    if q.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    (StatusCode::OK, Json(s)).into_response()
}

pub async fn change_password(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<ChangePasswordBody>,
) -> impl IntoResponse {
    let db = &st.db;

    if body.new_password.trim().len() < 8 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"detail": "Password must be at least 8 characters"})),
        )
            .into_response();
    }

    let row = sqlx::query("SELECT password_hash FROM users WHERE id = $1 LIMIT 1")
        .bind(me.id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let Some(r) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let stored: String = r.get("password_hash");
    if !auth::verify_password(&body.old_password, &stored) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"detail": "Invalid current password"})),
        )
            .into_response();
    }

    let new_hash = match auth::hash_password(&body.new_password) {
        Ok(h) => h,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let q = sqlx::query(
        r#"
        UPDATE users
        SET password_hash = $1, token_version = token_version + 1
        WHERE id = $2
        "#,
    )
    .bind(new_hash)
    .bind(me.id)
    .execute(db)
    .await;

    if q.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    (StatusCode::OK, Json(serde_json::json!({"status":"ok","reauth":true}))).into_response()
}

pub async fn my_status(
    State(st): State<AppState>,
    me: AuthUser,
) -> impl IntoResponse {
    let db = &st.db;

    let row = sqlx::query(
        r#"SELECT status, is_online, updated_at FROM user_presence WHERE user_id = $1 LIMIT 1"#,
    )
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    if let Some(r) = row {
        let status: String = r.get::<String, _>("status");
        let is_online = r.get::<bool, _>("is_online");
        let updated_at: Option<String> = r.try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at").ok().map(|d| d.to_rfc3339());
        return (StatusCode::OK, Json(MyStatus { status, is_online, updated_at })).into_response();
    }

    let now = auth::now_iso();
    let _ = sqlx::query(
        "INSERT INTO user_presence(user_id, is_online, status, updated_at) VALUES($1, FALSE, 'online', $2)",
    )
    .bind(me.id)
    .bind(&now)
    .execute(db)
    .await;

    (StatusCode::OK, Json(MyStatus { status: "online".to_string(), is_online: false, updated_at: Some(now.to_rfc3339()) })).into_response()
}

pub async fn set_my_status(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<SetStatusBody>,
) -> impl IntoResponse {
    let db = &st.db;
    let status = sanitize_status(&body.status);
    let now = auth::now_iso();

    let q = sqlx::query(
        r#"
        INSERT INTO user_presence(user_id, is_online, status, updated_at)
        VALUES($1, FALSE, $2, $3)
        ON CONFLICT(user_id) DO UPDATE SET
          status = excluded.status,
          updated_at = excluded.updated_at
        "#,
    )
    .bind(me.id)
    .bind(&status)
    .bind(&now)
    .execute(db)
    .await;

    if q.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let is_online = sqlx::query_scalar::<_, bool>(
        "SELECT is_online FROM user_presence WHERE user_id = $1 LIMIT 1",
    )
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .unwrap_or(false);

    (StatusCode::OK, Json(MyStatus { status, is_online, updated_at: Some(now.to_rfc3339()) })).into_response()
}

pub async fn set_cookie_consent(
    headers: HeaderMap,
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<CookieConsentBody>,
) -> impl IntoResponse {
    let db = &st.db;
    let now = auth::now_iso();
    let agreement_version = body
        .agreement_version
        .as_deref()
        .unwrap_or("cookies-geo-v1")
        .chars()
        .take(48)
        .collect::<String>();

    let (status, trust_factor, review_status, review_reason) = if body.accepted {
        (
            "accepted",
            100_i64,
            "clear",
            Option::<String>::None,
        )
    } else {
        (
            "declined",
            35_i64,
            "review",
            Some(format!(
                "Пользователь отказался от обязательных cookies/storage и проверочных сигналов безопасности. Требуется ручная проверка гео-политики. agreement={}",
                agreement_version
            )),
        )
    };

    let res = sqlx::query(
        r#"
        UPDATE users
        SET cookie_consent_status = $1,
            cookie_consent_at = $2,
            trust_factor = CASE
                WHEN $3 = 'accepted' THEN MAX(COALESCE(trust_factor, 100), $4)
                ELSE MIN(COALESCE(trust_factor, 100), $5)
            END,
            trust_review_status = CASE
                WHEN $6 = 'accepted' AND COALESCE(trust_review_reason, '') LIKE 'Пользователь отказался от обязательных cookies%' THEN 'clear'
                WHEN $7 = 'accepted' AND COALESCE(trust_review_status, 'clear') = 'review' THEN trust_review_status
                ELSE $8
            END,
            trust_review_reason = CASE
                WHEN $9 = 'accepted' AND COALESCE(trust_review_reason, '') LIKE 'Пользователь отказался от обязательных cookies%' THEN NULL
                WHEN $10 = 'accepted' AND COALESCE(trust_review_status, 'clear') = 'review' THEN trust_review_reason
                ELSE $11
            END,
            trust_review_at = CASE
                WHEN $12 = 'accepted' AND COALESCE(trust_review_reason, '') LIKE 'Пользователь отказался от обязательных cookies%' THEN NULL
                WHEN $13 = 'accepted' AND COALESCE(trust_review_status, 'clear') = 'review' THEN trust_review_at
                ELSE $14
            END
        WHERE id = $15
        "#,
    )
    .bind(status)
    .bind(&now)
    .bind(status)
    .bind(trust_factor)
    .bind(trust_factor)
    .bind(status)
    .bind(status)
    .bind(review_status)
    .bind(status)
    .bind(status)
    .bind(&review_reason)
    .bind(status)
    .bind(status)
    .bind(if body.accepted { None } else { Some(now) })
    .bind(me.id)
    .execute(db)
    .await;

    if res.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let row = sqlx::query(
        r#"
        SELECT COALESCE(cookie_consent_status, 'unknown') AS cookie_consent_status,
               COALESCE(trust_factor, 100) AS trust_factor,
               COALESCE(trust_review_status, 'clear') AS trust_review_status,
               trust_review_reason
        FROM users
        WHERE id = $1
        LIMIT 1
        "#,
    )
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(row) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let body = CookieConsentResponse {
        ok: true,
        cookie_consent_status: row.get("cookie_consent_status"),
        trust_factor: row.get("trust_factor"),
        trust_review_status: row.get("trust_review_status"),
        trust_review_reason: row.get("trust_review_reason"),
    };

    let mut response = (StatusCode::OK, Json(body)).into_response();
    let secure = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("https"))
        .unwrap_or(false);
    let secure_suffix = if secure { "; Secure" } else { "" };
    let cookie = format!(
        "lb_cookie_consent={}; Path=/; Max-Age=31536000; SameSite=Lax{}",
        status,
        secure_suffix
    );
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }

    response
}

pub async fn change_username(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<ChangeUsernameBody>,
) -> impl IntoResponse {
    let db = &st.db;

    let Some(new_username) = auth::normalize_username(&body.username) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"detail":"Invalid username"})),
        )
            .into_response();
    };

    let current_username = match sqlx::query_scalar::<_, String>(
        "SELECT username FROM users WHERE id = $1 LIMIT 1",
    )
    .bind(me.id)
    .fetch_optional(db)
    .await
    {
        Ok(Some(v)) => v,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    if current_username == new_username {
        return (
            StatusCode::OK,
            Json(ChangeUsernameResponse {
                status: "ok".to_string(),
                username: current_username,
                reauth: false,
            }),
        )
            .into_response();
    }

    let taken = sqlx::query_scalar::<_, i64>(
        "SELECT 1::bigint FROM users WHERE username = $1 AND id != $2 LIMIT 1",
    )
    .bind(&new_username)
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some();
    if taken {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"detail":"Username already used"})),
        )
            .into_response();
    }

    let now = auth::now_iso();

    let mut tx = match db.begin().await {
        Ok(tx) => tx,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let upd = sqlx::query(
        r#"
        UPDATE users
        SET username = $1,
            token_version = token_version + 1
        WHERE id = $2
        "#,
    )
    .bind(&new_username)
    .bind(me.id)
    .execute(&mut *tx)
    .await;

    if upd.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let _ = sqlx::query(
        "UPDATE user_sessions SET revoked_at = $1 WHERE user_id = $2 AND revoked_at IS NULL",
    )
    .bind(&now)
    .bind(me.id)
    .execute(&mut *tx)
    .await;

    let _ = sqlx::query(
        "UPDATE refresh_sessions SET revoked_at = $1 WHERE user_id = $2 AND revoked_at IS NULL",
    )
    .bind(&now)
    .bind(me.id)
    .execute(&mut *tx)
    .await;

    if tx.commit().await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    st.hub
        .disconnect_user(me.id, "username_changed", "Username changed")
        .await;

    (
        StatusCode::OK,
        Json(ChangeUsernameResponse {
            status: "ok".to_string(),
            username: new_username,
            reauth: true,
        }),
    )
        .into_response()
}

pub async fn request_email_code(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<RequestEmailCodeBody>,
) -> impl IntoResponse {
    let db = &st.db;

    let Some(email) = sanitize_email(&body.email) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"detail":"Invalid email"})),
        )
            .into_response();
    };

    let taken = sqlx::query_scalar::<_, i64>(
        "SELECT 1::bigint FROM users WHERE email = $1 AND id != $2 LIMIT 1",
    )
    .bind(&email)
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some();
    if taken {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"detail":"Email already in use"})),
        )
            .into_response();
    }

    let purpose = sanitize_email_purpose(body.purpose);

let rl_key = format!("email_code:{}:{}", me.id, purpose);
if !rate_limit::allow(&rl_key, 5, 3600) {
    return (
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({"detail":"Too many requests"})),
    )
        .into_response();
}

    let q = sqlx::query("UPDATE users SET email_pending = $1, email_verified = FALSE WHERE id = $2")
        .bind(&email)
        .bind(me.id)
        .execute(db)
        .await;
    if q.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let now = auth::now_iso();
    let _ = sqlx::query(
        "UPDATE email_codes SET consumed_at = $1 WHERE user_id = $2 AND purpose = $3 AND consumed_at IS NULL",
    )
    .bind(&now)
    .bind(me.id)
    .bind(&purpose)
    .execute(db)
    .await;

    let code = auth::generate_2fa_code_6();
    let code_hash = auth::sha256_hex(&format!("{}:{}:{}", me.id, &purpose, &code));
    let expires_at = now + chrono::Duration::seconds(10 * 60);

    let ins = sqlx::query(
        r#"
        INSERT INTO email_codes(user_id, purpose, code_hash, sent_to_email, created_at, expires_at)
        VALUES($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(me.id)
    .bind(&purpose)
    .bind(&code_hash)
    .bind(&email)
    .bind(&now)
    .bind(&expires_at)
    .execute(db)
    .await;

    if ins.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let debug = env_bool("LB_DEBUG_EMAIL_CODES");
    let local_fallback_code = env_bool("LB_EMAIL_LOCAL_CODE_FALLBACK");

    let (mail_sent, delivery, delivery_error) = match send_email_code_by_command(&email, &code, &purpose) {
        Ok(()) => (true, "sent".to_string(), None),
        Err(e) if e == "not_configured" => {
            tracing::warn!("email code delivery is not configured: set LB_EMAIL_SEND_COMMAND");
            (false, "not_configured".to_string(), None)
        }
        Err(e) => {
            tracing::warn!("email code delivery failed: {}", e);
            (false, "failed".to_string(), Some(e))
        }
    };

    let resp = RequestEmailCodeResp {
        ok: true,
        debug_code: if debug || (local_fallback_code && delivery == "not_configured") {
            Some(code)
        } else {
            None
        },
        expires_in_sec: 10 * 60,
        mail_sent,
        delivery,
        delivery_error,
    };
    (StatusCode::OK, Json(resp)).into_response()
}

pub async fn confirm_email_code(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<ConfirmEmailCodeBody>,
) -> impl IntoResponse {
    let db = &st.db;
    let code = body.code.trim();
    if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"detail":"Invalid code"})),
        )
            .into_response();
    }
    let purpose = sanitize_email_purpose(body.purpose);

let rl_key = format!("email_confirm:{}:{}", me.id, purpose);
if !rate_limit::allow(&rl_key, 10, 3600) {
    return (
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({"detail":"Too many requests"})),
    )
        .into_response();
}
    let now = auth::now_iso();
    let want_hash = auth::sha256_hex(&format!("{}:{}:{}", me.id, &purpose, code));

    let row = sqlx::query(
        r#"
        SELECT id, code_hash, expires_at
        FROM email_codes
        WHERE user_id = $1 AND purpose = $2 AND consumed_at IS NULL
        ORDER BY id DESC
        LIMIT 1
        "#,
    )
    .bind(me.id)
    .bind(&purpose)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(r) = row else {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"detail":"Code not found"})),
        )
            .into_response();
    };

    let code_id: i64 = r.get("id");
    let code_hash: String = r.get("code_hash");
    let expires_at: chrono::DateTime<chrono::Utc> = r.get("expires_at");
    if expires_at <= chrono::Utc::now() {
        let _ = sqlx::query("UPDATE email_codes SET consumed_at = $1 WHERE id = $2")
            .bind(&now)
            .bind(code_id)
            .execute(db)
            .await;
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"detail":"Code expired"})),
        )
            .into_response();
    }

    if code_hash != want_hash {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"detail":"Invalid code"})),
        )
            .into_response();
    }

    let _ = sqlx::query("UPDATE email_codes SET consumed_at = $1 WHERE id = $2")
        .bind(&now)
        .bind(code_id)
        .execute(db)
        .await;

    let pending: Option<String> = sqlx::query_scalar(
        "SELECT email_pending FROM users WHERE id = $1 LIMIT 1",
    )
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(pending_email) = pending.as_deref().and_then(sanitize_email) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"detail":"No pending email"})),
        )
            .into_response();
    };

    let taken = sqlx::query_scalar::<_, i64>(
        "SELECT 1::bigint FROM users WHERE email = $1 AND id != $2 LIMIT 1",
    )
    .bind(&pending_email)
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some();
    if taken {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"detail":"Email already in use"})),
        )
            .into_response();
    }

    let q = sqlx::query(
        "UPDATE users SET email = $1, email_verified = TRUE, email_pending = NULL WHERE id = $2",
    )
    .bind(&pending_email)
    .bind(me.id)
    .execute(db)
    .await;

    if q.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    (StatusCode::OK, Json(serde_json::json!({"ok":true}))).into_response()
}
