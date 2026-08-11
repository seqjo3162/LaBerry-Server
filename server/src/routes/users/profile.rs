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

use super::settings;
use super::{
    default_settings,
    sanitize_about, sanitize_color, sanitize_email, sanitize_status_text,
    UpdateMeBody, UserConnection, UserMeResponse,
};

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct UserProfileView {
    pub user_id: i64,
    pub avatar_file_id: Option<i64>,
    pub banner_file_id: Option<i64>,
    pub accent_color: Option<String>,
    pub about: Option<String>,
    pub status_text: Option<String>,
    pub integrations: serde_json::Value,
    pub updated_at: String,
}

#[derive(Serialize, Clone)]
pub struct PublicProfileView {
    pub user_id: i64,
    pub username: String,
    pub display_name: String,
    pub created_at: String,
    pub status: String,
    pub is_online: bool,
    pub avatar_file_id: Option<i64>,
    pub banner_file_id: Option<i64>,
    pub accent_color: Option<String>,
    pub about: Option<String>,
    pub status_text: Option<String>,
    pub integrations: serde_json::Value,
    pub connections: Vec<UserConnection>,
    pub updated_at: String,
}

#[derive(Deserialize, Default)]
pub struct UpdateProfileBody {
    pub avatar_file_id: Option<i64>,
    pub banner_file_id: Option<i64>,
    pub accent_color: Option<String>,
    pub about: Option<String>,
    pub status_text: Option<String>,
    pub integrations: Option<serde_json::Value>,
}

pub async fn me(
    State(st): State<AppState>,
    me: AuthUser,
) -> impl IntoResponse {
    let db = &st.db;

    let row = sqlx::query(
        r#"SELECT id,
                  username,
                  email,
                  email_verified,
                  email_pending,
                  public_encryption_key,
                  COALESCE(cookie_consent_status, 'unknown') AS cookie_consent_status,
                  cookie_consent_at,
                  COALESCE(trust_factor, 100) AS trust_factor,
                  COALESCE(trust_review_status, 'clear') AS trust_review_status,
                  trust_review_reason
           FROM users WHERE id = $1 LIMIT 1"#,
    )
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(r) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let u = UserMeResponse {
        id: r.get("id"),
        username: r.get("username"),
        email: r.get("email"),
        email_verified: r.get::<bool, _>("email_verified"),
        email_pending: r.get("email_pending"),
        public_encryption_key: r.get("public_encryption_key"),
        cookie_consent_status: r.get("cookie_consent_status"),
        cookie_consent_at: r.try_get::<chrono::DateTime<chrono::Utc>, _>("cookie_consent_at").ok().map(|d| d.to_rfc3339()),
        trust_factor: r.get("trust_factor"),
        trust_review_status: r.get("trust_review_status"),
        trust_review_reason: r.get("trust_review_reason"),
    };

    (StatusCode::OK, Json(u)).into_response()
}

pub async fn update_me(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<UpdateMeBody>,
) -> impl IntoResponse {
    let db = &st.db;

    if let Some(email_raw) = body.email {
        let Some(email) = sanitize_email(&email_raw) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"detail":"Invalid email"})),
            )
                .into_response();
        };

        let q = sqlx::query(
            r#"UPDATE users
               SET email_pending = $1, email_verified = FALSE
               WHERE id = $2"#,
        )
        .bind(email)
        .bind(me.id)
        .execute(db)
        .await;

        if q.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    // Валидация и сохранение публичного E2EE ключа
    if let Some(ref pub_key) = body.public_encryption_key {
        use crate::crypto::e2ee::E2eeKeyPair;
        // Валидируем что это корректный base64-encoded X25519 public key (32 bytes)
        if E2eeKeyPair::from_public_b64(pub_key).is_none() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"detail": "Invalid public encryption key. Must be base64-encoded 32-byte X25519 public key."})),
            ).into_response();
        }
    }
    
    if sqlx::query(
        r#"UPDATE users
           SET public_encryption_key = $1
           WHERE id = $2"#,
    )
    .bind(body.public_encryption_key)
    .bind(me.id)
    .execute(db)
    .await
    .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let row = sqlx::query(
        r#"SELECT id,
                  username,
                  email,
                  email_verified,
                  email_pending,
                  public_encryption_key,
                  COALESCE(cookie_consent_status, 'unknown') AS cookie_consent_status,
                  cookie_consent_at,
                  COALESCE(trust_factor, 100) AS trust_factor,
                  COALESCE(trust_review_status, 'clear') AS trust_review_status,
                  trust_review_reason
           FROM users WHERE id = $1 LIMIT 1"#,
    )
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(r) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let resp = UserMeResponse {
        id: r.get("id"),
        username: r.get("username"),
        email: r.get("email"),
        email_verified: r.get::<bool, _>("email_verified"),
        email_pending: r.get("email_pending"),
        public_encryption_key: r.get("public_encryption_key"),
        cookie_consent_status: r.get("cookie_consent_status"),
        cookie_consent_at: r.try_get::<chrono::DateTime<chrono::Utc>, _>("cookie_consent_at").ok().map(|d| d.to_rfc3339()),
        trust_factor: r.get("trust_factor"),
        trust_review_status: r.get("trust_review_status"),
        trust_review_reason: r.get("trust_review_reason"),
    };

    (StatusCode::OK, Json(resp)).into_response()
}

async fn get_or_create_profile(db: &sqlx::PgPool, user_id: i64) -> UserProfileView {
    let row = sqlx::query(
        r#"SELECT user_id, avatar_file_id, banner_file_id, accent_color, about, status_text, integrations_json, updated_at
           FROM user_profile WHERE user_id = $1 LIMIT 1"#,
    )
    .bind(user_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    if let Some(r) = row {
        let raw: String = r.get("integrations_json");
        let integrations = serde_json::from_str::<serde_json::Value>(&raw)
            .unwrap_or_else(|_| serde_json::json!({}));
        return UserProfileView {
            user_id: r.get("user_id"),
            avatar_file_id: r.try_get("avatar_file_id").ok(),
            banner_file_id: r.try_get("banner_file_id").ok(),
            accent_color: r.try_get("accent_color").ok(),
            about: r.try_get("about").ok(),
            status_text: r.try_get("status_text").ok(),
            integrations,
            updated_at: r.get::<chrono::DateTime<chrono::Utc>, _>("updated_at").to_rfc3339(),
        };
    }

    let now = auth::now_iso();
    let _ = sqlx::query(
        r#"INSERT INTO user_profile(user_id, integrations_json, updated_at) VALUES($1, '{}', $2)"#,
    )
    .bind(user_id)
    .bind(now)
    .execute(db)
    .await;

    UserProfileView {
        user_id,
        avatar_file_id: None,
        banner_file_id: None,
        accent_color: None,
        about: None,
        status_text: None,
        integrations: serde_json::json!({}),
        updated_at: now.to_rfc3339(),
    }
}

async fn get_public_profile(db: &sqlx::PgPool, user_id: i64) -> Option<PublicProfileView> {
    let user = sqlx::query("SELECT username, is_banned, created_at FROM users WHERE id = $1 LIMIT 1")
        .bind(user_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()?;

    if user.get::<bool, _>("is_banned") {
        return None;
    }

    let username: String = user.get("username");
    let created_at: String = user.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339();
    let profile = get_or_create_profile(db, user_id).await;
    let settings = settings::load_user_settings(db, user_id).await.unwrap_or_else(default_settings);

    let presence = sqlx::query("SELECT status, is_online FROM user_presence WHERE user_id = $1 LIMIT 1")
        .bind(user_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let mut status = presence
        .as_ref()
        .map(|r| r.get::<String, _>("status"))
        .unwrap_or_else(|| "offline".to_string());
    let mut is_online = presence
        .as_ref()
        .map(|r| r.get::<bool, _>("is_online"))
        .unwrap_or(false);

    if status == "invisible" || !is_online {
        status = "offline".to_string();
        is_online = false;
    }

    Some(PublicProfileView {
        user_id,
        username: username.clone(),
        display_name: username,
        created_at,
        status,
        is_online,
        avatar_file_id: profile.avatar_file_id,
        banner_file_id: profile.banner_file_id,
        accent_color: profile.accent_color,
        about: profile.about,
        status_text: profile.status_text,
        integrations: profile.integrations,
        connections: settings.connections,
        updated_at: profile.updated_at,
    })
}

pub async fn get_my_profile(State(st): State<AppState>, me: AuthUser) -> impl IntoResponse {
    match get_public_profile(&st.db, me.id).await {
        Some(v) => (StatusCode::OK, Json(v)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn update_my_profile(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<UpdateProfileBody>,
) -> impl IntoResponse {
    let db = &st.db;

    let existing = get_or_create_profile(db, me.id).await;

    let now = auth::now_iso();

    let avatar_file_id = body.avatar_file_id.or(existing.avatar_file_id);
    let banner_file_id = body.banner_file_id.or(existing.banner_file_id);

    let accent_color = match body.accent_color {
        Some(v) => sanitize_color(Some(v)),
        None => existing.accent_color.clone(),
    };
    let about = match body.about {
        Some(v) => sanitize_about(Some(v)),
        None => existing.about.clone(),
    };
    let status_text = match body.status_text {
        Some(v) => sanitize_status_text(Some(v)),
        None => existing.status_text.clone(),
    };

    let integrations_json = if let Some(v) = body.integrations {
        let s = serde_json::to_string(&v).unwrap_or_else(|_| "{}".to_string());
        if s.len() > 4096 { "{}".to_string() } else { s }
    } else {
        let s = serde_json::to_string(&existing.integrations).unwrap_or_else(|_| "{}".to_string());
        if s.len() > 4096 { "{}".to_string() } else { s }
    };

    let q = sqlx::query(
        r#"
        UPDATE user_profile
        SET avatar_file_id = $1,
            banner_file_id = $2,
            accent_color = $3,
            about = $4,
            status_text = $5,
            integrations_json = $6,
            updated_at = $7
        WHERE user_id = $8
        "#,
    )
    .bind(avatar_file_id)
    .bind(banner_file_id)
    .bind(accent_color)
    .bind(about)
    .bind(status_text)
    .bind(integrations_json)
    .bind(now)
    .bind(me.id)
    .execute(db)
    .await;

    if q.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let v = get_or_create_profile(db, me.id).await;
    (StatusCode::OK, Json(v)).into_response()
}

pub async fn get_profile_by_id(State(st): State<AppState>, _me: AuthUser, Path(id): Path<i64>) -> impl IntoResponse {
    let banned = sqlx::query_scalar::<_, bool>("SELECT is_banned FROM users WHERE id = $1 LIMIT 1")
        .bind(id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten();

    let Some(b) = banned else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if b {
        return StatusCode::FORBIDDEN.into_response();
    }

    match get_public_profile(&st.db, id).await {
        Some(v) => (StatusCode::OK, Json(v)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
