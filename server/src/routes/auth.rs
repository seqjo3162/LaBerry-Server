use axum::{
    extract::{Form, State},
    http::HeaderMap,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};

use serde::{Deserialize, Serialize};
use sqlx::Row;
use crate::{auth, server::AppState};
use crate::api_error::ApiError;
use crate::middleware::auth_guard::AuthUser;
use crate::middleware::rate_limit;
use uuid::Uuid;
use crate::db::bootstrap;

// ===========================
// Structures
// ===========================
#[derive(Serialize)]
pub struct MeResponse {
    pub id: i64,
    pub username: String,
    pub role: String,
}

pub async fn me(user: AuthUser) -> Json<MeResponse> {
    Json(MeResponse {
        id: user.id,
        username: user.username,
        role: user.role,
    })
}

#[derive(Deserialize)]
pub struct RegisterBody {
    pub username: String,
    pub password: String,
    pub email: Option<String>,
}

#[derive(Deserialize)]
pub struct LoginBody {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct Verify2FaBody {
    pub user_id: i64,
    pub code: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub token_type: Option<String>,
    pub user_id: Option<i64>,
    pub requires_2fa: bool,
}

// ===========================
// Router
// ===========================
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/register", post(register))
        .route("/login", post(login))
        .route("/verify-2fa", post(verify_2fa))
        .route("/logout", post(logout))
        .route("/refresh", post(refresh))
        .route("/me", get(me))
        .route("/verify", get(verify_token)) // ✅ новое
}

// ===========================
// Register
// ===========================
async fn register(
    State(st): State<AppState>,
    Json(body): Json<RegisterBody>,
) -> Result<Response, ApiError> {
    let db = &st.db;

    let username = auth::normalize_username(&body.username)
        .ok_or(ApiError::BadRequest("Invalid username"))?;

    // Проверка username
    let username_exists = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM users WHERE username = ? LIMIT 1",
    )
    .bind(&username)
    .fetch_optional(db)
    .await
    .map_err(|_| ApiError::Internal("Database error"))?
    .is_some();

    if username_exists {
        return Err(ApiError::BadRequest("Username already used"));
    }

    // Проверка email
    if let Some(email) = &body.email {
        let email_exists = sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM users WHERE email = ? LIMIT 1",
        )
        .bind(email)
        .fetch_optional(db)
        .await
        .map_err(|_| ApiError::Internal("Database error"))?
        .is_some();

        if email_exists {
            return Err(ApiError::BadRequest("Email already used"));
        }
    }

    let password_hash =
        auth::hash_password(&body.password).map_err(|_| ApiError::Internal("Hash error"))?;
    let created_at = auth::now_iso();

    let result = sqlx::query(
        r#"
        INSERT INTO users
        (username, email, password_hash, is_banned, created_at, token_version, is_2fa_enabled)
        VALUES (?, ?, ?, 0, ?, 1, 0)
        "#,
    )
    .bind(&username)
    .bind(&body.email)
    .bind(&password_hash)
    .bind(&created_at)
    .execute(db)
    .await
    .map_err(|_| ApiError::Internal("Database error"))?;

    // Добавляем в глобальный сервер
    let user_id = result.last_insert_rowid();
    bootstrap::add_user_to_global_server(db, user_id).await
        .map_err(|_| ApiError::Internal("Failed to join global server"))?;

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({ "status": "ok" })),
    ).into_response())
}

// ===========================
// Login
// ===========================
async fn login(
    State(st): State<AppState>,
    headers: HeaderMap,
    Form(body): Form<LoginBody>,
) -> Result<Response, ApiError> {

// rate limit (best-effort, in-memory)
let ip = rate_limit::extract_ip(&headers).unwrap_or_else(|| "unknown".to_string());
let u = body.username.trim().to_ascii_lowercase();
let key = format!("login:{}:{}", ip, u);
if !rate_limit::allow(&key, 12, 300) { // 12 attempts / 5 min per ip+username
    return Err(ApiError::TooManyRequests("Too many login attempts, try later"));
}


    let db = &st.db;

    let r = sqlx::query(
        r#"
        SELECT id, username, password_hash, is_banned,
               token_version, is_2fa_enabled
        FROM users
        WHERE username = ?
        LIMIT 1
        "#,
    )
    .bind(&body.username)
    .fetch_optional(db)
    .await
    .map_err(|_| ApiError::Internal("Database error"))?
    .ok_or(ApiError::Unauthorized("Invalid credentials"))?;

    if r.get::<i64, _>("is_banned") != 0 {
        return Err(ApiError::Forbidden("User banned"));
    }

    if !auth::verify_password(&body.password, r.get("password_hash")) {
        return Err(ApiError::Unauthorized("Invalid credentials"));
    }

    if r.get::<i64, _>("is_2fa_enabled") != 0 {
        let code = auth::generate_2fa_code_6();
        let code_hash = auth::sha256_hex(&code);
        let sent_at = auth::now_iso();

        sqlx::query(
            r#"
            UPDATE users
            SET two_factor_secret_code_hash = ?,
                two_factor_code_sent_at = ?
            WHERE id = ?
            "#,
        )
        .bind(code_hash)
        .bind(sent_at)
        .bind(r.get::<i64, _>("id"))
        .execute(db)
        .await
        .map_err(|_| ApiError::Internal("Database error"))?;

        return Ok((
            StatusCode::OK,
            Json(LoginResponse {
                access_token: None,
                refresh_token: None,
                token_type: None,
                user_id: Some(r.get("id")),
                requires_2fa: true,
            }),
        )
            .into_response());
    }

    
let username: String = r.get("username");
let token_version: i64 = r.get("token_version");
let user_id: i64 = r.get("id");

let token = auth::create_access_token(&username, token_version)
    .map_err(|_| ApiError::Internal("Token error"))?;

// refresh token (rotated, stored hashed)
let refresh_jti = Uuid::new_v4().to_string();
let refresh = auth::create_refresh_token(&username, token_version, &refresh_jti)
    .map_err(|_| ApiError::Internal("Token error"))?;
let refresh_hash = auth::sha256_hex(&refresh);

let now = auth::now_iso();
let ua = headers
    .get(axum::http::header::USER_AGENT)
    .and_then(|h| h.to_str().ok())
    .map(|s| s.to_string());
let ip = rate_limit::extract_ip(&headers);

// access token session (best-effort)
let token_hash = auth::sha256_hex(&token);
let _ = sqlx::query(
    r#"
    INSERT OR IGNORE INTO user_sessions(user_id, token_hash, user_agent, ip, created_at, last_seen_at, revoked_at)
    VALUES(?, ?, ?, ?, ?, ?, NULL)
    "#,
)
.bind(user_id)
.bind(&token_hash)
.bind(ua.clone())
.bind(ip.clone())
.bind(&now)
.bind(&now)
.execute(db)
.await;

// refresh session (strict)
let refresh_claims = auth::decode_refresh_claims(&refresh)
    .map_err(|_| ApiError::Internal("Token error"))?;
let expires_at = refresh_claims.exp.to_string();
let _ = sqlx::query(
    r#"
    INSERT INTO refresh_sessions(user_id, refresh_token_hash, user_agent, ip, created_at, last_used_at, expires_at, revoked_at)
    VALUES(?, ?, ?, ?, ?, ?, ?, NULL)
    "#,
)
.bind(user_id)
.bind(&refresh_hash)
.bind(ua)
.bind(ip)
.bind(&now)
.bind(&now)
.bind(&expires_at)
.execute(db)
.await;

Ok((
    StatusCode::OK,
    Json(LoginResponse {
        access_token: Some(token),
        refresh_token: Some(refresh),
        token_type: Some("bearer".into()),
        user_id: Some(user_id),
        requires_2fa: false,
    }),
)
    .into_response())
}

// ===========================
// Verify 2FA
// ===========================
async fn verify_2fa(
    State(st): State<AppState>,
    Json(body): Json<Verify2FaBody>,
) -> Result<Response, ApiError> {
    let db = &st.db;

    let r = sqlx::query(
        r#"
        SELECT id, username, token_version, two_factor_secret_code_hash
        FROM users
        WHERE id = ?
        LIMIT 1
        "#,
    )
    .bind(body.user_id)
    .fetch_optional(db)
    .await
    .map_err(|_| ApiError::Internal("Database error"))?
    .ok_or(ApiError::NotFound("User not found"))?;

    let stored_hash: Option<String> = r.get("two_factor_secret_code_hash");
    let stored_hash = stored_hash.ok_or(ApiError::NotFound("2FA not active"))?;

    if auth::sha256_hex(&body.code) != stored_hash {
        return Err(ApiError::Unauthorized("Invalid 2FA code"));
    }

    sqlx::query(
        r#"
        UPDATE users
        SET two_factor_secret_code_hash = NULL,
            two_factor_code_sent_at = NULL
        WHERE id = ?
        "#,
    )
    .bind(r.get::<i64, _>("id"))
    .execute(db)
    .await
    .map_err(|_| ApiError::Internal("Database error"))?;

    
let username: String = r.get("username");
let token_version: i64 = r.get("token_version");
let user_id: i64 = r.get("id");

let token = auth::create_access_token(&username, token_version)
    .map_err(|_| ApiError::Internal("Token error"))?;

let refresh_jti = Uuid::new_v4().to_string();
let refresh = auth::create_refresh_token(&username, token_version, &refresh_jti)
    .map_err(|_| ApiError::Internal("Token error"))?;
let refresh_hash = auth::sha256_hex(&refresh);
let now = auth::now_iso();
let refresh_claims = auth::decode_refresh_claims(&refresh)
    .map_err(|_| ApiError::Internal("Token error"))?;
let expires_at = refresh_claims.exp.to_string();

let _ = sqlx::query(
    r#"
    INSERT INTO refresh_sessions(user_id, refresh_token_hash, user_agent, ip, created_at, last_used_at, expires_at, revoked_at)
    VALUES(?, ?, NULL, NULL, ?, ?, ?, NULL)
    "#,
)
.bind(user_id)
.bind(&refresh_hash)
.bind(&now)
.bind(&now)
.bind(&expires_at)
.execute(db)
.await;

Ok((
    StatusCode::OK,
    Json(LoginResponse {
        access_token: Some(token),
        refresh_token: Some(refresh),
        token_type: Some("bearer".into()),
        user_id: Some(user_id),
        requires_2fa: false,
    }),
)
    .into_response())
}


// ===========================
// Refresh (uses refresh token, rotated)
// ===========================
async fn refresh(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let Some(authz) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
    else {
        return Err(ApiError::Unauthorized("Missing token"));
    };

    let Some(token) = authz.strip_prefix("Bearer ") else {
        return Err(ApiError::Unauthorized("Missing token"));
    };

    // rate limit refresh calls per ip
    let ip = rate_limit::extract_ip(&headers).unwrap_or_else(|| "unknown".to_string());
    if !rate_limit::allow(&format!("refresh:{}", ip), 60, 300) {
        return Err(ApiError::TooManyRequests("Too many refresh requests"));
    }

    let claims = auth::decode_refresh_claims(token)
        .map_err(|_| ApiError::Unauthorized("Invalid token"))?;

    let user_row = sqlx::query(
        r#"
        SELECT id, token_version, is_banned
        FROM users
        WHERE username = ?
        LIMIT 1
        "#,
    )
    .bind(&claims.sub)
    .fetch_optional(&st.db)
    .await
    .map_err(|_| ApiError::Internal("Database error"))?
    .ok_or(ApiError::Unauthorized("Invalid token"))?;

    if user_row.get::<i64, _>("is_banned") != 0 {
        return Err(ApiError::Forbidden("User banned"));
    }

    let user_id: i64 = user_row.get("id");
    let tv: i64 = user_row.get("token_version");
    if tv != claims.token_version {
        return Err(ApiError::Unauthorized("Token revoked"));
    }

    let now = auth::now_iso();
    let now_u = auth::now_unix();

    let token_hash = auth::sha256_hex(token);

    // must exist and not be revoked/expired
    let sess = sqlx::query(
        r#"
        SELECT id, revoked_at, expires_at
        FROM refresh_sessions
        WHERE refresh_token_hash = ? AND user_id = ?
        LIMIT 1
        "#,
    )
    .bind(&token_hash)
    .bind(user_id)
    .fetch_optional(&st.db)
    .await
    .map_err(|_| ApiError::Internal("Database error"))?
    .ok_or(ApiError::Unauthorized("Invalid token"))?;

    let revoked_at: Option<String> = sess.get("revoked_at");
    if revoked_at.is_some() {
        return Err(ApiError::Unauthorized("Invalid token"));
    }

    let expires_at: String = sess.get("expires_at");
    let exp = expires_at.parse::<i64>().unwrap_or(0);
    if exp <= now_u {
        let _ = sqlx::query("UPDATE refresh_sessions SET revoked_at = ? WHERE refresh_token_hash = ?")
            .bind(&now)
            .bind(&token_hash)
            .execute(&st.db)
            .await;
        return Err(ApiError::Unauthorized("Invalid token"));
    }

    // rotate: revoke old
    let _ = sqlx::query("UPDATE refresh_sessions SET revoked_at = ?, last_used_at = ? WHERE refresh_token_hash = ?")
        .bind(&now)
        .bind(&now)
        .bind(&token_hash)
        .execute(&st.db)
        .await;

    // mint new pair
    let access = auth::create_access_token(&claims.sub, tv)
        .map_err(|_| ApiError::Internal("Token error"))?;

    let refresh_jti = Uuid::new_v4().to_string();
    let refresh = auth::create_refresh_token(&claims.sub, tv, &refresh_jti)
        .map_err(|_| ApiError::Internal("Token error"))?;
    let refresh_claims = auth::decode_refresh_claims(&refresh)
        .map_err(|_| ApiError::Internal("Token error"))?;
    let refresh_hash = auth::sha256_hex(&refresh);

    let ua = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    let expires_at_new = refresh_claims.exp.to_string();

    let _ = sqlx::query(
        r#"
        INSERT INTO refresh_sessions(user_id, refresh_token_hash, user_agent, ip, created_at, last_used_at, expires_at, revoked_at)
        VALUES(?, ?, ?, ?, ?, ?, ?, NULL)
        "#,
    )
    .bind(user_id)
    .bind(&refresh_hash)
    .bind(ua)
    .bind(rate_limit::extract_ip(&headers))
    .bind(&now)
    .bind(&now)
    .bind(&expires_at_new)
    .execute(&st.db)
    .await;

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "access_token": access,
            "refresh_token": refresh,
            "token_type": "bearer"
        })),
    )
        .into_response())
}

// ===========================
// Logout
// ===========================
async fn logout(
    State(st): State<AppState>,
    user: AuthUser,
) -> Result<Response, ApiError> {
    let res = sqlx::query(
        r#"
        UPDATE users
        SET token_version = token_version + 1
        WHERE id = ?
        "#,
    )
    .bind(user.id)
    .execute(&st.db)
    .await
    .map_err(|_| ApiError::Internal("Database error"))?;

    // revoke refresh sessions
    let now = auth::now_iso();
    let _ = sqlx::query("UPDATE refresh_sessions SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL")
        .bind(&now)
        .bind(user.id)
        .execute(&st.db)
        .await;

    if res.rows_affected() == 0 {
        return Err(ApiError::Internal("Logout failed"));
    }

    let _ = sqlx::query("UPDATE user_presence SET is_online = 0, updated_at = ? WHERE user_id = ?")
        .bind(&now)
        .bind(user.id)
        .execute(&st.db)
        .await;

    st.hub.broadcast_presence(&serde_json::json!({
        "type": "user_offline",
        "user_id": user.id,
        "timestamp": chrono::Utc::now().timestamp_millis()
    }));
    st.hub.disconnect_user(user.id, "logout", "Logged out").await;

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({ "status": "ok" })),
    )
        .into_response())
}

// ===========================
// Verify Token (новое для автологина)
// ===========================
pub async fn verify_token(
    _user: AuthUser,
) -> impl IntoResponse {
    StatusCode::OK.into_response()
}
