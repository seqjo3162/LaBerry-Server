use axum::{
    extract::{Form, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use crate::{auth, server::AppState};
use crate::api_error::ApiError;
use crate::middleware::auth_guard::AuthUser;
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

    // Проверка username
    let username_exists = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM users WHERE username = ? LIMIT 1",
    )
    .bind(&body.username)
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
    .bind(&body.username)
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
    Form(body): Form<LoginBody>,
) -> Result<Response, ApiError> {
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
                token_type: None,
                user_id: Some(r.get("id")),
                requires_2fa: true,
            }),
        )
            .into_response());
    }

    let token = auth::create_access_token(
        r.get("username"),
        r.get("token_version"),
    )
    .map_err(|_| ApiError::Internal("Token error"))?;

    Ok((
        StatusCode::OK,
        Json(LoginResponse {
            access_token: Some(token),
            token_type: Some("bearer".into()),
            user_id: Some(r.get("id")),
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

    let token = auth::create_access_token(
        r.get("username"),
        r.get("token_version"),
    )
    .map_err(|_| ApiError::Internal("Token error"))?;

    Ok((
        StatusCode::OK,
        Json(LoginResponse {
            access_token: Some(token),
            token_type: Some("bearer".into()),
            user_id: Some(r.get("id")),
            requires_2fa: false,
        }),
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

    if res.rows_affected() == 0 {
        return Err(ApiError::Internal("Logout failed"));
    }

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({ "status": "ok" })),
    )
        .into_response())
}

// ===========================
// Verify Token (новое для автологина)
// ===========================
pub async fn verify_token(headers: HeaderMap) -> impl IntoResponse {
    if let Some(auth_header) = headers.get(axum::http::header::AUTHORIZATION) {
        if let Ok(value) = auth_header.to_str() {
            if let Some(token) = value.strip_prefix("Bearer ") {
                if auth::decode_username(token).is_ok() {
                    return StatusCode::OK.into_response();
                }
            }
        }
    }

    (StatusCode::UNAUTHORIZED, Json(json!({ "error": "invalid_token" }))).into_response()
}
