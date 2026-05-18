use axum::{
    async_trait,
    extract::FromRequestParts,
    http::{request::Parts, header},
};
use sqlx::Row;

use crate::{
    api_error::ApiError,
    auth,
    server::AppState,
};

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: i64,
    pub username: String,
    pub role: String,
    pub token_hash: String,
}

#[derive(Debug, Clone)]
pub struct AuthAdmin {
    pub id: i64,
    pub username: String,
}

#[async_trait]
impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // 1. Если пользователь уже есть в extensions — берём оттуда
        if let Some(user) = parts.extensions.get::<AuthUser>() {
            return Ok(user.clone());
        }

        // 2. Читаем Authorization header
        let auth_header = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .ok_or(ApiError::Unauthorized("Missing Authorization header"))?;

        let token = auth_header
            .strip_prefix("Bearer ")
            .ok_or(ApiError::Unauthorized("Invalid Authorization scheme"))?;

        let token_hash = auth::sha256_hex(token);

        let (username, token_version) =
            auth::decode_username(token)
                .map_err(|_| ApiError::Unauthorized("Invalid or expired token"))?;

        let row = sqlx::query(
            r#"
            SELECT
                id,
                token_version,
                is_banned,
                'user' AS role
            FROM users
            WHERE username = ?
            LIMIT 1
            "#,
        )
        .bind(&username)
        .fetch_optional(&state.db)
        .await
        .map_err(|_| ApiError::Internal("Database error"))?
        .ok_or(ApiError::Unauthorized("User not found"))?;

        if row.get::<i64, _>("is_banned") != 0 {
            return Err(ApiError::Forbidden("User banned"));
        }

        let db_version: i64 = row.get("token_version");
        if db_version != token_version {
            return Err(ApiError::Unauthorized("Token invalidated"));
        }

        let user = AuthUser {
            id: row.get("id"),
            username,
            role: row.get("role"),
            token_hash: token_hash.clone(),
        };

        // sessions (best-effort)
        let now = auth::now_iso();

        if let Ok(Some(revoked_at)) = sqlx::query_scalar::<_, Option<String>>(
            "SELECT revoked_at FROM user_sessions WHERE token_hash = ? LIMIT 1",
        )
        .bind(&token_hash)
        .fetch_optional(&state.db)
        .await
        {
            if revoked_at.is_some() {
                return Err(ApiError::Unauthorized("Session revoked"));
            }
        }

        let _ = sqlx::query("UPDATE user_sessions SET last_seen_at = ? WHERE token_hash = ?")
            .bind(&now)
            .bind(&token_hash)
            .execute(&state.db)
            .await;

        // 3. Кладём в extensions, чтобы не валидировать повторно
        parts.extensions.insert(user.clone());

        Ok(user)
    }
}

#[async_trait]
impl FromRequestParts<AppState> for AuthAdmin {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;

        if user.role != "admin" {
            return Err(ApiError::Forbidden("Admin access required"));
        }

        Ok(AuthAdmin {
            id: user.id,
            username: user.username,
        })
    }
}
