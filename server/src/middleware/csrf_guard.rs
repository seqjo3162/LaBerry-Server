use axum::{
    extract::Request,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
use uuid::Uuid;
use sqlx::SqlitePool;
use chrono;
use crate::auth;

/// Generate a new CSRF token (UUID v4)
pub fn generate_csrf_token() -> String {
    Uuid::new_v4().to_string()
}

/// Hash CSRF token for storage
pub fn hash_csrf_token(token: &str) -> String {
    auth::sha256_hex(token)
}

/// Store CSRF token in database
pub async fn store_csrf_token(
    db: &SqlitePool,
    user_id: i64,
    token: &str,
    ttl_seconds: i64,
) -> Result<(), sqlx::Error> {
    let token_hash = hash_csrf_token(token);
    let now = auth::now_iso();
    let expires_at = auth::now_unix() + ttl_seconds;
    let expires_at_iso = chrono::DateTime::<chrono::Utc>::from_timestamp(expires_at, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| now.clone());

    sqlx::query(
        r#"
        INSERT INTO csrf_tokens(token_hash, user_id, created_at, expires_at)
        VALUES(?, ?, ?, ?)
        "#,
    )
    .bind(&token_hash)
    .bind(user_id)
    .bind(&now)
    .bind(&expires_at_iso)
    .execute(db)
    .await?;

    Ok(())
}

/// Validate CSRF token from request header
pub async fn validate_csrf_token(
    db: &SqlitePool,
    user_id: i64,
    token: &str,
) -> Result<bool, sqlx::Error> {
    let token_hash = hash_csrf_token(token);
    let now = auth::now_iso();

    // Clean expired tokens
    sqlx::query(
        r#"DELETE FROM csrf_tokens WHERE expires_at < ?"#
    )
    .bind(&now)
    .execute(db)
    .await.ok();

    // Check if token exists and is valid for user
    let result = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*) FROM csrf_tokens
        WHERE token_hash = ? AND user_id = ? AND expires_at > ?
        "#,
    )
    .bind(&token_hash)
    .bind(user_id)
    .bind(&now)
    .fetch_one(db)
    .await?;

    // Consume token (one-time use)
    if result > 0 {
        sqlx::query(
            r#"DELETE FROM csrf_tokens WHERE token_hash = ?"#
        )
        .bind(&token_hash)
        .execute(db)
        .await.ok();
        return Ok(true);
    }

    Ok(false)
}

/// CSRF guard middleware for state-changing operations
/// Checks X-CSRF-Token header against stored CSRF tokens
pub async fn csrf_guard(
    headers: HeaderMap,
    mut req: Request,
    next: Next,
) -> Result<Response, (StatusCode, &'static str)> {
    // Only check POST, PUT, DELETE, PATCH requests
    match *req.method() {
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS => {
            // Safe methods, skip CSRF check
            return Ok(next.run(req).await);
        }
        _ => {}
    }

    // Skip CSRF for public endpoints (auth routes)
    let path = req.uri().path();
    if path.starts_with("/api/auth/login") 
        || path.starts_with("/api/auth/register")
        || path.starts_with("/api/auth/refresh")
        || path.starts_with("/api/auth/verify-2fa")
        || path.starts_with("/verify")
        || path.starts_with("/ws")
    {
        // These endpoints have their own security (2FA, rate limiting, token validation)
        return Ok(next.run(req).await);
    }

    // Check for CSRF token in header
    let csrf_token = headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .ok_or((StatusCode::FORBIDDEN, "Missing CSRF token (X-CSRF-Token header required)"))?;

    // Token validation will be done in the route handler
    // This middleware just ensures the header is present
    req.extensions_mut().insert(csrf_token.to_string());

    Ok(next.run(req).await)
}
