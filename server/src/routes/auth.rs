use axum::{
    extract::{Form, State, ConnectInfo},
    http::HeaderMap,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use sqlx::Row;
use sea_query::{Iden, IdenStatic};
use uuid::Uuid;

use crate::{auth, server::AppState};
use crate::api_error::ApiError;
use crate::middleware::auth_guard::AuthUser;
use crate::middleware::rate_limit;
use crate::db::bootstrap;
use crate::models::{
    UserIden, AuditLogIden, UserSessionIden, RefreshSessionIden, UserPresenceIden,
};

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
    pub accepted_terms: Option<bool>,
    pub agreement_version: Option<String>,
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

#[derive(Deserialize)]
pub struct LogoutBody {
    pub refresh_token: Option<String>,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub token_type: Option<String>,
    pub user_id: Option<i64>,
    pub requires_2fa: bool,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/register", post(register))
        .route("/login", post(login))
        .route("/verify-2fa", post(verify_2fa))
        .route("/logout", post(logout))
        .route("/logout_all", post(logout_all))
        .route("/refresh", post(refresh))
        .route("/me", get(me))
        .route("/verify", get(verify_token))
}

async fn register(
    State(st): State<AppState>,
    Json(body): Json<RegisterBody>,
) -> Result<Response, ApiError> {
    let db = &st.db;

    let username = auth::normalize_username(&body.username)
        .ok_or(ApiError::BadRequest("Invalid username"))?;

    if body.accepted_terms != Some(true) {
        return Err(ApiError::BadRequest("Terms agreement required"));
    }

    let username_exists = sqlx::query_scalar::<_, i64>(
        sqlx::AssertSqlSafe(format!(
            "SELECT 1::bigint FROM {} WHERE {} = $1 LIMIT 1",
            UserIden::Table.to_string(),
            UserIden::Username.to_string()
        ))
    )
    .bind(&username)
    .fetch_optional(db)
    .await
    .map_err(|_| ApiError::Internal("Database error"))?
    .is_some();

    if username_exists {
        return Err(ApiError::BadRequest("Username already used"));
    }

    if let Some(email) = &body.email {
        let email_exists = sqlx::query_scalar::<_, i64>(
            sqlx::AssertSqlSafe(format!(
                "SELECT 1::bigint FROM {} WHERE {} = $1 LIMIT 1",
                UserIden::Table.to_string(),
                UserIden::Email.to_string()
            ))
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

    let password_hash = auth::hash_password(&body.password)
        .map_err(|_| ApiError::Internal("Hash error"))?;
    let created_at = auth::now_iso();
    let agreement_version = body
        .agreement_version
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("license-rules-2026-05-24");

    let row = sqlx::query(
        sqlx::AssertSqlSafe(format!(
            "INSERT INTO {} (
                {}, {}, {}, {}, {}, {}, {},
                {}, {}
            ) VALUES ($1, $2, $3, false, $4, 1, false, $5, $6)
            RETURNING {}",
            UserIden::Table.to_string(),
            UserIden::Username.to_string(),
            UserIden::Email.to_string(),
            UserIden::PasswordHash.to_string(),
            UserIden::IsBanned.to_string(),
            UserIden::CreatedAt.to_string(),
            UserIden::TokenVersion.to_string(),
            UserIden::Is2faEnabled.to_string(),
            UserIden::TermsAcceptedAt.to_string(),
            UserIden::TermsAgreementVersion.to_string(),
            UserIden::Id.to_string()
        ))
    )
    .bind(&username)
    .bind(&body.email)
    .bind(&password_hash)
    .bind(created_at)
    .bind(created_at)
    .bind(agreement_version)
    .fetch_one(db)
    .await
    .map_err(|_| ApiError::Internal("Database error"))?;

    let user_id: i64 = row.get(0);

    bootstrap::add_user_to_global_server(db, user_id).await
        .map_err(|_| ApiError::Internal("Failed to join global server"))?;

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({ "status": "ok" })),
    ).into_response())
}

async fn login(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(body): Form<LoginBody>,
) -> Result<Response, ApiError> {
    let ip = rate_limit::extract_ip(&headers, Some(peer.ip()), st.trusted_proxies.as_slice())
        .unwrap_or_else(|| "unknown".to_string());
    let u = body.username.trim().to_ascii_lowercase();
    let key = format!("login:{}:{}", ip, u);
    if !rate_limit::allow(&key, 12, 300) {
        return Err(ApiError::TooManyRequests("Too many login attempts, try later"));
    }

    let db = &st.db;

    let r = sqlx::query(
        sqlx::AssertSqlSafe(format!(
            "SELECT {}, {}, {}, {}, {}, {}
             FROM {}
             WHERE {} = $1
             LIMIT 1",
            UserIden::Id.to_string(),
            UserIden::Username.to_string(),
            UserIden::PasswordHash.to_string(),
            UserIden::IsBanned.to_string(),
            UserIden::TokenVersion.to_string(),
            UserIden::Is2faEnabled.to_string(),
            UserIden::Table.to_string(),
            UserIden::Username.to_string()
        ))
    )
    .bind(&body.username)
    .fetch_optional(db)
    .await
    .map_err(|_| ApiError::Internal("Database error"))?;

    let (user_exists, r_unwrapped) = match r {
        Some(row) => (true, Some(row)),
        None => (false, None),
    };

    let stored_password_hash = r_unwrapped.as_ref().map(|r| r.get::<String, _>(UserIden::PasswordHash.as_str()));
    if !auth::verify_password_timing_safe(&body.password, stored_password_hash.as_deref()) {
        return Err(ApiError::Unauthorized("Invalid credentials"));
    }

    if !user_exists {
        return Err(ApiError::Unauthorized("Invalid credentials"));
    }

    let Some(r) = r_unwrapped else {
        return Err(ApiError::Internal("User vanished"));
    };

    if r.get::<bool, _>(UserIden::IsBanned.as_str()) {
        return Err(ApiError::Forbidden("User banned"));
    }

    let user_id: i64 = r.get(UserIden::Id.as_str());
    let username: String = r.get(UserIden::Username.as_str());
    let token_version: i64 = r.get(UserIden::TokenVersion.as_str());

    if r.get::<bool, _>(UserIden::Is2faEnabled.as_str()) {
        let code = auth::generate_2fa_code_6();
        let code_hash = auth::sha256_hex(&code);
        let sent_at = auth::now_iso();
        let expires_at = chrono::Utc::now() + chrono::Duration::seconds(300);

        sqlx::query(
            sqlx::AssertSqlSafe(format!(
                "UPDATE {}
                 SET {} = $1, {} = $2, {} = $3, {} = 0, {} = NULL
                 WHERE {} = $4",
                UserIden::Table.to_string(),
                UserIden::TwoFactorSecretCodeHash.to_string(),
                UserIden::TwoFactorCodeSentAt.to_string(),
                UserIden::TwoFactorCodeExpiresAt.to_string(),
                UserIden::TwoFactorCodeAttempts.to_string(),
                UserIden::TwoFactorLockedUntil.to_string(),
                UserIden::Id.to_string()
            ))
        )
        .bind(code_hash)
        .bind(sent_at)
        .bind(expires_at)
        .bind(user_id)
        .execute(db)
        .await
        .map_err(|_| ApiError::Internal("Database error"))?;

        let _ = sqlx::query(
            sqlx::AssertSqlSafe(format!(
                "INSERT INTO {} ({}, {}, {}, {}, {})
                 VALUES ($1, $2, $3, $4, $5)",
                AuditLogIden::Table.to_string(),
                AuditLogIden::UserId.to_string(),
                AuditLogIden::Action.to_string(),
                AuditLogIden::Status.to_string(),
                AuditLogIden::IpAddress.to_string(),
                AuditLogIden::CreatedAt.to_string()
            ))
        )
        .bind(user_id)
        .bind("2fa_code_request")
        .bind("success")
        .bind(&ip)
        .bind(auth::now_iso())
        .execute(db)
        .await;

        return Ok((
            StatusCode::OK,
            Json(LoginResponse {
                access_token: None,
                refresh_token: None,
                token_type: None,
                user_id: Some(user_id),
                requires_2fa: true,
            }),
        ).into_response());
    }

    let token = auth::create_access_token(&username, token_version)
        .map_err(|_| ApiError::Internal("Token error"))?;

    let refresh_jti = Uuid::new_v4().to_string();
    let refresh = auth::create_refresh_token(&username, token_version, &refresh_jti)
        .map_err(|_| ApiError::Internal("Token error"))?;
    let refresh_hash = auth::sha256_hex(&refresh);

    let now = auth::now_iso();
    let ua = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    let ip = rate_limit::extract_ip(&headers, Some(peer.ip()), st.trusted_proxies.as_slice());

    let token_hash = auth::sha256_hex(&token);
    let _ = sqlx::query(
        sqlx::AssertSqlSafe(format!(
            "INSERT INTO {} ({}, {}, {}, {}, {}, {}, {})
             VALUES ($1, $2, $3, $4, $5, $6, NULL)
             ON CONFLICT ({}) DO NOTHING",
            UserSessionIden::Table.to_string(),
            UserSessionIden::UserId.to_string(),
            UserSessionIden::TokenHash.to_string(),
            UserSessionIden::UserAgent.to_string(),
            UserSessionIden::Ip.to_string(),
            UserSessionIden::CreatedAt.to_string(),
            UserSessionIden::LastSeenAt.to_string(),
            UserSessionIden::RevokedAt.to_string(),
            UserSessionIden::TokenHash.to_string()
        ))
    )
    .bind(user_id)
    .bind(&token_hash)
    .bind(ua.clone())
    .bind(ip.clone())
    .bind(now)
    .bind(now)
    .execute(db)
    .await;

    let refresh_claims = auth::decode_refresh_claims(&refresh)
        .map_err(|_| ApiError::Internal("Token error"))?;
    let expires_at = chrono::DateTime::from_timestamp(refresh_claims.exp, 0)
        .unwrap_or_else(chrono::Utc::now);

    let _ = sqlx::query(
        sqlx::AssertSqlSafe(format!(
            "INSERT INTO {} ({}, {}, {}, {}, {}, {}, {}, {})
             VALUES ($1, $2, $3, $4, $5, $6, $7, NULL)",
            RefreshSessionIden::Table.to_string(),
            RefreshSessionIden::UserId.to_string(),
            RefreshSessionIden::RefreshTokenHash.to_string(),
            RefreshSessionIden::UserAgent.to_string(),
            RefreshSessionIden::Ip.to_string(),
            RefreshSessionIden::CreatedAt.to_string(),
            RefreshSessionIden::LastUsedAt.to_string(),
            RefreshSessionIden::ExpiresAt.to_string(),
            RefreshSessionIden::RevokedAt.to_string()
        ))
    )
    .bind(user_id)
    .bind(&refresh_hash)
    .bind(ua)
    .bind(ip)
    .bind(now)
    .bind(now)
    .bind(expires_at)
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
    ).into_response())
}

async fn verify_2fa(
    State(st): State<AppState>,
    Json(body): Json<Verify2FaBody>,
) -> Result<Response, ApiError> {
    let db = &st.db;

    let r = sqlx::query(
        sqlx::AssertSqlSafe(format!(
            "SELECT {}, {}, {}, {}, {}, {}, {}
             FROM {}
             WHERE {} = $1
             LIMIT 1",
            UserIden::Id.to_string(),
            UserIden::Username.to_string(),
            UserIden::TokenVersion.to_string(),
            UserIden::TwoFactorSecretCodeHash.to_string(),
            UserIden::TwoFactorCodeExpiresAt.to_string(),
            UserIden::TwoFactorCodeAttempts.to_string(),
            UserIden::TwoFactorLockedUntil.to_string(),
            UserIden::Table.to_string(),
            UserIden::Id.to_string()
        ))
    )
    .bind(body.user_id)
    .fetch_optional(db)
    .await
    .map_err(|_| ApiError::Internal("Database error"))?
    .ok_or(ApiError::NotFound("User not found"))?;

    let user_id: i64 = r.get(UserIden::Id.as_str());
    let username: String = r.get(UserIden::Username.as_str());
    let token_version: i64 = r.get(UserIden::TokenVersion.as_str());
    let stored_hash: Option<String> = r.get(UserIden::TwoFactorSecretCodeHash.as_str());
    let expires_at_dt: Option<chrono::DateTime<chrono::Utc>> = r.get(UserIden::TwoFactorCodeExpiresAt.as_str());
    let attempts: i64 = r.get(UserIden::TwoFactorCodeAttempts.as_str());
    let locked_until_dt: Option<chrono::DateTime<chrono::Utc>> = r.get(UserIden::TwoFactorLockedUntil.as_str());

    let stored_hash = stored_hash.ok_or(ApiError::NotFound("2FA not active"))?;

    if let Some(locked_until) = locked_until_dt {
        if chrono::Utc::now() < locked_until {
                let _ = sqlx::query(
                    sqlx::AssertSqlSafe(format!(
                        "INSERT INTO {} ({}, {}, {}, {}, {})
                         VALUES ($1, $2, $3, $4, $5)",
                        AuditLogIden::Table.to_string(),
                        AuditLogIden::UserId.to_string(),
                        AuditLogIden::Action.to_string(),
                        AuditLogIden::Status.to_string(),
                        AuditLogIden::Details.to_string(),
                        AuditLogIden::CreatedAt.to_string()
                    ))
                )
                .bind(user_id)
                .bind("2fa_verify")
                .bind("blocked_lockout")
                .bind("Account locked due to too many failed attempts")
                .bind(auth::now_iso())
                .execute(db)
                .await;
                return Err(ApiError::Forbidden("Account locked. Try again in 15 minutes"));
            }
        }

    if let Some(exp_at) = expires_at_dt {
        if chrono::Utc::now() > exp_at {
                let _ = sqlx::query(
                    sqlx::AssertSqlSafe(format!(
                        "INSERT INTO {} ({}, {}, {}, {}, {})
                         VALUES ($1, $2, $3, $4, $5)",
                        AuditLogIden::Table.to_string(),
                        AuditLogIden::UserId.to_string(),
                        AuditLogIden::Action.to_string(),
                        AuditLogIden::Status.to_string(),
                        AuditLogIden::Details.to_string(),
                        AuditLogIden::CreatedAt.to_string()
                    ))
                )
                .bind(user_id)
                .bind("2fa_verify")
                .bind("expired")
                .bind("2FA code expired")
                .bind(auth::now_iso())
                .execute(db)
                .await;
                return Err(ApiError::Unauthorized("2FA code expired. Please request a new one"));
            }
        }

    let code_matches = auth::constant_time_eq(&auth::sha256_hex(&body.code), &stored_hash);

    if !code_matches {
        let new_attempts = attempts + 1;

        if new_attempts >= 3 {
            let locked_until = chrono::Utc::now() + chrono::Duration::seconds(900);
            sqlx::query(
                sqlx::AssertSqlSafe(format!(
                    "UPDATE {}
                     SET {} = 3, {} = $1, {} = NULL, {} = NULL
                     WHERE {} = $2",
                    UserIden::Table.to_string(),
                    UserIden::TwoFactorCodeAttempts.to_string(),
                    UserIden::TwoFactorLockedUntil.to_string(),
                    UserIden::TwoFactorSecretCodeHash.to_string(),
                    UserIden::TwoFactorCodeSentAt.to_string(),
                    UserIden::Id.to_string())
                )
            )
            .bind(locked_until)
            .bind(user_id)
            .execute(db)
            .await
            .ok();

            let _ = sqlx::query(
                sqlx::AssertSqlSafe(format!(
                    "INSERT INTO {} ({}, {}, {}, {}, {})
                     VALUES ($1, $2, $3, $4, $5)",
                    AuditLogIden::Table.to_string(),
                    AuditLogIden::UserId.to_string(),
                    AuditLogIden::Action.to_string(),
                    AuditLogIden::Status.to_string(),
                    AuditLogIden::Details.to_string(),
                    AuditLogIden::CreatedAt.to_string())
                )
            )
            .bind(user_id)
            .bind("2fa_verify")
            .bind("locked")
            .bind("Account locked after 3 failed attempts")
            .bind(auth::now_iso())
            .execute(db)
            .await;

            return Err(ApiError::Forbidden("Too many failed attempts. Account locked for 15 minutes"));
        } else {
            sqlx::query(
                sqlx::AssertSqlSafe(format!(
                    "UPDATE {}
                     SET {} = $1
                     WHERE {} = $2",
                    UserIden::Table.to_string(),
                    UserIden::TwoFactorCodeAttempts.to_string(),
                    UserIden::Id.to_string())
                )
            )
            .bind(new_attempts)
            .bind(user_id)
            .execute(db)
            .await
            .ok();

            let _ = sqlx::query(
                sqlx::AssertSqlSafe(format!(
                    "INSERT INTO {} ({}, {}, {}, {}, {})
                     VALUES ($1, $2, $3, $4, $5)",
                    AuditLogIden::Table.to_string(),
                    AuditLogIden::UserId.to_string(),
                    AuditLogIden::Action.to_string(),
                    AuditLogIden::Status.to_string(),
                    AuditLogIden::Details.to_string(),
                    AuditLogIden::CreatedAt.to_string())
                )
            )
            .bind(user_id)
            .bind("2fa_verify")
            .bind("invalid_code")
            .bind(format!("Invalid 2FA code (attempt {}/3)", new_attempts))
            .bind(auth::now_iso())
            .execute(db)
            .await;
        }
        return Err(ApiError::Unauthorized("Invalid 2FA code"));
    }

    sqlx::query(
        sqlx::AssertSqlSafe(format!(
            "UPDATE {}
             SET {} = NULL, {} = NULL, {} = NULL, {} = 0
             WHERE {} = $1",
            UserIden::Table.to_string(),
            UserIden::TwoFactorSecretCodeHash.to_string(),
            UserIden::TwoFactorCodeSentAt.to_string(),
            UserIden::TwoFactorCodeExpiresAt.to_string(),
            UserIden::TwoFactorCodeAttempts.to_string(),
            UserIden::Id.to_string()
        ))
    )
    .bind(user_id)
    .execute(db)
    .await
    .map_err(|_| ApiError::Internal("Database error"))?;

    let token = auth::create_access_token(&username, token_version)
        .map_err(|_| ApiError::Internal("Token error"))?;

    let refresh_jti = Uuid::new_v4().to_string();
    let refresh = auth::create_refresh_token(&username, token_version, &refresh_jti)
        .map_err(|_| ApiError::Internal("Token error"))?;
    let refresh_hash = auth::sha256_hex(&refresh);
    let now = auth::now_iso();
    let refresh_claims = auth::decode_refresh_claims(&refresh)
        .map_err(|_| ApiError::Internal("Token error"))?;
    let expires_at = chrono::DateTime::from_timestamp(refresh_claims.exp, 0)
        .unwrap_or_else(chrono::Utc::now);

    let _ = sqlx::query(
        sqlx::AssertSqlSafe(format!(
            "INSERT INTO {} ({}, {}, {}, {}, {}, {}, {}, {})
             VALUES ($1, $2, NULL, NULL, $3, $4, $5, NULL)",
            RefreshSessionIden::Table.to_string(),
            RefreshSessionIden::UserId.to_string(),
            RefreshSessionIden::RefreshTokenHash.to_string(),
            RefreshSessionIden::UserAgent.to_string(),
            RefreshSessionIden::Ip.to_string(),
            RefreshSessionIden::CreatedAt.to_string(),
            RefreshSessionIden::LastUsedAt.to_string(),
            RefreshSessionIden::ExpiresAt.to_string(),
            RefreshSessionIden::RevokedAt.to_string()
        ))
    )
    .bind(user_id)
    .bind(&refresh_hash)
    .bind(now)
    .bind(now)
    .bind(expires_at)
    .execute(db)
    .await;

    let _ = sqlx::query(
        sqlx::AssertSqlSafe(format!(
            "INSERT INTO {} ({}, {}, {}, {})
             VALUES ($1, $2, $3, $4)",
            AuditLogIden::Table.to_string(),
            AuditLogIden::UserId.to_string(),
            AuditLogIden::Action.to_string(),
            AuditLogIden::Status.to_string(),
            AuditLogIden::CreatedAt.to_string()
        ))
    )
    .bind(user_id)
    .bind("2fa_verify_success")
    .bind("success")
    .bind(auth::now_iso())
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
    ).into_response())
}

async fn refresh(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
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

    let ip = rate_limit::extract_ip(&headers, Some(peer.ip()), st.trusted_proxies.as_slice())
        .unwrap_or_else(|| "unknown".to_string());
    if !rate_limit::allow(&format!("refresh:{}", ip), 60, 300) {
        return Err(ApiError::TooManyRequests("Too many refresh requests"));
    }

    let claims = auth::decode_refresh_claims(token)
        .map_err(|_| ApiError::Unauthorized("Invalid token"))?;

    let user_row = sqlx::query(
        sqlx::AssertSqlSafe(format!(
            "SELECT {}, {}, {}
             FROM {}
             WHERE {} = $1
             LIMIT 1",
            UserIden::Id.to_string(),
            UserIden::TokenVersion.to_string(),
            UserIden::IsBanned.to_string(),
            UserIden::Table.to_string(),
            UserIden::Username.to_string()
        ))
    )
    .bind(&claims.sub)
    .fetch_optional(&st.db)
    .await
    .map_err(|_| ApiError::Internal("Database error"))?
    .ok_or(ApiError::Unauthorized("Invalid token"))?;

    if user_row.get::<bool, _>(UserIden::IsBanned.as_str()) {
        return Err(ApiError::Forbidden("User banned"));
    }

    let user_id: i64 = user_row.get(UserIden::Id.as_str());
    let tv: i64 = user_row.get(UserIden::TokenVersion.as_str());
    if tv != claims.token_version {
        return Err(ApiError::Unauthorized("Token revoked"));
    }

    let now = auth::now_iso();

    let token_hash = auth::sha256_hex(token);

    let sess = sqlx::query(
        sqlx::AssertSqlSafe(format!(
            "SELECT {}, {}, {}
             FROM {}
             WHERE {} = $1 AND {} = $2
             LIMIT 1",
            RefreshSessionIden::Id.to_string(),
            RefreshSessionIden::RevokedAt.to_string(),
            RefreshSessionIden::ExpiresAt.to_string(),
            RefreshSessionIden::Table.to_string(),
            RefreshSessionIden::RefreshTokenHash.to_string(),
            RefreshSessionIden::UserId.to_string()
        ))
    )
    .bind(&token_hash)
    .bind(user_id)
    .fetch_optional(&st.db)
    .await
    .map_err(|_| ApiError::Internal("Database error"))?
    .ok_or(ApiError::Unauthorized("Invalid token"))?;

    let revoked_at: Option<chrono::DateTime<chrono::Utc>> = sess.get(RefreshSessionIden::RevokedAt.as_str());
    if revoked_at.is_some() {
        return Err(ApiError::Unauthorized("Invalid token"));
    }

    let expires_at: chrono::DateTime<chrono::Utc> = sess.get(RefreshSessionIden::ExpiresAt.as_str());
    if expires_at <= chrono::Utc::now() {
        let _ = sqlx::query(
            sqlx::AssertSqlSafe(format!(
                "UPDATE {}
                 SET {} = $1
                 WHERE {} = $2",
                RefreshSessionIden::Table.to_string(),
                RefreshSessionIden::RevokedAt.to_string(),
                RefreshSessionIden::RefreshTokenHash.to_string()
            ))
        )
        .bind(now)
        .bind(&token_hash)
        .execute(&st.db)
        .await;
        return Err(ApiError::Unauthorized("Invalid token"));
    }

    let _ = sqlx::query(
        sqlx::AssertSqlSafe(format!(
            "UPDATE {}
             SET {} = $1, {} = $2
             WHERE {} = $3",
            RefreshSessionIden::Table.to_string(),
            RefreshSessionIden::RevokedAt.to_string(),
            RefreshSessionIden::LastUsedAt.to_string(),
            RefreshSessionIden::RefreshTokenHash.to_string()
        ))
    )
    .bind(now)
    .bind(now)
    .bind(&token_hash)
    .execute(&st.db)
    .await;

    let access = auth::create_access_token(&claims.sub, tv)
        .map_err(|_| ApiError::Internal("Token error"))?;
    let access_hash = auth::sha256_hex(&access);

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
    let ip = rate_limit::extract_ip(&headers, Some(peer.ip()), st.trusted_proxies.as_slice());

    let expires_at_new = chrono::DateTime::from_timestamp(refresh_claims.exp, 0)
        .unwrap_or_else(chrono::Utc::now);

    let _ = sqlx::query(
        sqlx::AssertSqlSafe(format!(
            "INSERT INTO {} ({}, {}, {}, {}, {}, {}, {}, {})
             VALUES ($1, $2, $3, $4, $5, $6, $7, NULL)",
            RefreshSessionIden::Table.to_string(),
            RefreshSessionIden::UserId.to_string(),
            RefreshSessionIden::RefreshTokenHash.to_string(),
            RefreshSessionIden::UserAgent.to_string(),
            RefreshSessionIden::Ip.to_string(),
            RefreshSessionIden::CreatedAt.to_string(),
            RefreshSessionIden::LastUsedAt.to_string(),
            RefreshSessionIden::ExpiresAt.to_string(),
            RefreshSessionIden::RevokedAt.to_string()
        ))
    )
    .bind(user_id)
    .bind(&refresh_hash)
    .bind(ua.clone())
    .bind(ip.clone())
    .bind(now)
    .bind(now)
    .bind(expires_at_new)
    .execute(&st.db)
    .await;

    let _ = sqlx::query(
        sqlx::AssertSqlSafe(format!(
            "INSERT INTO {} ({}, {}, {}, {}, {}, {}, {})
             VALUES ($1, $2, $3, $4, $5, $6, NULL)
             ON CONFLICT ({}) DO NOTHING",
            UserSessionIden::Table.to_string(),
            UserSessionIden::UserId.to_string(),
            UserSessionIden::TokenHash.to_string(),
            UserSessionIden::UserAgent.to_string(),
            UserSessionIden::Ip.to_string(),
            UserSessionIden::CreatedAt.to_string(),
            UserSessionIden::LastSeenAt.to_string(),
            UserSessionIden::RevokedAt.to_string(),
            UserSessionIden::TokenHash.to_string()
        ))
    )
    .bind(user_id)
    .bind(&access_hash)
    .bind(ua)
    .bind(ip)
    .bind(now)
    .bind(now)
    .execute(&st.db)
    .await;

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "access_token": access,
            "refresh_token": refresh,
            "token_type": "bearer"
        })),
    ).into_response())
}

async fn logout(
    State(st): State<AppState>,
    user: AuthUser,
    body: Option<Json<LogoutBody>>,
) -> Result<Response, ApiError> {
    let now = auth::now_iso();

    let _ = sqlx::query(
        sqlx::AssertSqlSafe(format!(
            "UPDATE {}
             SET {} = $1
             WHERE {} = $2 AND {} = $3 AND {} IS NULL",
            UserSessionIden::Table.to_string(),
            UserSessionIden::RevokedAt.to_string(),
            UserSessionIden::UserId.to_string(),
            UserSessionIden::TokenHash.to_string(),
            UserSessionIden::RevokedAt.to_string()
        ))
    )
    .bind(now)
    .bind(user.id)
    .bind(&user.token_hash)
    .execute(&st.db)
    .await;

    let refresh_token = body.and_then(|Json(body)| body.refresh_token);
    if let Some(refresh_token) = refresh_token {
        let refresh_token = refresh_token.trim();
        if !refresh_token.is_empty() {
            let refresh_hash = auth::sha256_hex(refresh_token);
            let _ = sqlx::query(
                sqlx::AssertSqlSafe(format!(
                    "UPDATE {}
                     SET {} = $1
                     WHERE {} = $2 AND {} = $3 AND {} IS NULL",
                    RefreshSessionIden::Table.to_string(),
                    RefreshSessionIden::RevokedAt.to_string(),
                    RefreshSessionIden::UserId.to_string(),
                    RefreshSessionIden::RefreshTokenHash.to_string(),
                    RefreshSessionIden::RevokedAt.to_string())
                )
            )
            .bind(now)
            .bind(user.id)
            .bind(refresh_hash)
            .execute(&st.db)
            .await;
        }
    }

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({ "status": "ok" })),
    ).into_response())
}

async fn logout_all(
    State(st): State<AppState>,
    user: AuthUser,
) -> Result<Response, ApiError> {
    let res = sqlx::query(
        sqlx::AssertSqlSafe(format!(
            "UPDATE {}
             SET {} = {} + 1
             WHERE {} = $1",
            UserIden::Table.to_string(),
            UserIden::TokenVersion.to_string(),
            UserIden::TokenVersion.to_string(),
            UserIden::Id.to_string()
        ))
    )
    .bind(user.id)
    .execute(&st.db)
    .await
    .map_err(|_| ApiError::Internal("Database error"))?;

    let now = auth::now_iso();
    let _ = sqlx::query(
        sqlx::AssertSqlSafe(format!(
            "UPDATE {}
             SET {} = $1
             WHERE {} = $2 AND {} IS NULL",
            RefreshSessionIden::Table.to_string(),
            RefreshSessionIden::RevokedAt.to_string(),
            RefreshSessionIden::UserId.to_string(),
            RefreshSessionIden::RevokedAt.to_string()
        ))
    )
    .bind(now)
    .bind(user.id)
    .execute(&st.db)
    .await;

    if res.rows_affected() == 0 {
        return Err(ApiError::Internal("Logout failed"));
    }

    let _ = sqlx::query(
        sqlx::AssertSqlSafe(format!(
            "UPDATE {}
             SET {} = false, {} = $1
             WHERE {} = $2",
            UserPresenceIden::Table.to_string(),
            UserPresenceIden::IsOnline.to_string(),
            UserPresenceIden::UpdatedAt.to_string(),
            UserPresenceIden::UserId.to_string()
        ))
    )
    .bind(now)
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
    ).into_response())
}

pub async fn verify_token(
    _user: AuthUser,
) -> impl IntoResponse {
    StatusCode::OK.into_response()
}