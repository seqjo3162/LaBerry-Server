use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{server::AppState, auth};
use crate::middleware::auth_guard::AuthUser;
use crate::middleware::rate_limit;

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct UserConnection {
    pub kind: String,
    pub url: String,
    pub label: Option<String>,
}

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
    pub developer_mode: bool,
}

impl Default for UserSettings {
    fn default() -> Self {
        default_settings()
    }
}

impl Default for UserConnection {
    fn default() -> Self {
        UserConnection {
            kind: "website".to_string(),
            url: "".to_string(),
            label: None,
        }
    }
}

fn default_settings() -> UserSettings {
    UserSettings {
        theme: "dark".to_string(),
        locale: "ru".to_string(),
        show_header_status: false,
        compact_mode: false,
        show_timestamps: true,
        font_scale: 1.0,
        connections: vec![],
        friend_requests: "everyone".to_string(),
        dms: "friends_and_server".to_string(),
        notify_desktop: true,
        notify_sounds: true,
        notify_dms: true,
        notify_mentions: true,
        developer_mode: false,
    }
}

fn sanitize_theme(s: &str) -> String {
    match s.to_ascii_lowercase().as_str() {
        "light" => "light".to_string(),
        _ => "dark".to_string(),
    }
}

fn sanitize_locale(s: &str) -> String {
    match s.to_ascii_lowercase().as_str() {
        "en" => "en".to_string(),
        _ => "ru".to_string(),
    }
}

fn sanitize_friend_requests(s: &str) -> String {
    match s.to_ascii_lowercase().as_str() {
        "everyone" => "everyone".to_string(),
        "friends_of_friends" => "friends_of_friends".to_string(),
        "server_members" => "server_members".to_string(),
        "none" => "none".to_string(),
        _ => "everyone".to_string(),
    }
}

fn sanitize_dms(s: &str) -> String {
    match s.to_ascii_lowercase().as_str() {
        "friends_only" => "friends_only".to_string(),
        "friends_and_server" => "friends_and_server".to_string(),
        "everyone" => "everyone".to_string(),
        _ => "friends_and_server".to_string(),
    }
}

fn sanitize_connection_kind(s: &str) -> String {
    match s.to_ascii_lowercase().as_str() {
        "discord" => "discord".to_string(),
        "telegram" => "telegram".to_string(),
        "github" => "github".to_string(),
        "youtube" => "youtube".to_string(),
        "twitch" => "twitch".to_string(),
        "website" => "website".to_string(),
        "other" => "other".to_string(),
        _ => "other".to_string(),
    }
}

fn sanitize_connection_url(url: &str) -> Option<String> {
    let u = url.trim();
    if u.is_empty() {
        return None;
    }
    let mut out = u.to_string();
    if !out.starts_with("http://") && !out.starts_with("https://") {
        out = format!("https://{}", out);
    }
    if out.len() > 2048 {
        out.truncate(2048);
    }
    if !(out.starts_with("http://") || out.starts_with("https://")) {
        return None;
    }
    Some(out)
}

fn sanitize_connections(list: Vec<UserConnection>) -> Vec<UserConnection> {
    let mut out = Vec::new();

    for c in list.into_iter() {
        if out.len() >= 12 {
            break;
        }

        let kind = sanitize_connection_kind(&c.kind);
        let url = match sanitize_connection_url(&c.url) {
            Some(v) => v,
            None => continue,
        };

        let label = c
            .label
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .map(|mut x| {
                if x.len() > 64 {
                    x.truncate(64);
                }
                x
            });

        out.push(UserConnection { kind, url, label });
    }

    out
}

fn sanitize_settings(mut s: UserSettings) -> UserSettings {
    s.theme = sanitize_theme(&s.theme);
    s.locale = sanitize_locale(&s.locale);
    s.friend_requests = sanitize_friend_requests(&s.friend_requests);
    s.dms = sanitize_dms(&s.dms);

    if !(0.8..=1.3).contains(&s.font_scale) {
        s.font_scale = 1.0;
    }

    s.connections = sanitize_connections(s.connections);
    s
}

#[derive(Serialize)]
pub struct UserPublic {
    pub id: i64,
    pub username: String,
    pub public_encryption_key: Option<String>,
}

#[derive(Serialize)]
pub struct UserMeResponse {
    pub id: i64,
    pub username: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub email_pending: Option<String>,
    pub public_encryption_key: Option<String>,
}

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

#[derive(Deserialize, Default)]
pub struct UpdateProfileBody {
    pub avatar_file_id: Option<i64>,
    pub banner_file_id: Option<i64>,
    pub accent_color: Option<String>,
    pub about: Option<String>,
    pub status_text: Option<String>,
    pub integrations: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub struct UpdateMeBody {
    pub email: Option<String>,
    pub public_encryption_key: Option<String>,
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub query: String,
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

fn sanitize_status(s: &str) -> String {
    match s.to_ascii_lowercase().as_str() {
        "online" => "online".to_string(),
        "idle" => "idle".to_string(),
        "dnd" => "dnd".to_string(),
        "invisible" => "invisible".to_string(),
        _ => "online".to_string(),
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me", get(me).put(update_me))
        .route("/me/email/request_code", post(request_email_code))
        .route("/me/email/confirm_code", post(confirm_email_code))
        .route("/me/profile", get(get_my_profile).put(update_my_profile))
        .route("/me/delete", post(delete_me))
        .route("/me/blocks", get(list_blocks))
        .route("/me/blocks/:user_id", put(block_user).delete(unblock_user))
        .route("/me/status", get(my_status).put(set_my_status))
        .route("/me/settings", get(get_my_settings).put(update_my_settings))
        .route("/me/password", put(change_password))
        .route("/", get(list_users))
        .route("/search", get(search))
        .route("/:id/profile", get(get_profile_by_id))
        .route("/:id", get(get_by_id))
}

fn sanitize_email(s: &str) -> Option<String> {
    let e = s.trim();
    if e.is_empty() {
        return None;
    }
    if e.len() > 254 {
        return None;
    }
    if !e.contains('@') || !e.contains('.') {
        return None;
    }
    Some(e.to_ascii_lowercase())
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
}

#[derive(Deserialize)]
pub struct ConfirmEmailCodeBody {
    pub code: String,
    pub purpose: Option<String>,
}

fn sanitize_email_purpose(p: Option<String>) -> String {
    match p.unwrap_or_else(|| "verify_email".to_string()).to_ascii_lowercase().as_str() {
        "change_email" => "change_email".to_string(),
        _ => "verify_email".to_string(),
    }
}

async fn get_my_settings(
    State(st): State<AppState>,
    me: AuthUser,
) -> impl IntoResponse {
    let db = &st.db;

    let row = sqlx::query("SELECT settings_json FROM user_settings WHERE user_id = ? LIMIT 1")
        .bind(me.id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    if let Some(r) = row {
        let raw: String = r.get("settings_json");
        if let Ok(v) = serde_json::from_str::<UserSettings>(&raw) {
            return (StatusCode::OK, Json(sanitize_settings(v))).into_response();
        }
    }

    let def = default_settings();
    let now = auth::now_iso();
    let raw = serde_json::to_string(&def).unwrap_or_else(|_| "{}".to_string());
    let _ = sqlx::query(
        r#"
        INSERT INTO user_settings(user_id, settings_json, updated_at)
        VALUES(?, ?, ?)
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

async fn update_my_settings(
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
        VALUES(?, ?, ?)
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

#[derive(Deserialize)]
pub struct ChangePasswordBody {
    pub old_password: String,
    pub new_password: String,
}

async fn change_password(
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

    let row = sqlx::query("SELECT password_hash FROM users WHERE id = ? LIMIT 1")
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
        SET password_hash = ?, token_version = token_version + 1
        WHERE id = ?
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

async fn my_status(
    State(st): State<AppState>,
    me: AuthUser,
) -> impl IntoResponse {
    let db = &st.db;

    let row = sqlx::query(
        r#"SELECT status, is_online, updated_at FROM user_presence WHERE user_id = ? LIMIT 1"#,
    )
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    if let Some(r) = row {
        let status: String = r.get::<String, _>("status");
        let is_online = r.get::<i64, _>("is_online") != 0;
        let updated_at: Option<String> = r.get("updated_at");
        return (StatusCode::OK, Json(MyStatus { status, is_online, updated_at })).into_response();
    }

    let now = auth::now_iso();
    let _ = sqlx::query(
        "INSERT INTO user_presence(user_id, is_online, status, updated_at) VALUES(?, 0, 'online', ?)",
    )
    .bind(me.id)
    .bind(&now)
    .execute(db)
    .await;

    (StatusCode::OK, Json(MyStatus { status: "online".to_string(), is_online: false, updated_at: Some(now) })).into_response()
}

async fn set_my_status(
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
        VALUES(?, 0, ?, ?)
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

    let is_online = sqlx::query_scalar::<_, i64>(
        "SELECT is_online FROM user_presence WHERE user_id = ? LIMIT 1",
    )
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .unwrap_or(0) != 0;

    (StatusCode::OK, Json(MyStatus { status, is_online, updated_at: Some(now) })).into_response()
}

async fn me(
    State(st): State<AppState>,
    me: AuthUser,
) -> impl IntoResponse {
    let db = &st.db;

    let row = sqlx::query(
        r#"SELECT id, username, email, email_verified, email_pending, public_encryption_key
           FROM users WHERE id = ? LIMIT 1"#,
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
        email_verified: r.get::<i64, _>("email_verified") != 0,
        email_pending: r.get("email_pending"),
        public_encryption_key: r.get("public_encryption_key"),
    };

    (StatusCode::OK, Json(u)).into_response()
}

async fn update_me(
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
               SET email_pending = ?, email_verified = 0
               WHERE id = ?"#,
        )
        .bind(email)
        .bind(me.id)
        .execute(db)
        .await;

        if q.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    if sqlx::query(
        r#"UPDATE users
           SET public_encryption_key = ?
           WHERE id = ?"#,
    )
    .bind(body.public_encryption_key)
    .bind(me.id)
    .execute(db)
    .await
    .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
}

async fn request_email_code(
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
        "SELECT 1 FROM users WHERE email = ? AND id != ? LIMIT 1",
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

    let q = sqlx::query("UPDATE users SET email_pending = ?, email_verified = 0 WHERE id = ?")
        .bind(&email)
        .bind(me.id)
        .execute(db)
        .await;
    if q.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let now = auth::now_unix();
    let now_s = now.to_string();
    let _ = sqlx::query(
        "UPDATE email_codes SET consumed_at = ? WHERE user_id = ? AND purpose = ? AND consumed_at IS NULL",
    )
    .bind(&now_s)
    .bind(me.id)
    .bind(&purpose)
    .execute(db)
    .await;

    let code = auth::generate_2fa_code_6();
    let code_hash = auth::sha256_hex(&format!("{}:{}:{}", me.id, &purpose, &code));
    let expires_at = (now + 10 * 60).to_string();

    let ins = sqlx::query(
        r#"
        INSERT INTO email_codes(user_id, purpose, code_hash, sent_to_email, created_at, expires_at)
        VALUES(?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(me.id)
    .bind(&purpose)
    .bind(&code_hash)
    .bind(&email)
    .bind(&now_s)
    .bind(&expires_at)
    .execute(db)
    .await;

    if ins.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let debug = std::env::var("LB_DEBUG_EMAIL_CODES")
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(false);

    let resp = RequestEmailCodeResp {
        ok: true,
        debug_code: if debug { Some(code) } else { None },
        expires_in_sec: 10 * 60,
};
    (StatusCode::OK, Json(resp)).into_response()
}

async fn confirm_email_code(
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


let rl_key = format!("email_code:{}:{}", me.id, purpose);
if !rate_limit::allow(&rl_key, 5, 3600) {
    return (
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({"detail":"Too many requests"})),
    )
        .into_response();
}
    let now = auth::now_unix();
    let now_s = now.to_string();
    let want_hash = auth::sha256_hex(&format!("{}:{}:{}", me.id, &purpose, code));
    let row = sqlx::query(
        r#"
        SELECT id, code_hash, expires_at
        FROM email_codes
        WHERE user_id = ? AND purpose = ? AND consumed_at IS NULL
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
    let expires_at: String = r.get("expires_at");
    let exp = expires_at.parse::<i64>().unwrap_or(0);
    if exp <= now {
        let _ = sqlx::query("UPDATE email_codes SET consumed_at = ? WHERE id = ?")
            .bind(&now_s)
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

    let _ = sqlx::query("UPDATE email_codes SET consumed_at = ? WHERE id = ?")
        .bind(&now_s)
        .bind(code_id)
        .execute(db)
        .await;
    let pending: Option<String> = sqlx::query_scalar("SELECT email_pending FROM users WHERE id = ? LIMIT 1",)
        .bind(me.id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let Some(pending_email) = pending.and_then(|x| sanitize_email(&x)) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"detail":"No pending email"})),
        )
            .into_response();
    };

    let taken = sqlx::query_scalar::<_, i64>("SELECT 1 FROM users WHERE email = ? AND id != ? LIMIT 1",)
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

    let q = sqlx::query("UPDATE users SET email = ?, email_verified = 1, email_pending = NULL WHERE id = ?",)
    .bind(&pending_email)
    .bind(me.id)
    .execute(db)
    .await;

    if q.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    (StatusCode::OK, Json(serde_json::json!({"ok":true}))).into_response()
}

async fn list_users(
    State(st): State<AppState>,
    _me: AuthUser,
) -> impl IntoResponse {
    let db = &st.db;

    tracing::info!("list_users: start");

    let rows = sqlx::query(
        r#"SELECT id, username, public_encryption_key
           FROM users
           ORDER BY id DESC
           LIMIT 200"#,
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();

    tracing::info!("list_users: after db");
    
    let users: Vec<UserPublic> = rows
        .into_iter()
        .map(|r| UserPublic {
            id: r.get("id"),
            username: r.get("username"),
            public_encryption_key: r.get("public_encryption_key"),
        })
        .collect();

    (StatusCode::OK, Json(users)).into_response()
}

async fn search(
    State(st): State<AppState>,
    me: AuthUser,
    Query(q): Query<SearchQuery>,
) -> impl IntoResponse {
    let db = &st.db;

    let needle = q.query.trim();
    if needle.is_empty() {
        return (StatusCode::OK, Json(Vec::<UserPublic>::new())).into_response();
    }

    let pat = format!("%{}%", needle);

    let rows = sqlx::query(
        r#"SELECT u.id, u.username, u.public_encryption_key
           FROM users u
           WHERE u.username LIKE ?
             AND u.id != ?
              AND NOT EXISTS (
                SELECT 1 FROM user_blocks b
                WHERE (b.blocker_id = ? AND b.blocked_id = u.id)
                   OR (b.blocker_id = u.id AND b.blocked_id = ?)
              )
             AND NOT EXISTS (
               SELECT 1
               FROM friendships f
               WHERE (f.user_id = ? AND f.friend_id = u.id)
                  OR (f.user_id = u.id AND f.friend_id = ?)
             )
             AND NOT EXISTS (
               SELECT 1
               FROM friend_requests fr
               WHERE fr.status = 'pending'
                 AND ((fr.sender_id = ? AND fr.receiver_id = u.id)
                   OR (fr.sender_id = u.id AND fr.receiver_id = ?))
             )
           ORDER BY u.id DESC
           LIMIT 50"#,
    )
    .bind(pat)
    .bind(me.id)
    .bind(me.id)
    .bind(me.id)
    .bind(me.id)
    .bind(me.id)
    .bind(me.id)
    .bind(me.id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let users: Vec<UserPublic> = rows
        .into_iter()
        .map(|r| UserPublic {
            id: r.get("id"),
            username: r.get("username"),
            public_encryption_key: r.get("public_encryption_key"),
        })
        .collect();

    (StatusCode::OK, Json(users)).into_response()
}

async fn get_by_id(
    State(st): State<AppState>,
    _me: AuthUser,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    let row = sqlx::query(
        r#"SELECT id, username, public_encryption_key
           FROM users WHERE id = ? LIMIT 1"#,
    )
    .bind(id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(r) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let u = UserPublic {
        id: r.get("id"),
        username: r.get("username"),
        public_encryption_key: r.get("public_encryption_key"),
    };

    (StatusCode::OK, Json(u)).into_response()
}

async fn get_or_create_profile(db: &sqlx::SqlitePool, user_id: i64) -> UserProfileView {
    let row = sqlx::query(
        r#"SELECT user_id, avatar_file_id, banner_file_id, accent_color, about, status_text, integrations_json, updated_at
           FROM user_profile WHERE user_id = ? LIMIT 1"#,
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
            updated_at: r.get("updated_at"),
        };
    }

    let now = auth::now_iso();
    let _ = sqlx::query(
        r#"INSERT INTO user_profile(user_id, integrations_json, updated_at) VALUES(?, '{}', ?)"#,
    )
    .bind(user_id)
    .bind(&now)
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
        updated_at: now,
    }
}

fn sanitize_color(c: Option<String>) -> Option<String> {
    let mut s = c?.trim().to_string();
    if s.is_empty() {
        return None;
    }
    if s.len() > 16 {
        s.truncate(16);
    }
    let ok = s.starts_with('#')
        && (s.len() == 7 || s.len() == 9)
        && s.chars().skip(1).all(|ch| ch.is_ascii_hexdigit());
    if ok { Some(s) } else { None }
}

fn sanitize_about(s: Option<String>) -> Option<String> {
    let mut v = s?.trim().to_string();
    if v.is_empty() {
        return None;
    }
    if v.len() > 512 {
        v.truncate(512);
    }
    Some(v)
}

fn sanitize_status_text(s: Option<String>) -> Option<String> {
    let mut v = s?.trim().to_string();
    if v.is_empty() {
        return None;
    }
    if v.len() > 128 {
        v.truncate(128);
    }
    Some(v)
}

async fn get_my_profile(State(st): State<AppState>, me: AuthUser) -> impl IntoResponse {
    let v = get_or_create_profile(&st.db, me.id).await;
    (StatusCode::OK, Json(v)).into_response()
}

async fn update_my_profile(
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
        SET avatar_file_id = ?,
            banner_file_id = ?,
            accent_color = ?,
            about = ?,
            status_text = ?,
            integrations_json = ?,
            updated_at = ?
        WHERE user_id = ?
        "#,
    )
    .bind(avatar_file_id)
    .bind(banner_file_id)
    .bind(accent_color)
    .bind(about)
    .bind(status_text)
    .bind(integrations_json)
    .bind(&now)
    .bind(me.id)
    .execute(db)
    .await;

    if q.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let v = get_or_create_profile(db, me.id).await;
    (StatusCode::OK, Json(v)).into_response()
}

async fn get_profile_by_id(State(st): State<AppState>, _me: AuthUser, Path(id): Path<i64>) -> impl IntoResponse {
    let banned = sqlx::query_scalar::<_, i64>("SELECT is_banned FROM users WHERE id = ? LIMIT 1")
        .bind(id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten();

    let Some(b) = banned else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if b != 0 {
        return StatusCode::FORBIDDEN.into_response();
    }

    let v = get_or_create_profile(&st.db, id).await;
    (StatusCode::OK, Json(v)).into_response()
}

#[derive(Serialize)]
pub struct BlockView {
    pub user_id: i64,
    pub username: String,
    pub created_at: String,
}

async fn list_blocks(State(st): State<AppState>, me: AuthUser) -> impl IntoResponse {
    let rows = sqlx::query(
        r#"
        SELECT b.blocked_id AS user_id, u.username, b.created_at
        FROM user_blocks b
        JOIN users u ON u.id = b.blocked_id
        WHERE b.blocker_id = ?
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
            created_at: r.get("created_at"),
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(out)).into_response()
}

async fn block_user(State(st): State<AppState>, me: AuthUser, Path(user_id): Path<i64>) -> impl IntoResponse {
    if user_id == me.id {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let exists = sqlx::query_scalar::<_, i64>("SELECT 1 FROM users WHERE id = ? LIMIT 1")
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
        "INSERT OR IGNORE INTO user_blocks(blocker_id, blocked_id, created_at) VALUES(?, ?, ?)",
    )
    .bind(me.id)
    .bind(user_id)
    .bind(&now)
    .execute(&st.db)
    .await;

    (StatusCode::OK, Json(serde_json::json!({"status":"ok"}))).into_response()
}

async fn unblock_user(State(st): State<AppState>, me: AuthUser, Path(user_id): Path<i64>) -> impl IntoResponse {
    let _ = sqlx::query("DELETE FROM user_blocks WHERE blocker_id = ? AND blocked_id = ?")
        .bind(me.id)
        .bind(user_id)
        .execute(&st.db)
        .await;

    (StatusCode::OK, Json(serde_json::json!({"status":"ok"}))).into_response()
}

#[derive(Deserialize)]
pub struct DeleteMeBody {
    pub username: String,
}

async fn delete_me(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<DeleteMeBody>,
) -> impl IntoResponse {
    let db = &st.db;

    let row = sqlx::query("SELECT username FROM users WHERE id = ? LIMIT 1")
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

    let _ = sqlx::query("DELETE FROM user_sessions WHERE user_id = ?")
        .bind(me.id)
        .execute(&mut *tx)
        .await;

    let _ = sqlx::query("DELETE FROM friendships WHERE user_id = ? OR friend_id = ?")
        .bind(me.id)
        .bind(me.id)
        .execute(&mut *tx)
        .await;

    let _ = sqlx::query("DELETE FROM friend_requests WHERE sender_id = ? OR receiver_id = ?")
        .bind(me.id)
        .bind(me.id)
        .execute(&mut *tx)
        .await;

    let _ = sqlx::query("DELETE FROM server_members WHERE user_id = ?")
        .bind(me.id)
        .execute(&mut *tx)
        .await;

    let new_username = format!("deleted_{}", me.id);
    let new_pwd = format!("deleted:{}", uuid::Uuid::new_v4());

    let _ = sqlx::query(
        r#"
        UPDATE users
        SET username = ?,
            email = NULL,
            email_pending = NULL,
            email_verified = 0,
            password_hash = ?,
            token_version = token_version + 1,
            public_encryption_key = NULL,
            is_banned = 1
        WHERE id = ?
        "#,
    )
    .bind(&new_username)
    .bind(&new_pwd)
    .bind(me.id)
    .execute(&mut *tx)
    .await;

    let _ = sqlx::query("UPDATE user_presence SET is_online = 0, status = 'offline', updated_at = ? WHERE user_id = ?")
        .bind(auth::now_iso())
        .bind(me.id)
        .execute(&mut *tx)
        .await;

    if tx.commit().await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    (StatusCode::OK, Json(serde_json::json!({"detail":"deleted"}))).into_response()
}
