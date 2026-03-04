use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Serialize;

use crate::middleware::auth_guard::AuthUser;
use crate::server::AppState;

#[derive(Debug, Serialize, Clone)]
pub struct IceServer {
    pub urls: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct IceResponse {
    #[serde(rename = "iceServers")]
    pub ice_servers: Vec<IceServer>,
}

fn env_bool(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        }
        Err(_) => false,
    }
}

fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

fn env_csv(name: &str) -> Vec<String> {
    std::env::var(name)
        .ok()
        .map(|v| {
            v.split(',')
                .map(|x| x.trim())
                .filter(|x| !x.is_empty())
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn default_stun_urls() -> Vec<String> {
    vec![
        "stun:stun.l.google.com:19302".to_string(),
        "stun:stun1.l.google.com:19302".to_string(),
    ]
}

fn turn_rest_credential(secret: &str, username: &str) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use hmac::{Hmac, Mac};
    use sha1::Sha1;

    let mut mac = Hmac::<Sha1>::new_from_slice(secret.as_bytes())
        .expect("HMAC can take key of any size");
    mac.update(username.as_bytes());
    let out = mac.finalize().into_bytes();
    STANDARD.encode(out)
}

async fn ice(State(_st): State<AppState>, me: AuthUser) -> impl IntoResponse {
    let mut out: Vec<IceServer> = Vec::new();

    let mut stun_urls = env_csv("LB_STUN_URLS");
    if stun_urls.is_empty() {
        stun_urls = default_stun_urls();
    }
    if !stun_urls.is_empty() {
        out.push(IceServer {
            urls: stun_urls,
            username: None,
            credential: None,
        });
    }

    if !env_bool("LB_TURN_DISABLE") {
        let turn_urls = env_csv("LB_TURN_URLS");
        let turn_username = std::env::var("LB_TURN_USERNAME").ok();
        let turn_password = std::env::var("LB_TURN_PASSWORD").ok();
        let turn_secret = std::env::var("LB_TURN_SECRET").ok();
        let ttl_sec = env_i64("LB_TURN_TTL_SEC", 3600).max(60);

        if !turn_urls.is_empty() {
            let (u, c) = if let Some(secret) = turn_secret {
                let now = chrono::Utc::now().timestamp();
                let exp = now + ttl_sec;
                let username = format!("{}:{}", exp, me.id);
                let credential = turn_rest_credential(&secret, &username);
                (Some(username), Some(credential))
            } else if turn_username.is_some() && turn_password.is_some() {
                (turn_username, turn_password)
            } else {
                (None, None)
            };

            out.push(IceServer {
                urls: turn_urls,
                username: u,
                credential: c,
            });
        }
    }

    (StatusCode::OK, Json(IceResponse { ice_servers: out })).into_response()
}

pub fn router() -> Router<AppState> {
    Router::new().route("/ice", get(ice))
}
