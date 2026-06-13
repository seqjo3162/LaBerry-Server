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
use tracing;

pub mod profile;
pub mod settings;
pub mod social;

use self::{profile::*, settings::*, social::*};

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct UserConnection {
    pub kind: String,
    pub url: String,
    pub label: Option<String>,
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

pub fn default_settings() -> UserSettings {
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
        voice_input_device_id: "default".to_string(),
        voice_video_device_id: "default".to_string(),
        developer_mode: false,
    }
}

pub fn sanitize_theme(s: &str) -> String {
    match s.to_ascii_lowercase().as_str() {
        "light" => "light".to_string(),
        _ => "dark".to_string(),
    }
}

pub fn sanitize_locale(s: &str) -> String {
    match s.to_ascii_lowercase().as_str() {
        "en" => "en".to_string(),
        _ => "ru".to_string(),
    }
}

pub fn sanitize_friend_requests(s: &str) -> String {
    match s.to_ascii_lowercase().as_str() {
        "everyone" => "everyone".to_string(),
        "friends_of_friends" => "friends_of_friends".to_string(),
        "server_members" => "server_members".to_string(),
        "none" => "none".to_string(),
        _ => "everyone".to_string(),
    }
}

pub fn sanitize_dms(s: &str) -> String {
    match s.to_ascii_lowercase().as_str() {
        "friends_only" => "friends_only".to_string(),
        "friends_and_server" => "friends_and_server".to_string(),
        "everyone" => "everyone".to_string(),
        _ => "friends_and_server".to_string(),
    }
}

pub fn sanitize_connection_kind(s: &str) -> String {
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

pub fn sanitize_connection_url(url: &str) -> Option<String> {
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

pub fn sanitize_connections(list: Vec<UserConnection>) -> Vec<UserConnection> {
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

pub fn sanitize_device_id(s: &str) -> String {
    let mut out = s.trim().to_string();
    if out.is_empty() || out == "default" {
        return "default".to_string();
    }
    if out.len() > 256 {
        out.truncate(256);
    }
    out
}

pub fn sanitize_settings(mut s: UserSettings) -> UserSettings {
    s.theme = sanitize_theme(&s.theme);
    s.locale = sanitize_locale(&s.locale);
    s.show_header_status = false;
    s.friend_requests = sanitize_friend_requests(&s.friend_requests);
    s.dms = sanitize_dms(&s.dms);

    if !(0.8..=1.3).contains(&s.font_scale) {
        s.font_scale = 1.0;
    }

    s.connections = sanitize_connections(s.connections);
    s.voice_input_device_id = sanitize_device_id(&s.voice_input_device_id);
    s.voice_video_device_id = sanitize_device_id(&s.voice_video_device_id);
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
    pub cookie_consent_status: String,
    pub cookie_consent_at: Option<String>,
    pub trust_factor: i64,
    pub trust_review_status: String,
    pub trust_review_reason: Option<String>,
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

#[derive(Deserialize)]
pub struct RegisterDeviceKeyBody {
    pub device_id: String,
    /// X25519 public key (base64url) or JSON string containing it
    pub public_jwk: Option<String>,
    /// Опциональная метка устройства (например "iPhone 15")
    pub label: Option<String>,
}

#[derive(Serialize)]
pub struct DeviceKeyView {
    pub device_id: String,
    pub public_jwk: Option<String>,
    pub label: Option<String>,
    pub last_seen: Option<String>,
}

fn normalize_client_public_key(raw: &str) -> Option<String> {
    use crate::e2ee::E2eeKeyPair;

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(s) = parsed.as_str() {
            return E2eeKeyPair::from_public_b64(s).map(|k| k.public_key_b64);
        }
    }
    E2eeKeyPair::from_public_b64(trimmed).map(|k| k.public_key_b64)
}

pub fn sanitize_email(s: &str) -> Option<String> {
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

pub fn sanitize_email_purpose(p: Option<String>) -> String {
    match p.unwrap_or_else(|| "verify_email".to_string()).to_ascii_lowercase().as_str() {
        "change_email" => "change_email".to_string(),
        _ => "verify_email".to_string(),
    }
}

pub fn sanitize_status(s: &str) -> String {
    match s.to_ascii_lowercase().as_str() {
        "online" => "online".to_string(),
        "idle" => "idle".to_string(),
        "dnd" => "dnd".to_string(),
        "invisible" => "invisible".to_string(),
        _ => "online".to_string(),
    }
}

pub fn env_bool(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(false)
}

pub fn send_email_code_by_command(email: &str, code: &str, purpose: &str) -> Result<(), String> {
    let command = std::env::var("LB_EMAIL_SEND_COMMAND")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "not_configured".to_string())?;

    let subject = match purpose {
        "change_email" => "LaBerry: подтверждение нового email",
        _ => "LaBerry: подтверждение email",
    };

    let body = format!(
        "Ваш код подтверждения LaBerry: {}\n\nКод действует 10 минут. Если вы не запрашивали код, просто проигнорируйте письмо.",
        code
    );

    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.arg("/C").arg(&command);
        c
    };

    #[cfg(not(target_os = "windows"))]
    let mut cmd = {
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg(&command);
        c
    };

    let output = cmd
        .env("LB_EMAIL_TO", email)
        .env("LB_EMAIL_CODE", code)
        .env("LB_EMAIL_PURPOSE", purpose)
        .env("LB_EMAIL_SUBJECT", subject)
        .env("LB_EMAIL_BODY", body)
        .output()
        .map_err(|e| format!("failed_to_run: {}", e))?;

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if output.status.success() && stderr.is_empty() {
        Ok(())
    } else if output.status.success() {
        Err(stderr)
    } else {
        if stderr.is_empty() {
            Err(format!("exit_status: {}", output.status))
        } else {
            Err(stderr)
        }
    }
}

pub fn sanitize_color(c: Option<String>) -> Option<String> {
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

pub fn sanitize_about(s: Option<String>) -> Option<String> {
    let mut v = s?.trim().to_string();
    if v.is_empty() {
        return None;
    }
    if v.len() > 512 {
        v.truncate(512);
    }
    Some(v)
}

pub fn sanitize_status_text(s: Option<String>) -> Option<String> {
    let mut v = s?.trim().to_string();
    if v.is_empty() {
        return None;
    }
    if v.len() > 128 {
        v.truncate(128);
    }
    Some(v)
}

pub fn sanitize_report_reason(raw: Option<String>) -> String {
    let value = raw.unwrap_or_else(|| "other".to_string()).trim().to_ascii_lowercase();
    match value.as_str() {
        "spam" => "spam".to_string(),
        "abuse" => "abuse".to_string(),
        "avatar" => "avatar".to_string(),
        "username" => "username".to_string(),
        "ads" => "ads".to_string(),
        "scam" => "scam".to_string(),
        "other" => "other".to_string(),
        _ => "other".to_string(),
    }
}

pub fn sanitize_report_message(raw: Option<String>) -> String {
    let mut out = raw.unwrap_or_default().trim().to_string();
    if out.len() > 1200 {
        out.truncate(1200);
    }
    out
}

pub fn trim_chars(raw: &str, max_chars: usize) -> String {
    raw.trim().chars().take(max_chars).collect()
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me", get(me).put(update_me))
        .route("/me/device-keys", post(register_device_key))
        .route("/me/email/request_code", post(request_email_code))
        .route("/me/email/confirm_code", post(confirm_email_code))
        .route("/me/profile", get(get_my_profile).put(update_my_profile))
        .route("/me/delete", post(delete_me))
        .route("/me/blocks", get(list_blocks))
        .route("/me/blocks/{user_id}", put(block_user).delete(unblock_user))
        .route("/me/status", get(my_status).put(set_my_status))
        .route("/me/cookie-consent", post(set_cookie_consent))
        .route("/me/settings", get(get_my_settings).put(update_my_settings))
        .route("/me/suggestions", get(list_my_suggestions).post(create_suggestion))
        .route("/me/password", put(change_password))
        .route("/me/username", put(change_username))
        .route("/", get(list_users))
        .route("/search", get(search))
        .route("/{id}/report", post(report_user))
        .route("/{id}/profile", get(get_profile_by_id))
        .route("/{id}", get(get_by_id))
        .route("/{id}/device-keys", get(get_user_device_keys))
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

async fn register_device_key(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<RegisterDeviceKeyBody>,
) -> impl IntoResponse {
    let db = &st.db;
    let now = auth::now_iso();

    let did = body.device_id.trim();
    if did.is_empty() || did.len() > 255 {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "Invalid device_id"}))).into_response();
    }

    if !did.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "Invalid device_id format"}))).into_response();
    }
    
    let raw_key = body.public_jwk.as_deref().unwrap_or("").trim();
    let is_valid_b64 = raw_key.len() >= 43 && raw_key.len() <= 44 
        && raw_key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '+' || c == '/' || c == '=');

    let pub_key_b64 = if is_valid_b64 {
        raw_key.to_string()
    } else if let Some(normalized) = body.public_jwk.as_deref().and_then(normalize_client_public_key) {
        normalized
    } else {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "Invalid public_jwk"}))).into_response();
    };

    let _ = sqlx::query("UPDATE users SET public_encryption_key = ? WHERE id = ?")
        .bind(&pub_key_b64).bind(me.id).execute(db).await;

    let label = body.label.as_ref().map(|s| s.trim().to_string());
    let _ = sqlx::query(
        r#"INSERT OR REPLACE INTO user_device_keys(device_id, user_id, public_jwk, label, created_at, last_seen)
           VALUES(?, ?, ?, ?, ?, ?)"#,
    )
    .bind(did).bind(me.id).bind(&pub_key_b64).bind(label).bind(&now).bind(&now)
    .execute(db).await;

    (StatusCode::OK, Json(serde_json::json!({
        "ok": true, "public_key": &pub_key_b64, "device_id": did
    }))).into_response()
}

async fn get_user_device_keys(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    let rows = sqlx::query(
        r#"SELECT device_id, public_jwk, label, last_seen FROM user_device_keys WHERE user_id = ?"#,
    )
    .bind(id)
    .fetch_all(db)
    .await
    .ok()
    .unwrap_or_default();

    let account_key: Option<String> = sqlx::query_scalar(
        "SELECT public_encryption_key FROM users WHERE id = ? LIMIT 1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let mut out: Vec<DeviceKeyView> = Vec::new();
    for r in rows.into_iter() {
        let device_id: String = r.get("device_id");
        let mut public_jwk: Option<String> = r.try_get("public_jwk").ok();
        if public_jwk.as_deref().unwrap_or("").trim().is_empty() {
            public_jwk = account_key.clone();
        }
        let label: Option<String> = r.get("label");
        let last_seen: Option<String> = r.get("last_seen");
        out.push(DeviceKeyView {
            device_id,
            public_jwk,
            label,
            last_seen,
        });
    }

    (StatusCode::OK, Json(out)).into_response()
}
