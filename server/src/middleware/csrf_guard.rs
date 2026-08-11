use axum::{
    extract::Request,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
use uuid::Uuid;
use sqlx::PgPool;
use chrono;
use crate::auth;

pub fn generate_csrf_token() -> String {
    Uuid::new_v4().to_string()
}

pub fn hash_csrf_token(token: &str) -> String {
    auth::sha256_hex(token)
}

pub async fn store_csrf_token(
    db: &PgPool,
    user_id: i64,
    token: &str,
    ttl_seconds: i64,
) -> Result<(), sqlx::Error> {
    let token_hash = hash_csrf_token(token);
    let now = auth::now_iso();
    let expires_at = now + chrono::Duration::seconds(ttl_seconds);

    sqlx::query(
        r#"
        INSERT INTO csrf_tokens(token_hash, user_id, created_at, expires_at)
        VALUES($1, $2, $3, $4)
        "#,
    )
    .bind(&token_hash)
    .bind(user_id)
    .bind(&now)
    .bind(&expires_at)
    .execute(db)
    .await?;

    Ok(())
}

pub async fn validate_csrf_token(
    db: &PgPool,
    user_id: i64,
    token: &str,
) -> Result<bool, sqlx::Error> {
    let token_hash = hash_csrf_token(token);
    let now = auth::now_iso();

    sqlx::query(
        r#"DELETE FROM csrf_tokens WHERE expires_at < $1"#
    )
    .bind(&now)
    .execute(db)
    .await.ok();

    let result = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*) FROM csrf_tokens
        WHERE token_hash = $1 AND user_id = $2 AND expires_at > $3
        "#,
    )
    .bind(&token_hash)
    .bind(user_id)
    .bind(&now)
    .fetch_one(db)
    .await?;

    if result > 0 {
        sqlx::query(
            r#"DELETE FROM csrf_tokens WHERE token_hash = $1"#
        )
        .bind(&token_hash)
        .execute(db)
        .await.ok();
        return Ok(true);
    }

    Ok(false)
}

pub async fn csrf_guard(
    headers: HeaderMap,
    mut req: Request,
    next: Next,
) -> Result<Response, (StatusCode, &'static str)> {
    match *req.method() {
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS => {
            return Ok(next.run(req).await);
        }
        _ => {}
    }

    let path = req.uri().path();
    
    if path.starts_with("/api/") {
        return Ok(next.run(req).await);
    }
    
    if path.starts_with("/ws") {
        return Ok(next.run(req).await);
    }
    
    if path.starts_with("/admin") || path.starts_with("/admin-panel/") {
        return Ok(next.run(req).await);
    }
    
    if path.starts_with("/api/auth/login")
        || path.starts_with("/api/auth/register")
        || path.starts_with("/api/auth/refresh")
        || path.starts_with("/api/auth/verify-2fa")
        || path.starts_with("/verify")
    {
        return Ok(next.run(req).await);
    }

    let csrf_token = headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .ok_or((StatusCode::FORBIDDEN, "Missing CSRF token (X-CSRF-Token header required)"))?;

    req.extensions_mut().insert(csrf_token.to_string());

    Ok(next.run(req).await)
}
