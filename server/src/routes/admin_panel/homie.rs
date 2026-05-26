use crate::server::{AdminSession, AppState};

use axum::{
    extract::State,
    Json,
    http::HeaderMap,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use std::env;

use super::{require_admin_panel_enabled, require_allow_ip, require_auth};

#[derive(Deserialize)]
pub(super) struct HomieJsonForm {
    csrf: String,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    message: String,
}

#[derive(Serialize)]
pub(super) struct HomieJsonResponse {
    ok: bool,
    answer: String,
    error: String,
}

#[derive(Serialize)]
pub(super) struct HomieProxyResponse {
    ok: bool,
    error: String,
    upstream: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub(super) struct HomieUpstreamRequest {
    session_id: String,
    message: String,
}

fn homie_base_url() -> String {
    env::var("LB_HOMIE_API_URL")
        .or_else(|_| env::var("HOMIE_API_URL"))
        .unwrap_or_else(|_| "http://127.0.0.1:8090".to_string())
        .trim()
        .trim_end_matches('/')
        .to_string()
}

fn homie_http_token() -> Option<String> {
    env::var("LB_HOMIE_HTTP_TOKEN")
        .or_else(|_| env::var("HOMIE_HTTP_TOKEN"))
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn homie_attach_auth(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    if let Some(token) = homie_http_token() {
        req.bearer_auth(token.clone()).header("X-Homie-Token", token)
    } else {
        req
    }
}

fn homie_session_id(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.trim().chars().take(80) {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.' || ch == ':' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() { "admin-center".to_string() } else { out }
}

fn homie_looks_like_json_text(s: &str) -> bool {
    let t = s.trim();
    (t.len() >= 2)
        && ((t.starts_with('{') && t.ends_with('}'))
            || (t.starts_with('[') && t.ends_with(']'))
            || (t.starts_with('\"') && t.ends_with('\"')))
}

fn homie_normalize_plain_text(s: &str) -> String {
    if s.contains('\n') {
        return s.to_string();
    }

    if !s.contains("\\n") && !s.contains("\\r\\n") {
        return s.to_string();
    }

    s.replace("\\r\\n", "\n")
        .replace("\\n", "\n")
        .replace("\\t", "  ")
}

fn homie_text_from_value(value: &serde_json::Value, depth: usize) -> Option<String> {
    if depth > 6 {
        return None;
    }

    match value {
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return None;
            }

            if homie_looks_like_json_text(trimmed) {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if let Some(text) = homie_text_from_value(&parsed, depth + 1) {
                        if !text.trim().is_empty() {
                            return Some(text);
                        }
                    }
                }
            }

            Some(homie_normalize_plain_text(s))
        }
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(text) = homie_text_from_value(item, depth + 1) {
                    if !text.trim().is_empty() {
                        return Some(text);
                    }
                }
            }
            None
        }
        serde_json::Value::Object(map) => {
            const KEYS: &[&str] = &["answer", "final", "message", "content", "text", "output", "response", "result"];

            for key in KEYS {
                if let Some(v) = map.get(*key) {
                    if let Some(text) = homie_text_from_value(v, depth + 1) {
                        if !text.trim().is_empty() {
                            return Some(text);
                        }
                    }
                }
            }

            if let Some(choices) = map.get("choices") {
                if let Some(text) = homie_text_from_value(choices, depth + 1) {
                    if !text.trim().is_empty() {
                        return Some(text);
                    }
                }
            }

            None
        }
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(v) => Some(v.to_string()),
        serde_json::Value::Null => None,
    }
}

fn homie_error_from_value(value: &serde_json::Value) -> String {
    match value.get("error") {
        Some(v) => homie_text_from_value(v, 0).unwrap_or_default(),
        None => String::new(),
    }
}

fn homie_ok_from_value(value: &serde_json::Value, answer: &str) -> bool {
    value
        .get("ok")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || !answer.trim().is_empty()
}

pub async fn homie_health_get(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err((_code, msg)) = require_admin_panel_enabled() {
        return Json(HomieProxyResponse { ok: false, error: msg, upstream: None }).into_response();
    }
    if let Err((_code, msg)) = require_allow_ip(&headers) {
        return Json(HomieProxyResponse { ok: false, error: msg, upstream: None }).into_response();
    }
    if require_auth(&st, &headers).is_err() {
        return Json(HomieProxyResponse { ok: false, error: "Нужна авторизация администратора".to_string(), upstream: None }).into_response();
    }

    let url = format!("{}/health", homie_base_url());
    let client = reqwest::Client::new();
    let res = homie_attach_auth(client.get(url)).send().await;

    match res {
        Ok(resp) => {
            let status_ok = resp.status().is_success();
            match resp.json::<serde_json::Value>().await {
                Ok(value) => Json(HomieProxyResponse {
                    ok: status_ok && value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
                    error: if status_ok { String::new() } else { "Homie health вернул ошибку".to_string() },
                    upstream: Some(value),
                }).into_response(),
                Err(e) => Json(HomieProxyResponse {
                    ok: false,
                    error: format!("Не удалось разобрать ответ Homie health: {e}"),
                    upstream: None,
                }).into_response(),
            }
        }
        Err(e) => Json(HomieProxyResponse {
            ok: false,
            error: format!("Homie API недоступен: {e}"),
            upstream: None,
        }).into_response(),
    }
}

pub async fn homie_tools_get(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err((_code, msg)) = require_admin_panel_enabled() {
        return Json(HomieProxyResponse { ok: false, error: msg, upstream: None }).into_response();
    }
    if let Err((_code, msg)) = require_allow_ip(&headers) {
        return Json(HomieProxyResponse { ok: false, error: msg, upstream: None }).into_response();
    }
    if require_auth(&st, &headers).is_err() {
        return Json(HomieProxyResponse { ok: false, error: "Нужна авторизация администратора".to_string(), upstream: None }).into_response();
    }

    let url = format!("{}/tools", homie_base_url());
    let client = reqwest::Client::new();
    let res = homie_attach_auth(client.get(url)).send().await;

    match res {
        Ok(resp) => {
            let status_ok = resp.status().is_success();
            match resp.json::<serde_json::Value>().await {
                Ok(value) => Json(HomieProxyResponse {
                    ok: status_ok && value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
                    error: if status_ok { String::new() } else { "Homie tools вернул ошибку".to_string() },
                    upstream: Some(value),
                }).into_response(),
                Err(e) => Json(HomieProxyResponse {
                    ok: false,
                    error: format!("Не удалось разобрать ответ Homie tools: {e}"),
                    upstream: None,
                }).into_response(),
            }
        }
        Err(e) => Json(HomieProxyResponse {
            ok: false,
            error: format!("Homie API недоступен: {e}"),
            upstream: None,
        }).into_response(),
    }
}

pub async fn homie_chat_post(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(f): Json<HomieJsonForm>,
) -> impl IntoResponse {
    if let Err((_code, msg)) = require_admin_panel_enabled() {
        return Json(HomieJsonResponse { ok: false, answer: String::new(), error: msg }).into_response();
    }
    if let Err((_code, msg)) = require_allow_ip(&headers) {
        return Json(HomieJsonResponse { ok: false, answer: String::new(), error: msg }).into_response();
    }

    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(_) => {
            return Json(HomieJsonResponse {
                ok: false,
                answer: String::new(),
                error: "Нужна авторизация администратора".to_string(),
            }).into_response();
        }
    };

    if f.csrf != sess.csrf {
        return Json(HomieJsonResponse {
            ok: false,
            answer: String::new(),
            error: "CSRF-токен не совпадает".to_string(),
        }).into_response();
    }

    let message = f.message.trim().to_string();
    if message.is_empty() {
        return Json(HomieJsonResponse {
            ok: false,
            answer: String::new(),
            error: "Пустое сообщение".to_string(),
        }).into_response();
    }

    let url = format!("{}/chat", homie_base_url());
    let req = HomieUpstreamRequest {
        session_id: homie_session_id(&f.session_id),
        message,
    };

    let client = reqwest::Client::new();
    let res = homie_attach_auth(client.post(url).json(&req)).send().await;
    let res = match res {
        Ok(v) => v,
        Err(e) => {
            return Json(HomieJsonResponse {
                ok: false,
                answer: String::new(),
                error: format!("Homie API недоступен: {e}"),
            }).into_response();
        }
    };

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        let extra = if body.trim().is_empty() { String::new() } else { format!(": {}", body.chars().take(500).collect::<String>()) };
        return Json(HomieJsonResponse {
            ok: false,
            answer: String::new(),
            error: format!("Homie API вернул HTTP {status}{extra}"),
        }).into_response();
    }

    match res.json::<serde_json::Value>().await {
        Ok(value) => {
            let answer = homie_text_from_value(&value, 0).unwrap_or_default();
            let error = homie_error_from_value(&value);

            Json(HomieJsonResponse {
                ok: homie_ok_from_value(&value, &answer),
                answer,
                error,
            }).into_response()
        }
        Err(e) => Json(HomieJsonResponse {
            ok: false,
            answer: String::new(),
            error: format!("Не удалось разобрать ответ Homie API: {e}"),
        }).into_response(),
    }
}

pub async fn homie_reset_post(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(f): Json<HomieJsonForm>,
) -> impl IntoResponse {
    if let Err((_code, msg)) = require_admin_panel_enabled() {
        return Json(HomieJsonResponse { ok: false, answer: String::new(), error: msg }).into_response();
    }
    if let Err((_code, msg)) = require_allow_ip(&headers) {
        return Json(HomieJsonResponse { ok: false, answer: String::new(), error: msg }).into_response();
    }

    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(_) => {
            return Json(HomieJsonResponse {
                ok: false,
                answer: String::new(),
                error: "Нужна авторизация администратора".to_string(),
            }).into_response();
        }
    };

    if f.csrf != sess.csrf {
        return Json(HomieJsonResponse {
            ok: false,
            answer: String::new(),
            error: "CSRF-токен не совпадает".to_string(),
        }).into_response();
    }

    let url = format!("{}/reset", homie_base_url());
    let req = HomieUpstreamRequest {
        session_id: homie_session_id(&f.session_id),
        message: String::new(),
    };

    let client = reqwest::Client::new();
    match homie_attach_auth(client.post(url).json(&req)).send().await {
        Ok(res) if res.status().is_success() => Json(HomieJsonResponse {
            ok: true,
            answer: "Контекст Homie сброшен".to_string(),
            error: String::new(),
        }).into_response(),
        Ok(res) => Json(HomieJsonResponse {
            ok: false,
            answer: String::new(),
            error: format!("Homie API вернул HTTP {}", res.status()),
        }).into_response(),
        Err(e) => Json(HomieJsonResponse {
            ok: false,
            answer: String::new(),
            error: format!("Homie API недоступен: {e}"),
        }).into_response(),
    }
}

pub fn render_homie_center_panel(sess: &AdminSession) -> String {
    format!(
        r#"<div class='card homie-chat-card' id='homie-center-root'>
  <input type='hidden' id='homie-center-csrf' value='{csrf}' />
  <input type='hidden' id='homie-center-session' value='admin-center' />

  <div class='homie-chat-topbar'>
    <div class='homie-chat-titlebox'>
      <div class='homie-chat-avatar'>H</div>
      <div class='homie-chat-title-main'>
        <div class='homie-chat-name'>Homie AI</div>
        <div class='homie-chat-sub'>Локальный агент админ-панели</div>
      </div>
    </div>
    <div class='homie-chat-actions'>
      <span class='pill homie-status-pill' id='homie-center-status'>Проверка...</span>
      <button type='button' class='btn-soft homie-top-btn' id='homie-center-check'>Статус</button>
      <button type='button' class='btn-soft homie-top-btn' id='homie-center-tools'>Инструменты</button>
    </div>
  </div>

  <div id='homie-center-feed' class='homie-chat-feed'></div>

  <div class='homie-composer'>
    <textarea id='homie-center-input' class='homie-input' rows='1' autocomplete='off' spellcheck='true' placeholder='Напиши задачу для Homie...'></textarea>
    <div class='homie-composer-row'>
      <span class='homie-composer-hint'>Enter — отправить · Shift/Ctrl + Enter — новая строка</span>
      <span class='homie-chat-actions'>
        <button type='button' class='btn-soft homie-reset-btn' id='homie-center-reset'>Сбросить контекст</button>
        <button type='button' class='btn-soft homie-send-btn' id='homie-center-send'>Отправить</button>
      </span>
    </div>
  </div>
</div>
"#,
        csrf = super::escape_html(&sess.csrf),
    )
}
