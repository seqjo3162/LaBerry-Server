use crate::{auth, middleware::rate_limit, server::{AdminSession, AppState}};

use axum::{
    body::Body,
    extract::{Form, Path, Query, State, ConnectInfo},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};

use std::net::SocketAddr;
use chrono::{Duration as ChronoDuration, TimeZone, Utc};
use argon2::password_hash::{PasswordHash, PasswordVerifier};
use regex::Regex;
use serde::Deserialize;
use sqlx::{Row, PgPool};
use std::{env, net::IpAddr};

pub mod users;
use users::*;

pub(super) mod content;
use self::content::*;

// homie module not exposed in admin panel UI

mod servers;
use servers::*;

// =============================
// Router
// =============================

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(root))
        .route("/login", get(login_get).post(login_post))
        .route("/logout", post(logout_post))
        .route("/users", get(users_list))
        .route("/users/{id}/card", get(admin_user_card_fragment))
        .route("/users/{id}/details", get(admin_user_details_fragment))
        .route("/users/{id}/ban", post(user_ban))
        .route("/users/{id}/unban", post(user_unban))
        .route("/users/{id}/ban_forever", post(user_ban_forever))
        .route("/users/{id}/purge", post(user_purge_content))
        .route("/users/{user_id}/kick_from/{server_id}", post(admin_user_kick_from_server))
        .route("/reports/{id}/status", post(admin_report_status))
        .route("/suggestions", get(suggestions_page))
        .route("/suggestions/{id}/status", post(admin_suggestion_status))
        .route("/downloads", get(downloads_page))
        .route("/downloads/upload", post(admin_download_upload))
        .route("/downloads/{id}/delete", post(admin_download_delete))
        .route("/test-users", get(test_users_page).post(test_users_delete))
        .route("/servers", get(servers_list))
        .route("/servers/{id}/delete", post(server_delete))
        .route("/servers/{id}/add_all_users", post(server_add_all_users))
        .route("/center", get(center_page))
        .route("/files/{id}/raw", get(admin_file_raw))
        .route("/profile-files/{id}/raw", get(admin_profile_file_raw))
        .route("/db", get(db_tools_page))
        .route("/db/wipe_messages", post(db_wipe_messages_post))
        .route("/db/wipe_servers", post(db_wipe_servers_post))
        .route("/db/reset_keep_users", post(db_reset_keep_users_post))
        .route("/db/vacuum", post(db_vacuum_post))
        .route("/db/cleanup_expired_files", post(db_cleanup_expired_files_post))
        .route("/db/expired_files", get(content::db_list_expired_files_get))
}

// =============================
// Config helpers
// =============================

fn admin_enabled() -> bool {
    env_bool("LB_ENABLE_ADMIN_PANEL", false) || admin_password_configured()
}

fn env_bool(key: &str, default: bool) -> bool {
    env::var(key)
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(default)
}

fn admin_cookie_secure(headers: &HeaderMap) -> bool {
    if env_bool("LB_ADMIN_COOKIE_INSECURE", false) {
        return false;
    }
    if env_bool("LB_ADMIN_COOKIE_SECURE", false) {
        return true;
    }
    
    if let Some(host) = headers.get("host").and_then(|v| v.to_str().ok()) {
        let host_lower = host.to_ascii_lowercase();
        if host_lower.starts_with("127.0.0.1") || host_lower.starts_with("localhost") {
            return false;
        }
    }

    if let Some(v) = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
    {
        return v.eq_ignore_ascii_case("https");
    }
    
    false
}

fn test_user_re() -> Regex {
    Regex::new(&env::var("LB_TEST_USER_REGEX").unwrap_or_else(|_| "^test_".to_string()))
        .unwrap_or_else(|_| Regex::new("^test_").unwrap())
}

pub(crate) fn test_server_re() -> Regex {
    Regex::new(&env::var("LB_TEST_SERVER_REGEX").unwrap_or_else(|_| "^test_".to_string()))
        .unwrap_or_else(|_| Regex::new("^test_").unwrap())
}

fn admin_env_trim(key: &str) -> Option<String> {
    env::var(key).ok().map(|v| {
        let v = v.trim();
        v.trim_matches('"').trim_matches('\'').to_string()
    }).filter(|s| !s.is_empty())
}

fn admin_password_configured() -> bool {
    admin_env_trim("LB_ADMIN_PASSWORD_HASH").is_some() || admin_env_trim("LB_ADMIN_PASSWORD").is_some()
}

pub(crate) fn admin_password_is_configured() -> bool {
    admin_password_configured()
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    if ab.len() != bb.len() {
        return false;
    }
    let mut r: u8 = 0;
    for i in 0..ab.len() {
        r |= ab[i] ^ bb[i];
    }
    r == 0
}

fn verify_admin_password(pw: &str) -> anyhow::Result<()> {
    if let Some(plain) = admin_env_trim("LB_ADMIN_PASSWORD") {
        if constant_time_eq(pw, &plain) {
            return Ok(());
        }
        anyhow::bail!("Неверный пароль администратора");
    }

    if let Some(hash) = admin_env_trim("LB_ADMIN_PASSWORD_HASH") {
        let argon2 = argon2::Argon2::default();
        let parsed_hash = PasswordHash::new(&hash).map_err(|_| anyhow::anyhow!("Invalid hash format"))?;
        argon2
            .verify_password(pw.as_bytes(), &parsed_hash)
            .map_err(|_| anyhow::anyhow!("Неверный пароль администратора"))?;
        return Ok(());
    }

    anyhow::bail!(
        "Пароль администратора не настроен (укажи LB_ADMIN_PASSWORD_HASH или LB_ADMIN_PASSWORD)"
    );
}

pub(crate) fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn escape_html_lines(s: &str) -> String {
    escape_html(s).replace("\r\n", "\n").replace('\r', "\n").replace('\n', "<br>")
}

pub(super) fn admin_sanitize_filename(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        let ok = ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-';
        if ok { out.push(ch); } else { out.push('_'); }
    }
    if out.is_empty() { "file".to_string() } else { out }
}

pub(super) fn admin_format_bytes(size: i64) -> String {
    if size <= 0 { return "размер неизвестен".to_string(); }
    let mut v = size as f64;
    let units = ["Б", "КБ", "МБ", "ГБ"];
    let mut idx = 0usize;
    while v >= 1024.0 && idx + 1 < units.len() { v /= 1024.0; idx += 1; }
    if idx == 0 { format!("{} {}", size, units[idx]) } else { format!("{:.1} {}", v, units[idx]) }
}

fn url_decode_component_lossy(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let val = |b: u8| -> Option<u8> {
                match b {
                    b'0'..=b'9' => Some(b - b'0'),
                    b'a'..=b'f' => Some(b - b'a' + 10),
                    b'A'..=b'F' => Some(b - b'A' + 10),
                    _ => None,
                }
            };
            if let (Some(a), Some(b)) = (val(bytes[i + 1]), val(bytes[i + 2])) {
                out.push((a << 4) | b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn render_admin_file_marker(file_id: i64, encoded_name: &str, mime: &str, size: i64) -> String {
    let name = url_decode_component_lossy(encoded_name);
    let safe_name = escape_html(&name);
    let clean_mime = if mime.trim().is_empty() { "application/octet-stream" } else { mime.trim() };
    let safe_mime = escape_html(clean_mime);
    let size_text = escape_html(&admin_format_bytes(size));
    let type_label = if clean_mime.starts_with("image/") { "IMG" }
        else if clean_mime.starts_with("video/") { "VID" }
        else if clean_mime.starts_with("audio/") { "AUD" }
        else if name.to_ascii_lowercase().ends_with(".md") { "MD" }
        else { "FILE" };

    format!(
        r#"<div class='admin-file-card'>
  <div class='admin-file-type'>{type_label}</div>
  <div class='admin-file-main'>
    <div class='admin-file-name'>{safe_name}</div>
    <div class='admin-file-meta'>#{file_id} · {safe_mime} · {size_text}</div>
  </div>
  <a class='admin-file-action' href='/admin/files/{file_id}/raw' target='_blank' rel='noopener'>Скачать</a>
</div>"#,
        type_label = escape_html(type_label), safe_name = safe_name, file_id = file_id,
        safe_mime = safe_mime, size_text = size_text,
    )
}

fn render_admin_message_html(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() { return "<span class='muted'>[вложение или пустое сообщение]</span>".to_string(); }

    let re = Regex::new(r"\[\[file:(\d+)\|([^|\]]+)(?:\|([^|\]]*))?(?:\|(\d+))?\]\]").unwrap();
    let mut out = String::new();
    let mut last = 0usize;
    let mut found = false;

    for cap in re.captures_iter(content) {
        let Some(m) = cap.get(0) else { continue; };
        if m.start() > last {
            let part = &content[last..m.start()];
            if !part.trim().is_empty() {
                out.push_str("<div class='admin-message-text'>");
                out.push_str(&escape_html_lines(part.trim()));
                out.push_str("</div>");
            }
        }
        let id = cap.get(1).and_then(|x| x.as_str().parse::<i64>().ok()).unwrap_or(0);
        let name = cap.get(2).map(|x| x.as_str()).unwrap_or("file");
        let mime = cap.get(3).map(|x| x.as_str()).unwrap_or("application/octet-stream");
        let size = cap.get(4).and_then(|x| x.as_str().parse::<i64>().ok()).unwrap_or(0);
        if id > 0 { out.push_str(&render_admin_file_marker(id, name, mime, size)); found = true; }
        else { out.push_str(&escape_html_lines(m.as_str())); }
        last = m.end();
    }

    if last < content.len() {
        let part = &content[last..];
        if !part.trim().is_empty() {
            out.push_str("<div class='admin-message-text'>");
            out.push_str(&escape_html_lines(part.trim()));
            out.push_str("</div>");
        }
    }
    if found || !out.trim().is_empty() { out } else { escape_html_lines(content) }
}

fn cookie_get(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let p = part.trim();
        if let Some((k, v)) = p.split_once('=') {
            if k.trim() == name {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

fn set_cookie(headers: &mut HeaderMap, cookie: String) {
    match HeaderValue::from_str(&cookie) {
        Ok(v) => {
            headers.append(header::SET_COOKIE, v);
        }
        Err(e) => {
            tracing::error!("[ADMIN] Invalid Set-Cookie header: {e}");
        }
    }
}

fn url_encode_component(s: &str) -> String {
    // RFC3986 unreserved: ALPHA / DIGIT / "-" / "." / "_" / "~"
    let mut out = String::new();
    for &b in s.as_bytes() {
        let c = b as char;
        let ok = c.is_ascii_alphanumeric() || "-._~".contains(c);
        if ok {
            out.push(c);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

pub(super) fn admin_redirect_with_msg(path: &str, msg: &str) -> impl IntoResponse {
    let sep = if path.contains('?') {
        if path.ends_with('?') || path.ends_with('&') { "" } else { "&" }
    } else {
        "?"
    };
    let p = format!("{}{}msg={}", path, sep, url_encode_component(msg));
    Redirect::to(&p)
}


fn is_test_user(re: &Regex, username: &str, email: &str) -> bool {
    re.is_match(username) || (!email.is_empty() && re.is_match(email))
}

pub(crate) fn is_test_server(re: &Regex, name: &str) -> bool {
    re.is_match(name)
}

fn now_ts() -> i64 {
    Utc::now().timestamp()
}

pub(super) fn fmt_admin_dt(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return "—".to_string();
    }

    if let Ok(ts) = s.parse::<i64>() {
        if let Some(dt) = Utc.timestamp_opt(ts, 0).single() {
            return dt.format("%Y-%m-%d %H:%M:%S UTC").to_string();
        }
    }

    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return dt
            .with_timezone(&Utc)
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string();
    }

    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        let dt = chrono::DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc);
        return dt.format("%Y-%m-%d %H:%M:%S UTC").to_string();
    }

    s.to_string()
}

pub(super) fn page(title: &str, body: &str, msg: Option<&str>) -> Html<String> {
    let msg_html = msg
        .filter(|m| !m.trim().is_empty())
        .map(|m| format!("<div class='msg'>{}</div>", escape_html(m)))
        .unwrap_or_default();

    let show_top_nav = !title.contains("Центр") && !title.contains("Вход");
    let nav = if show_top_nav {
        [
            ("/admin/users", "Пользователи", title.contains("Пользователи")),
            ("/admin/servers", "Серверы", title.contains("Серверы")),
            ("/admin/suggestions", "Предложения", title.contains("Предложения")),
            ("/admin/gifs", "GIF", title.contains("GIF")),
            ("/admin/downloads", "Загрузки", title.contains("Загрузки")),
            ("/admin/center", "Центр", title.contains("Центр")),
            ("/admin/db", "База данных", title.contains("База данных")),
        ]
        .iter()
        .map(|(href, label, active)| {
            let cls = if *active { "nav-link active" } else { "nav-link" };
            format!("<a href='{href}' class='{cls}'>{label}</a>")
        })
        .collect::<Vec<_>>()
        .join("")
    } else {
        String::new()
    };

    let main_class = if title.contains("Центр") {
        "admin-main admin-main-wide"
    } else {
        "admin-main"
    };

    let center_script = if title.contains("Центр") {
        "<script src='/static/js/admin-center.js?v=1' defer></script>"
    } else {
        ""
    };
    let users_script = if title.contains("Центр") || title.contains("Пользователи") {
        "<script src='/static/js/admin-users.js?v=2' defer></script>"
    } else {
        ""
    };
    let show_logout = !title.contains("Вход");
    let logout_html = if show_logout {
        "<form method='post' action='/admin/logout'><button type='submit'>Выйти</button></form>"
    } else {
        ""
    };

    let html = format!(
        r#"<!doctype html>
    <html lang="ru">
    <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>{title}</title>
    <style>
    :root {{
    --bg:#090c14;
    --panel:#10162a;
    --panel-2:#0d1324;
    --border:#243152;
    --text:#f2f5ff;
    --muted:#97a4c5;
    --accent:#7c5cff;
    --danger:#8f2f46;
    --danger-bg:#2d1420;
    --ok:#1c6b3d;
    --ok-bg:#0e2418;
    }}
    * {{ box-sizing:border-box; }}
    body {{ font-family:system-ui,-apple-system,Segoe UI,Roboto,Arial,sans-serif; background:var(--bg); color:var(--text); margin:0; }}
    header {{ position:sticky; top:0; z-index:20; padding:12px 18px; background:#0f1425; border-bottom:1px solid var(--border); display:flex; align-items:center; gap:12px; flex-wrap:wrap; }}
    .brand {{ font-weight:800; font-size:26px; letter-spacing:-0.03em; }}
    .nav {{ display:flex; gap:10px; flex-wrap:wrap; }}
    .nav-link {{ color:#dbe6ff; text-decoration:none; padding:10px 14px; border:1px solid var(--border); border-radius:14px; background:#121933; }}
    .nav-link.active {{ background:#1b2850; border-color:#4d67b1; box-shadow:inset 0 0 0 1px rgba(255,255,255,0.05); }}
    .nav-link:hover {{ background:#182243; }}
    header form {{ margin-left:auto; }}
    main.admin-main {{ padding:18px; max-width:1280px; margin:0 auto; }}
    main.admin-main-wide {{ width:100%; max-width:none; padding:20px clamp(18px,2.2vw,36px) 28px; }}
    .card {{ background:linear-gradient(180deg,#11172a 0%, #0d1324 100%); border:1px solid var(--border); border-radius:18px; padding:16px; margin-bottom:14px; box-shadow:0 20px 50px rgba(0,0,0,0.18); }}
    .card h2, .card h3 {{ margin:0 0 12px 0; }}
    .small {{ color:var(--muted); font-size:12px; }}
    input, button, textarea, select {{ font-size:14px; font-family:inherit; }}
    input[type=text], input[type=password], textarea, select {{ width:100%; max-width:640px; padding:11px 13px; border-radius:14px; border:1px solid var(--border); background:#090d18; color:var(--text); outline:none; }}
    input[type=text]:focus, input[type=password]:focus, textarea:focus, select:focus {{ border-color:#6452d7; box-shadow:0 0 0 3px rgba(124,92,255,0.15); }}
    button {{ padding:10px 14px; border-radius:14px; border:1px solid var(--border); background:#182243; color:var(--text); cursor:pointer; }}
    button:hover {{ background:#202c57; }}
    .btn-soft {{ background:#151d37; }}
    .btn-danger {{ background:var(--danger-bg); border-color:var(--danger); }}
    .btn-danger:hover {{ background:#3a1a29; }}
    .table {{ width:100%; border-collapse:collapse; }}
    .table th, .table td {{ border-bottom:1px solid var(--border); padding:12px 8px; text-align:left; vertical-align:top; }}
    .msg {{ background:#182243; border:1px solid #31467c; padding:11px 13px; border-radius:14px; margin-bottom:14px; }}
    .hstack {{ display:flex; align-items:center; gap:10px; flex-wrap:wrap; }}
    .search-row {{ display:flex; gap:10px; flex-wrap:wrap; align-items:center; }}
    .search-row form {{ display:flex; gap:10px; flex:1 1 560px; flex-wrap:wrap; }}
    .search-row input[type=text] {{ flex:1 1 340px; max-width:none; }}
    .pill {{ display:inline-flex; align-items:center; gap:8px; padding:7px 11px; border-radius:999px; border:1px solid var(--border); background:#0f1322; color:#cdd7f6; font-size:12px; }}
    .users-list, .servers-list, .db-list {{ display:flex; flex-direction:column; gap:12px; }}
    .user-card, .server-row-card {{ display:grid; grid-template-columns:minmax(0,1fr) auto; gap:14px; border:1px solid var(--border); background:#0d1120; border-radius:16px; padding:14px; }}
    .user-main, .server-main {{ min-width:0; }}
    .user-top, .server-top {{ display:flex; align-items:flex-start; justify-content:space-between; gap:12px; flex-wrap:wrap; margin-bottom:8px; }}
    .user-title, .server-title {{ display:flex; align-items:center; gap:10px; flex-wrap:wrap; min-width:0; }}
    .user-id, .server-id {{ color:#8fb4ff; font-weight:700; }}
    .user-name, .server-name {{ font-size:18px; font-weight:700; word-break:break-word; }}
    .user-email, .server-meta {{ color:var(--muted); word-break:break-all; }}
    .user-meta {{ color:var(--muted); font-size:12px; }}
    .user-fields {{ display:grid; grid-template-columns:repeat(3,minmax(0,1fr)); gap:10px; margin-top:10px; }}
    .user-field {{ border:1px solid var(--border); background:#10182d; border-radius:14px; padding:10px 12px; min-width:0; }}
    .user-field-label {{ color:var(--muted); font-size:11px; text-transform:uppercase; letter-spacing:.05em; margin-bottom:6px; }}
    .user-field-value {{ word-break:break-word; line-height:1.45; }}
    .user-field-value.mono {{ font-family:ui-monospace,SFMono-Regular,Consolas,monospace; }}
    .status-badge {{ display:inline-flex; align-items:center; padding:4px 10px; border-radius:999px; font-size:12px; font-weight:700; border:1px solid var(--border); }}
    .status-active {{ background:var(--ok-bg); color:#98e2b8; border-color:#214d35; }}
    .status-banned {{ background:#251316; color:#ffb4bf; border-color:#5a2730; }}
    .test-badge {{ background:#161d31; color:#9ec0ff; border-color:#2f4478; }}
    .user-actions, .server-actions {{ display:flex; flex-wrap:wrap; gap:8px; align-content:flex-start; justify-content:flex-end; }}
    .inline-form {{ display:inline-flex; margin:0; }}
    .empty-state {{ padding:26px 18px; border:1px dashed var(--border); border-radius:16px; color:var(--muted); text-align:center; }}
    .db-card {{ border:1px solid var(--border); border-radius:18px; padding:16px; background:#0d1324; }}
    .db-card h3 {{ margin-bottom:8px; }}
    .danger-note {{ color:#ffb9c7; }}
    .center-shell {{ display:grid; grid-template-columns:300px minmax(0,1fr); gap:16px; min-height:72vh; }}
    .center-sidebar, .center-main {{ background:linear-gradient(180deg,#10162a 0%, #0d1324 100%); border:1px solid var(--border); border-radius:20px; box-shadow:0 18px 40px rgba(0,0,0,0.22); }}
    .center-sidebar {{ padding:14px; display:flex; flex-direction:column; gap:10px; }}
    .center-main {{ padding:16px; }}
    .center-nav-item {{ display:block; padding:14px 15px; border:1px solid var(--border); border-radius:16px; background:#121933; text-decoration:none; color:var(--text); transition:background .16s ease,border-color .16s ease,transform .16s ease; }}
    .center-nav-item:hover {{ background:#182243; border-color:#36508d; transform:translateY(-1px); }}
    .center-nav-item strong {{ display:block; margin-bottom:6px; font-size:18px; }}
    .center-nav-item .small {{ line-height:1.45; }}
    .center-nav-item.active {{ background:linear-gradient(180deg,#19264a 0%, #131d39 100%); border-color:#4d67b1; }}
    .center-hero {{ display:flex; justify-content:space-between; align-items:flex-start; gap:14px; padding:16px; border:1px solid var(--border); border-radius:18px; background:#0b1020; margin-bottom:14px; flex-wrap:wrap; }}
    .center-hero-title {{ margin:0; font-size:28px; letter-spacing:-0.03em; }}
    .center-hero-sub {{ color:var(--muted); max-width:720px; line-height:1.45; margin-top:8px; }}
    .center-stat-row {{ display:flex; gap:10px; flex-wrap:wrap; }}
    .center-stat {{ min-width:128px; padding:12px 14px; border-radius:16px; border:1px solid var(--border); background:#121933; }}
    .center-stat-label {{ color:var(--muted); font-size:12px; margin-bottom:6px; }}
    .center-stat-value {{ font-size:24px; font-weight:800; line-height:1; }}
    .center-workspace {{ display:grid; grid-template-columns:340px minmax(0,1fr); gap:14px; min-height:58vh; }}
    .center-column {{ display:flex; flex-direction:column; gap:14px; min-width:0; }}
    .center-panel {{ border:1px solid var(--border); border-radius:18px; background:#0b1020; overflow:hidden; }}
    .center-panel-header {{ display:flex; justify-content:space-between; align-items:center; gap:10px; padding:14px 16px; border-bottom:1px solid var(--border); background:#11182d; flex-wrap:wrap; }}
    .center-panel-title {{ font-size:18px; font-weight:800; }}
    .center-panel-sub {{ color:var(--muted); font-size:12px; }}
    .center-panel-body {{ padding:14px; }}
    .center-filter-row {{ display:flex; gap:8px; flex-wrap:wrap; }}
    .center-chip {{ display:inline-flex; align-items:center; gap:7px; padding:7px 10px; border-radius:999px; border:1px solid var(--border); background:#131a30; color:#d9e3ff; font-size:12px; }}
    .center-queue-list {{ display:flex; flex-direction:column; gap:10px; }}
    .center-queue-item {{ border:1px solid var(--border); border-radius:16px; padding:12px; background:#11182d; }}
    .center-queue-top {{ display:flex; justify-content:space-between; gap:10px; align-items:flex-start; margin-bottom:8px; }}
    .center-queue-title {{ font-weight:800; line-height:1.3; }}
    .center-queue-meta {{ color:var(--muted); font-size:12px; line-height:1.45; }}
    .center-status {{ display:inline-flex; align-items:center; padding:4px 10px; border-radius:999px; font-size:12px; font-weight:700; border:1px solid var(--border); white-space:nowrap; }}
    .center-status-new {{ background:#1a2647; color:#aecaff; border-color:#39508a; }}
    .center-status-live {{ background:#10261b; color:#97e0b6; border-color:#28533a; }}
    .center-status-warn {{ background:#2a1d10; color:#ffc98f; border-color:#6f4b24; }}
    .center-status-muted {{ background:#151b2f; color:#b4c0df; border-color:#2e3a63; }}
    .center-mini-list {{ display:flex; flex-direction:column; gap:8px; }}
    .center-mini-item {{ display:flex; justify-content:space-between; gap:12px; padding:10px 12px; border:1px solid var(--border); border-radius:14px; background:#11182d; }}
    .center-mini-main {{ min-width:0; }}
    .center-mini-title {{ font-weight:700; margin-bottom:4px; word-break:break-word; }}
    .center-mini-meta {{ color:var(--muted); font-size:12px; }}
    .center-mini-side {{ color:var(--muted); font-size:12px; white-space:nowrap; }}
    .center-feed-list {{ display:flex; flex-direction:column; gap:10px; }}
    .center-feed-item {{ border:1px solid var(--border); border-radius:16px; padding:12px 14px; background:#11182d; }}
    .center-feed-head {{ display:flex; justify-content:space-between; gap:12px; align-items:flex-start; margin-bottom:8px; flex-wrap:wrap; }}
    .center-feed-author {{ font-weight:800; }}
    .center-feed-loc {{ color:#9eb7ff; font-size:12px; margin-top:4px; }}
    .center-feed-time {{ color:var(--muted); font-size:12px; white-space:nowrap; }}
    .center-feed-text {{ line-height:1.55; color:#eef2ff; word-break:break-word; }}
    .admin-message-text {{ margin:6px 0; }}
    .admin-file-card {{ display:flex; align-items:center; gap:12px; border:1px solid #314068; border-radius:14px; background:#0d1428; padding:10px 12px; margin:8px 0; max-width:720px; }}
    .admin-file-type {{ width:42px; height:34px; border-radius:10px; display:flex; align-items:center; justify-content:center; background:#172241; color:#c084fc; font-weight:900; font-size:12px; border:1px solid #3b4a77; flex:0 0 auto; }}
    .admin-file-main {{ min-width:0; flex:1; }}
    .admin-file-name {{ font-weight:800; color:#fff; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }}
    .admin-file-meta {{ color:var(--muted); font-size:12px; margin-top:3px; }}
    .admin-file-action {{ flex:0 0 auto; text-decoration:none; border:1px solid #4d67b1; color:#eef2ff; background:#172241; padding:7px 10px; border-radius:10px; font-size:12px; font-weight:800; }}
    .admin-file-action:hover {{ border-color:#8b5cf6; color:#fff; }}

    .center-empty {{ padding:28px 18px; border:1px dashed var(--border); border-radius:16px; color:var(--muted); text-align:center; background:#0f1527; }}
    .center-split {{ display:grid; grid-template-columns:minmax(0,1fr) 280px; gap:14px; }}
    .center-note-card {{ border:1px solid var(--border); border-radius:16px; background:#11182d; padding:14px; }}
    .center-note-card h3 {{ margin:0 0 10px 0; }}
    .center-note-list {{ display:flex; flex-direction:column; gap:8px; }}
    .center-note-line {{ color:var(--muted); font-size:13px; line-height:1.45; }}
    .panel-shell {{ display:grid; grid-template-columns:280px minmax(0,1fr); gap:18px; min-height:calc(100vh - 118px); align-items:stretch; }}
    .panel-sidebar {{ display:flex; flex-direction:column; gap:10px; padding:0; border:0; border-radius:0; background:transparent; }}
    .panel-stage {{ min-width:0; border:0; border-radius:0; background:transparent; box-shadow:none; overflow:visible; }}
    .panel-stage-header {{ display:flex; justify-content:space-between; align-items:center; gap:12px; padding:0 0 14px; border-bottom:1px solid rgba(255,255,255,0.08); background:transparent; flex-wrap:wrap; }}
    .panel-stage-title {{ font-size:22px; font-weight:800; }}
    .panel-stage-sub {{ color:var(--muted); font-size:13px; }}
    .panel-stage-body {{ min-height:calc(100vh - 190px); padding:0; }}
    .panel-frame {{ width:100%; min-height:66vh; border:0; border-radius:16px; background:#0b1020; }}
    .panel-frame-wrap {{ border:1px solid var(--border); border-radius:16px; overflow:hidden; background:#0b1020; min-height:68vh; }}
    .messenger-frame-wrap {{ border:1px solid var(--border); border-radius:16px; overflow:hidden; background:#0b1020; height:72vh; }}
    .messenger-frame {{ width:100%; height:calc(100% + 68px); margin-top:-68px; border:0; display:block; background:#0b1020; }}
    .helper-grid {{ display:grid; grid-template-columns:repeat(3,minmax(0,1fr)); gap:14px; }}
    .helper-card {{ border:1px solid var(--border); border-radius:16px; background:#11182d; padding:14px; }}
    .helper-card h3 {{ margin:0 0 10px 0; }}
    .center-inline-search {{ display:flex; gap:8px; width:min(520px,100%); }}
    .panel-switch {{ text-align:left; width:100%; }}
    .panel-switch strong {{ pointer-events:none; }}
    .panel-view {{ display:none; }}
    .panel-view.is-active {{ display:block; }}
    .admin-messenger {{ display:grid; grid-template-columns:300px minmax(0,1fr); gap:14px; }}
    .admin-messenger-sidebar, .admin-messenger-main {{ border:1px solid var(--border); border-radius:16px; background:#0b1020; }}
    .admin-messenger-sidebar {{ padding:12px; display:flex; flex-direction:column; gap:10px; min-width:0; }}
    .admin-chat-list {{ display:flex; flex-direction:column; gap:8px; max-height:68vh; overflow:auto; }}
    .center-chat-item {{ width:100%; text-align:left; background:#11182d; border:1px solid var(--border); border-radius:14px; padding:12px; }}
    .center-chat-item.is-active {{ background:#1b2850; border-color:#4d67b1; }}
    .center-chat-title {{ font-weight:700; margin-bottom:4px; word-break:break-word; }}
    .center-chat-meta {{ color:var(--muted); font-size:12px; }}
    .admin-messenger-main {{ min-width:0; overflow:hidden; }}
    .admin-chat-header {{ display:flex; justify-content:space-between; align-items:center; gap:12px; padding:14px 16px; border-bottom:1px solid var(--border); flex-wrap:wrap; }}
    .admin-chat-feed {{ padding:14px; max-height:68vh; overflow:auto; }}
    .ai-form {{ display:flex; flex-direction:column; gap:14px; }}
    .ai-grid {{ display:grid; grid-template-columns:repeat(2,minmax(0,1fr)); gap:12px; }}
    .ai-grid label, .ai-form label {{ display:flex; flex-direction:column; gap:7px; color:#dbe6ff; font-weight:700; }}
    .ai-check {{ flex-direction:row !important; align-items:center; gap:8px !important; border:1px solid var(--border); border-radius:14px; background:#11182d; padding:12px; }}
    .ai-form input[type=text], .ai-form input[type=number], .ai-form select, .ai-form textarea {{ width:100%; max-width:none; }}
    .utc-note {{ margin-left:auto; color:var(--muted); font-size:12px; }}

    .admin-users-shell {{ display:flex; flex-direction:column; gap:10px; }}
    .admin-users-titlebar {{ display:flex; align-items:center; justify-content:space-between; min-height:34px; }}
    .admin-users-titlebar strong {{ font-size:18px; letter-spacing:-0.02em; }}
    .admin-users-grid {{ display:grid; grid-template-columns:330px minmax(0,1fr); gap:12px; min-height:58vh; }}
    .admin-users-list, .admin-users-detail {{ border:1px solid var(--border); border-radius:18px; background:#0b1020; min-width:0; }}
    .admin-users-list {{ padding:12px; display:flex; flex-direction:column; gap:10px; }}
    .admin-user-tabs {{ display:flex; gap:8px; flex-wrap:wrap; }}
    .admin-user-tabs a {{ color:#dbe6ff; text-decoration:none; border:1px solid var(--border); background:#121933; border-radius:999px; padding:7px 10px; font-size:12px; font-weight:800; }}
    .admin-user-tabs a.active {{ background:#1b2850; border-color:#4d67b1; }}
    .admin-user-search {{ display:flex; gap:8px; width:100%; }}
    .admin-user-search input[type=text] {{ max-width:none; flex:1 1 auto; }}
    .admin-user-search button {{ flex:0 0 auto; }}
    .admin-user-list-scroll {{ display:flex; flex-direction:column; gap:8px; overflow:auto; max-height:62vh; padding-right:3px; }}
    .admin-user-row {{ display:flex; align-items:center; gap:10px; border:1px solid var(--border); border-radius:16px; background:#11182d; padding:10px; color:var(--text); text-decoration:none; min-width:0; }}
    .admin-user-row:hover {{ background:#15203a; border-color:#36508d; }}
    .admin-user-row.active {{ background:#172241; border-color:#4d67b1; box-shadow:inset 0 0 0 1px rgba(255,255,255,.04); }}
    .admin-user-row-avatar, .admin-user-avatar {{ width:38px; height:38px; border-radius:50%; flex:0 0 auto; overflow:hidden; display:flex; align-items:center; justify-content:center; border:1px solid #314068; background:radial-gradient(circle at 30% 30%, #314a88, #11182d 70%); color:#fff; font-weight:900; }}
    .admin-user-row-avatar-img, .admin-user-avatar-img {{ width:100%; height:100%; object-fit:cover; display:block; }}
    .admin-user-row-main {{ min-width:0; flex:1; }}
    .admin-user-row-name {{ font-weight:900; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }}
    .admin-user-row-meta {{ color:var(--muted); font-size:12px; margin-top:2px; }}
    .admin-user-pill {{ display:inline-flex; align-items:center; justify-content:center; border-radius:999px; border:1px solid var(--border); padding:5px 9px; font-size:12px; font-weight:800; white-space:nowrap; }}
    .admin-user-pill.online {{ background:#10261b; color:#98e2b8; border-color:#28533a; }}
    .admin-user-pill.offline {{ background:#151b2f; color:#b8c3e4; border-color:#2e3a63; }}
    .admin-user-pill.banned {{ background:#251316; color:#ffb4bf; border-color:#5a2730; }}
    .admin-user-pill.clear {{ background:#141e37; color:#aecdff; border-color:#314a84; }}
    .admin-user-pill.review {{ background:#2b1b10; color:#ffd19a; border-color:#73522b; }}
    .admin-users-detail {{ padding:14px; }}
    .admin-user-card {{ display:flex; flex-direction:column; gap:12px; }}
    .admin-user-card-head {{ display:flex; justify-content:space-between; align-items:flex-start; gap:12px; }}
    .admin-user-card-ident {{ display:flex; align-items:center; gap:12px; min-width:0; }}
    .admin-user-avatar {{ width:56px; height:56px; font-size:22px; cursor:pointer; padding:0; }}
    .admin-user-card-title {{ min-width:0; }}
    .admin-user-name {{ font-size:28px; font-weight:900; letter-spacing:-0.03em; line-height:1; word-break:break-word; }}
    .admin-user-sub {{ color:var(--muted); margin-top:6px; word-break:break-word; }}
    .admin-user-pills {{ display:flex; gap:8px; flex-wrap:wrap; margin-top:10px; }}
    .admin-user-gear {{ width:38px; height:38px; padding:0; border-radius:12px; }}
    .admin-user-info-grid {{ display:grid; grid-template-columns:repeat(2,minmax(0,1fr)); gap:10px; }}
    .admin-user-info-grid.compact {{ grid-template-columns:repeat(2,minmax(0,1fr)); }}
    .admin-user-info {{ border:1px solid var(--border); border-radius:16px; background:#11182d; padding:12px; min-width:0; }}
    .admin-user-info span {{ display:block; color:var(--muted); font-size:11px; text-transform:uppercase; letter-spacing:.07em; margin-bottom:7px; }}
    .admin-user-info strong {{ display:block; word-break:break-word; }}
    .admin-user-section {{ border:1px solid var(--border); border-radius:16px; background:#11182d; padding:12px; }}
    .admin-user-section-head {{ display:flex; justify-content:space-between; gap:10px; margin-bottom:10px; }}
    .admin-user-section-head span {{ color:var(--muted); font-size:12px; }}
    .admin-report-list {{ display:flex; flex-direction:column; gap:8px; }}
    .admin-report-row {{ border:1px solid #2f3d68; border-radius:14px; background:#0d1324; padding:10px; }}
    .admin-report-top {{ display:flex; justify-content:space-between; gap:8px; align-items:center; margin-bottom:7px; }}
    .admin-report-reason {{ color:#ffb4bf; font-weight:900; font-size:12px; }}
    .admin-report-status {{ border:1px solid var(--border); border-radius:999px; padding:4px 8px; font-size:11px; font-weight:800; }}
    .admin-report-status.open {{ background:#2a1d10; color:#ffc98f; border-color:#6f4b24; }}
    .admin-report-status.done {{ background:#10261b; color:#98e2b8; border-color:#28533a; }}
    .admin-report-text {{ line-height:1.45; }}
    .admin-report-meta {{ color:var(--muted); font-size:12px; margin-top:6px; }}
    .admin-report-actions {{ display:flex; gap:8px; flex-wrap:wrap; margin-top:8px; }}
    .admin-report-actions form {{ margin:0; }}
    .admin-report-actions button {{ padding:7px 10px; font-size:12px; }}
    .admin-suggestions-shell {{ display:grid; grid-template-columns:minmax(0,1fr) 300px; gap:14px; align-items:start; }}
    .admin-suggestion-list {{ display:flex; flex-direction:column; gap:10px; }}
    .admin-suggestion-card {{ border:1px solid var(--border); border-radius:16px; background:#0d1324; padding:14px; }}
    .admin-suggestion-head {{ display:flex; justify-content:space-between; gap:12px; align-items:flex-start; flex-wrap:wrap; margin-bottom:10px; }}
    .admin-suggestion-title {{ font-size:17px; font-weight:900; word-break:break-word; }}
    .admin-suggestion-meta {{ color:var(--muted); font-size:12px; margin-top:4px; word-break:break-word; }}
    .admin-suggestion-text {{ line-height:1.55; word-break:break-word; white-space:pre-wrap; }}
    .admin-suggestion-actions {{ display:flex; gap:8px; margin-top:12px; align-items:flex-start; flex-wrap:wrap; }}
    .admin-suggestion-action-form {{ display:flex; gap:8px; align-items:flex-start; flex:1 1 420px; margin:0; min-width:min(100%,420px); }}
    .admin-suggestion-actions textarea {{ flex:1 1 240px; max-width:none; min-height:40px; resize:vertical; }}
    .admin-suggestion-actions form.inline-form {{ margin:0; }}
    .admin-suggestion-actions button {{ white-space:nowrap; }}
    .admin-suggestion-side {{ border:1px solid var(--border); border-radius:16px; background:#11182d; padding:14px; position:sticky; top:86px; }}
    .admin-gif-shell {{ display:grid; grid-template-columns:300px minmax(0,1fr); gap:14px; align-items:start; }}
    .admin-gif-upload {{ border:1px solid var(--border); border-radius:16px; background:#11182d; padding:14px; position:sticky; top:86px; }}
    .admin-gif-upload form {{ display:flex; flex-direction:column; gap:10px; }}
    .admin-gif-upload input[type=file] {{ width:100%; color:var(--muted); }}
    .admin-gif-grid {{ display:grid; grid-template-columns:repeat(auto-fill,minmax(180px,1fr)); gap:12px; }}
    .admin-gif-card {{ border:1px solid var(--border); border-radius:16px; background:#0d1324; overflow:hidden; min-width:0; }}
    .admin-gif-thumb {{ aspect-ratio:1.45/1; background:#060914; display:flex; align-items:center; justify-content:center; }}
    .admin-gif-thumb img {{ width:100%; height:100%; object-fit:contain; display:block; }}
    .admin-gif-body {{ padding:10px; display:flex; flex-direction:column; gap:8px; }}
    .admin-gif-name {{ font-weight:900; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }}
    .admin-gif-actions {{ display:flex; gap:8px; flex-wrap:wrap; }}
    .admin-gif-actions form {{ margin:0; }}
    .admin-download-shell {{ display:grid; grid-template-columns:300px minmax(0,1fr); gap:14px; align-items:start; }}
    .admin-download-upload {{ border:1px solid var(--border); border-radius:16px; background:#11182d; padding:14px; position:sticky; top:86px; }}
    .admin-download-upload form {{ display:flex; flex-direction:column; gap:10px; }}
    .admin-download-upload input[type=file] {{ width:100%; color:var(--muted); }}
    .admin-download-grid {{ display:grid; grid-template-columns:repeat(auto-fill,minmax(240px,1fr)); gap:12px; }}
    .admin-download-card {{ border:1px solid var(--border); border-radius:16px; background:#0d1324; padding:14px; display:flex; flex-direction:column; gap:10px; min-width:0; }}
    .admin-download-top {{ display:flex; justify-content:space-between; gap:10px; align-items:flex-start; }}
    .admin-download-title {{ font-weight:900; font-size:16px; word-break:break-word; }}
    .admin-download-meta {{ color:var(--muted); font-size:12px; line-height:1.45; word-break:break-word; }}
    .admin-download-actions {{ display:flex; gap:8px; flex-wrap:wrap; margin-top:auto; }}
    .admin-download-actions form {{ margin:0; }}
    .admin-download-actions a {{ text-decoration:none; border:1px solid var(--border); background:#151d37; color:var(--text); border-radius:14px; padding:10px 14px; font-weight:800; }}
    .admin-user-actions {{ display:grid; grid-template-columns:repeat(3,minmax(0,1fr)); gap:10px; }}
    .admin-user-actions form {{ margin:0; }}
    .admin-ban-reason-input {{ width:100%; max-width:none; margin-bottom:8px; padding:9px 10px; border-radius:12px; border:1px solid var(--border); background:#090d18; color:var(--text); }}
    .admin-user-actions button {{ width:100%; }}
    .btn-ok {{ background:#122b1c; border-color:#28533a; color:#baf0cf; }}
    .btn-ok:hover {{ background:#173625; }}
    .admin-user-emptyline, .admin-user-empty-detail {{ border:1px dashed var(--border); border-radius:14px; background:#0f1527; color:var(--muted); padding:18px; text-align:center; }}
    .admin-user-empty-detail {{ min-height:360px; display:flex; align-items:center; justify-content:center; }}
    .admin-modal-backdrop {{ position:fixed; inset:0; z-index:1000; background:rgba(0,0,0,.58); display:flex; align-items:center; justify-content:center; padding:20px; }}
    .admin-modal-window {{ width:min(760px,100%); max-height:90vh; overflow:auto; border:1px solid var(--border); border-radius:22px; background:#0b1020; box-shadow:0 24px 80px rgba(0,0,0,.45); }}
    .admin-modal-head {{ display:flex; justify-content:space-between; align-items:flex-start; gap:12px; border-bottom:1px solid var(--border); padding:16px; }}
    .admin-modal-title {{ font-size:24px; font-weight:900; letter-spacing:-.02em; }}
    .admin-modal-sub {{ color:var(--muted); margin-top:4px; }}
    .admin-modal-close {{ width:36px; height:36px; padding:0; }}
    .admin-modal-body {{ padding:16px; display:flex; flex-direction:column; gap:14px; }}
    .admin-modal-avatar {{ border:1px solid var(--border); border-radius:18px; background:#11182d; min-height:260px; display:flex; align-items:center; justify-content:center; overflow:hidden; }}
    .admin-modal-avatar-img {{ max-width:100%; max-height:420px; object-fit:contain; display:block; }}
    .admin-modal-avatar-empty {{ width:120px; height:120px; border-radius:50%; display:flex; align-items:center; justify-content:center; font-size:42px; font-weight:900; background:#172241; border:1px solid #314068; }}
    .admin-modal-actions {{ display:flex; gap:10px; flex-wrap:wrap; }}
    .admin-modal-actions a {{ text-decoration:none; border:1px solid var(--border); background:#121933; color:#eef2ff; border-radius:14px; padding:10px 12px; font-weight:800; }}
    .admin-user-toast {{ position:fixed; right:18px; bottom:18px; z-index:1200; max-width:min(420px,calc(100vw - 36px)); padding:12px 14px; border-radius:14px; border:1px solid #5a2730; background:#2d1420; color:#ffd7df; box-shadow:0 18px 50px rgba(0,0,0,.38); font-weight:800; }}
    .admin-user-toast[data-kind='ok'] {{ border-color:#28533a; background:#10261b; color:#baf0cf; }}
    .admin-user-toast[hidden] {{ display:none; }}
    @media (max-width: 980px) {{ .user-card, .server-row-card, .center-shell, .center-workspace, .panel-shell, .helper-grid, .user-fields, .admin-messenger, .admin-users-grid, .admin-user-info-grid, .admin-user-actions, .admin-suggestions-shell, .admin-gif-shell, .admin-download-shell {{ grid-template-columns:1fr; }} .user-actions, .server-actions {{ justify-content:flex-start; }} .admin-user-list-scroll {{ max-height:none; }} .admin-suggestion-side, .admin-gif-upload, .admin-download-upload {{ position:static; }} }}
    @media (max-width: 640px) {{ main.admin-main {{ padding:12px; }} header {{ padding:12px; }} .brand {{ font-size:22px; }} .search-row form {{ flex-direction:column; }} .search-row input[type=text], .search-row button {{ width:100%; }} .messenger-frame {{ margin-top:-62px; height:calc(100% + 62px); }} .admin-suggestion-action-form {{ flex-direction:column; }} .admin-suggestion-actions textarea, .admin-suggestion-actions button {{ width:100%; }} }}
    </style>
    </head>
    <body>
    <header>
    <div class='brand'>Админ-панель</div>
    <nav class='nav'>{nav}</nav>
    <div class='utc-note'>Время везде: UTC</div>
    {logout_html}
    </header>
    <main class='{main_class}'>
    {msg_html}
    {body}
    </main>
    {center_script}{users_script}</body>
    </html>"#,
        title = escape_html(title),
        nav = nav,
        msg_html = msg_html,
        body = body,
        main_class = main_class,
        center_script = center_script,
        users_script = users_script,
        logout_html = logout_html
    );

    Html(html)
}

pub(super) fn embedded_page(title: &str, body: &str, msg: Option<&str>) -> Html<String> {
    let mut html = page(title, body, msg).0;
    html = html.replacen("<header>", "<header style='display:none'>", 1);
    html = html.replacen("<main class='admin-main'>", "<main class='admin-main' style='max-width:none;padding:12px;'>", 1);
    Html(html)
}

// moved to content.rs

// =============================
// Auth/session
// =============================

pub(super) fn require_admin_panel_enabled() -> Result<(), (StatusCode, String)> {
    if !admin_enabled() {
        return Err((
            StatusCode::NOT_FOUND,
            "Админка отключена (включи LB_ENABLE_ADMIN_PANEL=1 или настрой LB_ADMIN_PASSWORD[_HASH])".to_string(),
        ));
    }
    Ok(())
}

fn session_get(st: &AppState, headers: &HeaderMap) -> Option<(String, AdminSession)> {
    let sid = cookie_get(headers, "lb_admin_sid")?;
    let entry = st.admin_sessions.get(&sid)?;
    let s = entry.clone();
    drop(entry);
    if s.expires_at < now_ts() {
        st.admin_sessions.remove(&sid);
        return None;
    }
    Some((sid, s))
}

pub(super) fn require_auth(st: &AppState, headers: &HeaderMap) -> Result<(String, AdminSession), Redirect> {
    match session_get(st, headers) {
        Some(v) => Ok(v),
        None => Err(Redirect::to("/admin/login")),
    }
}

pub(super) fn require_allow_ip(st: &AppState, headers: &HeaderMap, peer: Option<SocketAddr>) -> Result<(), (StatusCode, String)> {
    // Optional simple allow-list by exact IPs.
    // NOTE: If you run behind proxy, ensure X-Forwarded-For is trusted.
    let allow = env::var("LB_ADMIN_ALLOW_IPS").unwrap_or_default();
    let allow = allow.trim();
    if allow.is_empty() {
        return Ok(());
    }

    // Determine client IP but only trust forwarded headers when peer is trusted.
    let remote = rate_limit::extract_ip(
        headers,
        peer.map(|p| p.ip()),
        st.trusted_proxies.as_slice(),
    );

    let Some(remote) = remote else {
        return Err((StatusCode::FORBIDDEN, "Включён список разрешённых IP, но IP не обнаружен".to_string()));
    };

    let ip: IpAddr = remote
        .parse()
        .map_err(|_| (StatusCode::FORBIDDEN, "Некорректный IP клиента".to_string()))?;

    for a in allow.split(',') {
        let a = a.trim();
        if a.is_empty() {
            continue;
        }
        if let Ok(x) = a.parse::<IpAddr>() {
            if x == ip {
                return Ok(());
            }
        }
    }

    Err((StatusCode::FORBIDDEN, "Доступ с этого IP запрещён".to_string()))
}

fn new_session(st: &AppState) -> (String, AdminSession) {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use rand::Rng;

    let mut buf = [0u8; 32];
    rand::rng().fill_bytes(&mut buf);
    let sid = URL_SAFE_NO_PAD.encode(buf);

    let mut csrf_buf = [0u8; 16];
    rand::rng().fill_bytes(&mut csrf_buf);
    let csrf = URL_SAFE_NO_PAD.encode(csrf_buf);

    let expires_at = (Utc::now() + ChronoDuration::hours(8)).timestamp();
    let sess = AdminSession { expires_at, csrf };

    st.admin_sessions.insert(sid.clone(), sess.clone());
    (sid, sess)
}

fn cookie_for_session(sid: &str, secure: bool) -> String {
    // Make admin session cookie persistent for 8 hours to improve redirect behavior
    // Use SameSite=None when secure cookies are required by the browser environment.
    let same_site = if secure { "None" } else { "Lax" };
    let mut c = format!("lb_admin_sid={sid}; Path=/; Max-Age=28800; HttpOnly; SameSite={same_site}");
    if secure {
        c.push_str("; Secure");
    }
    c
}

fn cookie_clear(secure: bool) -> String {
    let same_site = if secure { "None" } else { "Lax" };
    let mut c = format!("lb_admin_sid=deleted; Path=/; Max-Age=0; HttpOnly; SameSite={same_site}");
    if secure {
        c.push_str("; Secure");
    }
    c
}

// =============================
// Pages
// =============================

async fn root(State(st): State<AppState>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) {
        return e.into_response();
    }
    if session_get(&st, &headers).is_none() {
        return Redirect::to("/admin/login").into_response();
    }
    Redirect::to("/admin/center").into_response()
}

#[derive(Deserialize, Default)]
pub(super) struct MsgQuery {
    msg: Option<String>,
    embed: Option<u8>,
    view: Option<String>,
    q: Option<String>,
    mode: Option<String>,
    status: Option<String>,
    user_id: Option<i64>,
}

async fn login_get(State(st): State<AppState>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Query(q): Query<MsgQuery>) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) {
        return e.into_response();
    }
    if session_get(&st, &headers).is_some() {
        return Redirect::to("/admin/center").into_response();
    }

    let warn = if !admin_password_configured() {
        "<div class='warn'>LB_ADMIN_PASSWORD_HASH / LB_ADMIN_PASSWORD не настроен. Опасные действия для обычных пользователей и серверов будут заблокированы.</div>"
    } else {
        ""
    };

    let body = format!(
        r#"<div class='card'>
        <h2>Вход</h2>
        <p class='small'>Admin listener: <code>{base}</code></p>
        <form id='admin-login-form' method='post' action='/admin/login'>
        <div class='small'>Пароль администратора</div>
        <input type='password' name='password' autocomplete='current-password' required />
        <div style='height:10px'></div>
        <button type='submit'>Войти</button>
        </form>
        {warn}
        </div>"#, 
        base = escape_html(&crate::routes::pages::admin_panel_base_url()),
        warn = warn
    );

    page("Админка • Вход", &body, q.msg.as_deref()).into_response()
}

#[derive(Deserialize)]
struct LoginForm {
    password: String,
}

async fn login_post(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) {
        return e.into_response();
    }

    if let Err(err) = verify_admin_password(&form.password) {
        tracing::warn!("[ADMIN] Login failed: {err}");
        return admin_redirect_with_msg("/admin/login", &format!("{}", err)).into_response();
    }

    let (sid, _sess) = new_session(&st);
    let cookie = cookie_for_session(&sid, admin_cookie_secure(&headers));
    let raw_cookie = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()).unwrap_or("NONE");
    tracing::info!("[ADMIN] Checking auth. Raw Cookie header: {}", raw_cookie);

    let mut response = Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, "/admin/center")
        .body(Body::empty())
        .expect("admin login redirect response");

    set_cookie(response.headers_mut(), cookie);

    response
}

async fn logout_post(State(st): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Some((sid, _)) = session_get(&st, &headers) {
        st.admin_sessions.remove(&sid);
    }

    let mut response = Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, "/admin/login")
        .body(Body::empty())
        .expect("admin logout redirect response");

    set_cookie(response.headers_mut(), cookie_clear(admin_cookie_secure(&headers)));

    response
}

// =============================
// Пользователи list + actions
// =============================

#[derive(Deserialize, Default)]
pub(crate) struct ListQuery {
    q: Option<String>,
    msg: Option<String>,
    embed: Option<u8>,
    return_to: Option<String>,
    mode: Option<String>,
    user_id: Option<i64>,
}


pub(super) fn safe_admin_return_to(input: &str, fallback: &str) -> String {
    let s = input.trim();
    if s.starts_with("/admin/") && !s.contains("\n") && !s.contains("\r") {
        s.to_string()
    } else {
        fallback.to_string()
    }
}

// moved to users.rs

fn normalized_suggestion_status(input: Option<&str>) -> &'static str {
    match input.unwrap_or("open").trim().to_ascii_lowercase().as_str() {
        "all" => "all",
        "reviewed" => "reviewed",
        "rejected" => "rejected",
        _ => "open",
    }
}

fn suggestion_status_label(status: &str) -> &'static str {
    match status {
        "reviewed" => "Просмотрено",
        "rejected" => "Отклонено",
        _ => "Новое",
    }
}

fn suggestion_status_class(status: &str) -> &'static str {
    match status {
        "reviewed" => "done",
        "rejected" => "done",
        _ => "open",
    }
}

fn suggestions_page_url(base_path: &str, embedded: bool, status: &str) -> String {
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    if embedded {
        ser.append_pair("view", "suggestions");
    }
    if status != "open" {
        ser.append_pair("status", status);
    }
    let query = ser.finish();
    if query.is_empty() { base_path.to_string() } else { format!("{base_path}?{query}") }
}

fn render_suggestions_panel_body(
    sess: &AdminSession,
    suggestions: &[UserSuggestionRow],
    status: &str,
    embedded: bool,
    current_return_to: &str,
) -> String {
    let base_path = if embedded { "/admin/center" } else { "/admin/suggestions" };
    let status = normalized_suggestion_status(Some(status));
    let open_href = suggestions_page_url(base_path, embedded, "open");
    let all_href = suggestions_page_url(base_path, embedded, "all");
    let reviewed_href = suggestions_page_url(base_path, embedded, "reviewed");
    let rejected_href = suggestions_page_url(base_path, embedded, "rejected");

    let mut rows_html = String::new();
    if suggestions.is_empty() {
        rows_html.push_str("<div class='empty-state'>Предложений в этом статусе пока нет.</div>");
    } else {
        for item in suggestions {
            let title = if item.title.trim().is_empty() {
                format!("Предложение #{}", item.id)
            } else {
                item.title.clone()
            };
            let reviewed = item.reviewed_at.as_deref().unwrap_or("").trim();
            let reviewed_html = if reviewed.is_empty() {
                String::new()
            } else {
                format!("<div class='admin-suggestion-meta'>Рассмотрено: {}</div>", escape_html(&fmt_admin_dt(reviewed)))
            };
            let admin_note_html = if item.admin_note.trim().is_empty() {
                String::new()
            } else {
                format!("<div class='admin-suggestion-meta'>Заметка: {}</div>", escape_html(&item.admin_note))
            };
            let note_value = escape_html(&item.admin_note);
            let actions = if item.status == "open" {
                format!(
                    r#"<div class='admin-suggestion-actions'>
  <form method='post' action='/admin/suggestions/{id}/status' class='admin-suggestion-action-form'>
    <input type='hidden' name='csrf' value='{csrf}' />
    <input type='hidden' name='return_to' value='{return_to}' />
    <input type='hidden' name='status' value='reviewed' />
    <textarea name='admin_note' rows='2' placeholder='Заметка администратора'>{note}</textarea>
    <button type='submit' class='btn-soft'>Просмотрено</button>
  </form>
  <form method='post' action='/admin/suggestions/{id}/status' class='inline-form'>
    <input type='hidden' name='csrf' value='{csrf}' />
    <input type='hidden' name='return_to' value='{return_to}' />
    <input type='hidden' name='status' value='rejected' />
    <button type='submit' class='btn-soft'>Отклонить</button>
  </form>
</div>"#,
                    id = item.id,
                    csrf = escape_html(&sess.csrf),
                    return_to = escape_html(current_return_to),
                    note = note_value,
                )
            } else {
                format!(
                    r#"<div class='admin-suggestion-actions'>
  <form method='post' action='/admin/suggestions/{id}/status' class='inline-form'>
    <input type='hidden' name='csrf' value='{csrf}' />
    <input type='hidden' name='return_to' value='{return_to}' />
    <input type='hidden' name='status' value='open' />
    <button type='submit' class='btn-soft'>Вернуть в новые</button>
  </form>
</div>"#,
                    id = item.id,
                    csrf = escape_html(&sess.csrf),
                    return_to = escape_html(current_return_to),
                )
            };

            rows_html.push_str(&format!(
                r#"<article class='admin-suggestion-card'>
  <div class='admin-suggestion-head'>
    <div>
      <div class='admin-suggestion-title'>{title}</div>
      <div class='admin-suggestion-meta'>От: #{user_id} {username} · {created_at}</div>
      {reviewed_html}
    </div>
    <span class='admin-report-status {status_class}'>{status_label}</span>
  </div>
  <div class='admin-suggestion-text'>{message}</div>
  {admin_note_html}
  {actions}
</article>"#,
                title = escape_html(&title),
                user_id = item.user_id,
                username = escape_html(&item.username),
                created_at = escape_html(&fmt_admin_dt(&item.created_at)),
                reviewed_html = reviewed_html,
                status_class = suggestion_status_class(&item.status),
                status_label = suggestion_status_label(&item.status),
                message = escape_html(&item.message),
                admin_note_html = admin_note_html,
                actions = actions,
            ));
        }
    }

    let current_label = match status {
        "all" => "Все",
        "reviewed" => "Просмотрено",
        "rejected" => "Отклонено",
        _ => "Новые",
    };

    format!(
        r#"<div class='card'>
  <div class='search-row'>
    <div class='hstack'>
      <h2 style='margin:0;'>Предложения</h2>
      <span class='pill'>{current_label}</span>
    </div>
    <div class='admin-user-tabs'>
      <a href='{open_href}' class='{open_cls}'>Новые</a>
      <a href='{all_href}' class='{all_cls}'>Все</a>
      <a href='{reviewed_href}' class='{reviewed_cls}'>Просмотрено</a>
      <a href='{rejected_href}' class='{rejected_cls}'>Отклонено</a>
    </div>
  </div>
</div>
<div class='admin-suggestions-shell'>
  <section class='admin-suggestion-list'>{rows_html}</section>
  <aside class='admin-suggestion-side'>
    <h3 style='margin:0 0 8px;'>Просмотр</h3>
    <div class='small'>Здесь отображаются идеи, отправленные пользователями из настроек. Новые предложения можно пометить просмотренными или отклонёнными.</div>
  </aside>
</div>"#,
        current_label = current_label,
        open_href = escape_html(&open_href),
        all_href = escape_html(&all_href),
        reviewed_href = escape_html(&reviewed_href),
        rejected_href = escape_html(&rejected_href),
        open_cls = if status == "open" { "active" } else { "" },
        all_cls = if status == "all" { "active" } else { "" },
        reviewed_cls = if status == "reviewed" { "active" } else { "" },
        rejected_cls = if status == "rejected" { "active" } else { "" },
        rows_html = rows_html,
    )
}


#[derive(Deserialize)]
pub(crate) struct ActionForm {
    pub csrf: String,
    #[serde(default)] pub phrase: String,
    #[serde(default)] pub admin_password: String,
    #[serde(default)] pub return_to: String,
    #[serde(default)] pub reason: String,
    #[serde(default)] pub user_id: String,
}


#[derive(Deserialize)]
struct ReportStatusForm {
    csrf: String,
    status: String,
    #[serde(default)]
    return_to: String,
}

#[derive(Deserialize, Default)]
struct SuggestionsQuery {
    msg: Option<String>,
    status: Option<String>,
    embed: Option<u8>,
    return_to: Option<String>,
}

#[derive(Deserialize)]
struct SuggestionStatusForm {
    csrf: String,
    status: String,
    #[serde(default)]
    admin_note: String,
    #[serde(default)]
    return_to: String,
}

async fn admin_report_status(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ReportStatusForm>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() { return e.into_response(); }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) { return e.into_response(); }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let return_to = safe_admin_return_to(&f.return_to, "/admin/users");
    if f.csrf != sess.csrf {
        return admin_redirect_with_msg(&return_to, "CSRF-токен не совпадает").into_response();
    }
    let status = match f.status.trim() {
        "open" => "open",
        "reviewed" => "reviewed",
        "rejected" => "rejected",
        _ => "reviewed",
    };
    let now = auth::now_iso();
    let res = if status == "open" {
        sqlx::query("UPDATE user_reports SET status = 'open', resolved_at = NULL, resolved_by = NULL WHERE id = $1")
            .bind(id)
            .execute(&st.db)
            .await
    } else {
        sqlx::query("UPDATE user_reports SET status = $1, resolved_at = $2, resolved_by = NULL WHERE id = $3")
            .bind(status)
            .bind(&now)
            .bind(id)
            .execute(&st.db)
            .await
    };
    match res {
        Ok(_) => admin_redirect_with_msg(&return_to, "Готово").into_response(),
        Err(e) => admin_redirect_with_msg(&return_to, &format!("Ошибка: {e}")).into_response(),
    }
}

async fn suggestions_page(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<SuggestionsQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let status = normalized_suggestion_status(q.status.as_deref());
    let embed = q.embed == Some(1);
    let fallback_return_to = suggestions_page_url(if embed { "/admin/center" } else { "/admin/suggestions" }, embed, status);
    let return_to = safe_admin_return_to(q.return_to.as_deref().unwrap_or(""), &fallback_return_to);
    let body = match fetch_suggestions(&st.db, status, 200).await {
        Ok(list) => render_suggestions_panel_body(&sess, &list, status, embed, &return_to),
        Err(err) => format!(
            "<div class='card'><div class='empty-state'>Ошибка БД: {}</div></div>",
            escape_html(&format!("{}", err))
        ),
    };

    if embed {
        embedded_page("Админка • Предложения", &body, q.msg.as_deref()).into_response()
    } else {
        page("Админка • Предложения", &body, q.msg.as_deref()).into_response()
    }
}

async fn admin_suggestion_status(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<SuggestionStatusForm>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let return_to = safe_admin_return_to(&f.return_to, "/admin/suggestions");
    if f.csrf != sess.csrf {
        return admin_redirect_with_msg(&return_to, "CSRF-токен не совпадает").into_response();
    }

    let status = match f.status.trim() {
        "open" => "open",
        "rejected" => "rejected",
        "reviewed" => "reviewed",
        _ => "reviewed",
    };
    let note: String = f.admin_note.trim().chars().take(800).collect();
    let now = auth::now_iso();
    let res = if status == "open" {
        sqlx::query(
            "UPDATE user_suggestions SET status = 'open', reviewed_at = NULL, reviewed_by = NULL WHERE id = $1",
        )
        .bind(id)
        .execute(&st.db)
        .await
    } else {
        sqlx::query(
            "UPDATE user_suggestions SET status = $1, reviewed_at = $2, reviewed_by = NULL, admin_note = $3 WHERE id = $4",
        )
        .bind(status)
        .bind(&now)
        .bind(&note)
        .bind(id)
        .execute(&st.db)
        .await
    };

    match res {
        Ok(_) => admin_redirect_with_msg(&return_to, "Готово").into_response(),
        Err(e) => admin_redirect_with_msg(&return_to, &format!("Ошибка: {e}")).into_response(),
    }
}

// moved to content.rs



// =============================
// Серверы list + actions
// =============================

// moved to servers.rs

// moved to servers.rs


// moved to servers.rs


// =============================
// Инструменты базы данных
// =============================

// moved to content.rs





async fn center_page(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<MsgQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let users_total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users").fetch_one(&st.db).await.unwrap_or(0);
    let servers_total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM servers").fetch_one(&st.db).await.unwrap_or(0);
    // messages count removed from admin center (messages are E2EE)
    let banned_total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE is_banned = TRUE").fetch_one(&st.db).await.unwrap_or(0);

    let users_query = q.q.clone().unwrap_or_default().trim().to_string();
    let users = fetch_users(&st.db, &users_query, 200).await.unwrap_or_default();
    let users_mode = normalized_user_mode(q.mode.as_deref());
    let selected_id = q.user_id.or_else(|| users.iter().find(|u| user_mode_matches(u, users_mode)).map(|u| u.id));
    let users_return_to = user_page_url("/admin/center", true, &users_query, users_mode, selected_id);
    let user_reports = match selected_id {
        Some(id) => fetch_user_reports(&st.db, id, 8).await.unwrap_or_default(),
        None => Vec::new(),
    };
    let selected_servers = match selected_id {
        Some(id) => fetch_user_servers(&st.db, id).await.unwrap_or_default(),
        None => Vec::new(),
    };
    let users_panel = render_users_panel_body(
        &sess,
        &users,
        &users::UsersPanelFilter {
            query: &users_query,
            mode: users_mode,
            requested_user_id: selected_id,
            current_return_to: &users_return_to,
        },
        true,
        &user_reports,
        &selected_servers,
    );

    let servers = fetch_servers(&st.db, "", 200).await.unwrap_or_default();
    let mut server_cards = String::new();
    for s in servers.iter() {
        let filter = format!("#{} {} {}", s.id, s.name.to_lowercase(), s.owner_username.to_lowercase());
        server_cards.push_str(&format!(
            r#"<div class='server-row-card' data-filter-item='servers' data-filter='{filter}'>
  <div class='server-main'>
    <div class='server-top'>
      <div class='server-title'>
        <span class='server-id'>#{id}</span>
        <span class='server-name'>{name}</span>
      </div>
      <div class='user-meta'>Создан: {created_at}</div>
    </div>
    <div class='server-meta'>Владелец: #{owner_id} {owner_name}</div>
  </div>
  <div class='server-actions'>
    <form method='post' action='/admin/servers/{id}/add_all_users' class='inline-form'>
      <input type='hidden' name='csrf' value='{csrf}' />
      <input type='hidden' name='return_to' value='/admin/center?view=servers' />
      <button type='submit' class='btn-soft'>Добавить всех пользователей</button>
    </form>
    <form method='post' action='/admin/servers/{id}/delete' class='inline-form'>
      <input type='hidden' name='csrf' value='{csrf}' />
      <input type='hidden' name='return_to' value='/admin/center' />
      <button type='submit' class='btn-danger'>Удалить сервер</button>
    </form>
  </div>
</div>"#,
            id=s.id, name=escape_html(&s.name), created_at=escape_html(&fmt_admin_dt(&s.created_at)),
            owner_id=s.owner_id, owner_name=escape_html(&s.owner_username), csrf=escape_html(&sess.csrf), filter=escape_html(&filter),
        ));
    }
    if server_cards.is_empty() { server_cards.push_str("<div class='empty-state'>Серверы не найдены.</div>"); }
    let servers_panel = render_servers_panel_body("", &server_cards, true);
    let db_panel = render_db_panel_body(&sess, "/admin/center");
    // GIF panel hidden from center; keep backend endpoints intact
    let download_rows = fetch_admin_downloads(&st.db).await;
    let downloads_panel = render_admin_downloads_panel_body(&sess, &download_rows, "/admin/center?view=downloads");
    let suggestion_status = normalized_suggestion_status(q.status.as_deref());
    let suggestions = fetch_suggestions(&st.db, suggestion_status, 120).await.unwrap_or_default();
    let suggestions_return_to = suggestions_page_url("/admin/center", true, suggestion_status);
    let suggestions_panel = render_suggestions_panel_body(&sess, &suggestions, suggestion_status, true, &suggestions_return_to);

        // Messenger feed removed from admin center (messages are end-to-end encrypted)

    let overview_panel = format!(
        r#"<div class='center-hero'>
  <div>
    <h2 class='center-hero-title'>Центр управления</h2>
    <div class='center-hero-sub'>Одна рабочая область для админки и мессенджера. Секции раскрываются по всей доступной ширине, а поиски и выбранная панель сохраняются.</div>
  </div>
        <div class='center-stat-row'>
        <div class='center-stat'><div class='center-stat-label'>Пользователи</div><div class='center-stat-value'>{users_total}</div></div>
        <div class='center-stat'><div class='center-stat-label'>Серверы</div><div class='center-stat-value'>{servers_total}</div></div>
        <div class='center-stat'><div class='center-stat-label'>Заблокировано</div><div class='center-stat-value'>{banned_total}</div></div>
    </div>
</div>
<div class='helper-grid'>
  <div class='helper-card'><h3>Навигация</h3><div class='center-note-list'>
    <div class='center-note-line'>• слева выбираешь нужную секцию;</div>
    <div class='center-note-line'>• справа открывается рабочая область без перезагрузки;</div>
    <div class='center-note-line'>• состояние панели сохраняется после обновления.</div>
  </div></div>
  <div class='helper-card'><h3>Модерация</h3><div class='center-note-list'>
    <div class='center-note-line'>• репорты видны в карточке пользователя;</div>
    <div class='center-note-line'>• предложения вынесены в отдельную панель;</div>
    <div class='center-note-line'>• опасные действия требуют админ-доступ.</div>
  </div></div>
    <div class='helper-card'><h3>Мониторинг</h3><div class='center-note-list'>
        <div class='center-note-line'>• время в админке показывается в UTC.</div>
    </div></div>
</div>"#,
        users_total=users_total, servers_total=servers_total, banned_total=banned_total,
    );

    let body = format!(
        r#"<div class='panel-shell'>
        <aside class='panel-sidebar'>
        <button type='button' class='center-nav-item panel-switch' data-center-switch='overview'><strong>Центр</strong><span class='small'>Общий вид и точка входа в остальные панели.</span></button>
        <button type='button' class='center-nav-item panel-switch' data-center-switch='users'><strong>Пользователи</strong><span class='small'>Почта, ник и действия по аккаунтам без прыжков по страницам.</span></button>
        <button type='button' class='center-nav-item panel-switch' data-center-switch='suggestions'><strong>Предложения</strong><span class='small'>Идеи пользователей из настроек и быстрый просмотр статусов.</span></button>
        <button type='button' class='center-nav-item panel-switch' data-center-switch='servers'><strong>Серверы</strong><span class='small'>Проверка владельцев и удаление прямо внутри рабочей области.</span></button>
        <button type='button' class='center-nav-item panel-switch' data-center-switch='downloads'><strong>Загрузки</strong><span class='small'>APK и ПК клиент, которые сервер отдает на странице скачивания.</span></button>
        <button type='button' class='center-nav-item panel-switch' data-center-switch='db'><strong>База данных</strong><span class='small'>Сервисные действия и обслуживание без отдельной вкладки.</span></button>
    </aside>
    <section class='panel-stage'>
        <div class='panel-stage-header'>
        <div>
            <div class='panel-stage-title' data-center-stage-title>Центр управления</div>
            <div class='panel-stage-sub' data-center-stage-sub>Одна рабочая страница для админки и внутреннего мониторинга мессенджера.</div>
        </div>
        </div>
        <div class='panel-stage-body'>
        <div class='panel-view' data-panel-view='overview' data-stage-title='Центр управления' data-stage-sub='Одна рабочая страница для админки и внутреннего мониторинга мессенджера.'>{overview_panel}</div>
        <div class='panel-view' data-panel-view='users' data-stage-title='Пользователи' data-stage-sub=''>{users_panel}</div>
        <div class='panel-view' data-panel-view='suggestions' data-stage-title='Предложения' data-stage-sub='Идеи пользователей из настроек и статус их рассмотрения.'>{suggestions_panel}</div>
        <div class='panel-view' data-panel-view='servers' data-stage-title='Панель серверов' data-stage-sub='Проверка серверов и действия с ними в общей рабочей области.'>{servers_panel}</div>
        <div class='panel-view' data-panel-view='downloads' data-stage-title='Загрузки приложения' data-stage-sub='APK и ПК клиент, которые пользователи скачивают с сервера.'>{downloads_panel}</div>
        <div class='panel-view' data-panel-view='db' data-stage-title='Панель базы данных' data-stage-sub='Сервисные инструменты открываются здесь же, без переходов по страницам.'>{db_panel}</div>
                <!-- GIF, Messenger and Homie panels are hidden -->
        </div>
    </section>
    </div>"#,
                overview_panel=overview_panel, users_panel=users_panel, suggestions_panel=suggestions_panel, servers_panel=servers_panel, downloads_panel=downloads_panel, db_panel=db_panel,
    );

    page("Админка • Центр", &body, q.msg.as_deref()).into_response()
}


// =============================
// DB helpers
// =============================

#[derive(Clone)]
pub(crate) struct UserRow {
    pub(crate) id: i64,
    pub(crate) username: String,
    pub(crate) email: String,
    pub(crate) is_banned: bool,
    pub(crate) created_at: String,
    pub(crate) is_online: bool,
    pub(crate) presence_status: String,
    pub(crate) presence_updated_at: String,
    pub(crate) avatar_file_id: Option<i64>,
    pub(crate) ban_reason: String,
    pub(crate) ban_at: String,
    pub(crate) cookie_consent_status: String,
    pub(crate) cookie_consent_at: String,
    pub(crate) trust_factor: i64,
    pub(crate) trust_review_status: String,
    pub(crate) trust_review_reason: String,
    pub(crate) trust_review_at: String,
}

#[derive(Clone)]
pub(crate) struct UserReportRow {
    pub(crate) id: i64,
    pub(crate) reporter_id: i64,
    pub(crate) reporter_username: String,
    pub(crate) target_user_id: i64,
    pub(crate) message_id: Option<i64>,
    pub(crate) reason: String,
    pub(crate) message: String,
    pub(crate) status: String,
    pub(crate) created_at: String,
}

#[derive(Clone)]
struct UserSuggestionRow {
    id: i64,
    user_id: i64,
    username: String,
    title: String,
    message: String,
    status: String,
    created_at: String,
    reviewed_at: Option<String>,
    admin_note: String,
}

fn map_user_row(r: sqlx::postgres::PgRow) -> UserRow {
    UserRow {
        id: r.get("id"),
        username: r.get("username"),
        email: r.get("email"),
        is_banned: r.get::<bool, _>("is_banned"),
        created_at: r.get("created_at"),
        is_online: r.get::<bool, _>("is_online"),
        presence_status: r.get("presence_status"),
        presence_updated_at: r.get("presence_updated_at"),
        avatar_file_id: r.get("avatar_file_id"),
        ban_reason: r.get("ban_reason"),
        ban_at: r.get("ban_at"),
        cookie_consent_status: r.get("cookie_consent_status"),
        cookie_consent_at: r.get("cookie_consent_at"),
        trust_factor: r.get("trust_factor"),
        trust_review_status: r.get("trust_review_status"),
        trust_review_reason: r.get("trust_review_reason"),
        trust_review_at: r.get("trust_review_at"),
    }
}

async fn fetch_users(db: &PgPool, q: &str, limit: i64) -> anyhow::Result<Vec<UserRow>> {
    let select = r#"
        SELECT u.id,
               u.username,
               COALESCE(u.email,'') AS email,
               u.is_banned,
               u.created_at,
               COALESCE(p.is_online, FALSE) AS is_online,
               COALESCE(p.status, 'offline') AS presence_status,
               COALESCE(p.updated_at, '') AS presence_updated_at,
               up.avatar_file_id AS avatar_file_id,
               COALESCE((SELECT me.reason FROM moderation_events me WHERE me.user_id = u.id AND me.kind = 'ban' ORDER BY me.id DESC LIMIT 1), '') AS ban_reason,
               COALESCE((SELECT me.created_at FROM moderation_events me WHERE me.user_id = u.id AND me.kind = 'ban' ORDER BY me.id DESC LIMIT 1), '') AS ban_at,
               COALESCE(u.cookie_consent_status, 'unknown') AS cookie_consent_status,
               COALESCE(u.cookie_consent_at, '') AS cookie_consent_at,
               COALESCE(u.trust_factor, 100) AS trust_factor,
               COALESCE(u.trust_review_status, 'clear') AS trust_review_status,
               COALESCE(u.trust_review_reason, '') AS trust_review_reason,
               COALESCE(u.trust_review_at, '') AS trust_review_at
        FROM users u
        LEFT JOIN user_presence p ON p.user_id = u.id
        LEFT JOIN user_profile up ON up.user_id = u.id
    "#;

    let rows = if q.is_empty() {
        sqlx::query(sqlx::AssertSqlSafe(format!("{select} ORDER BY u.id DESC LIMIT $1")))
            .bind(limit)
            .fetch_all(db)
            .await?
    } else if let Ok(id) = q.parse::<i64>() {
        sqlx::query(sqlx::AssertSqlSafe(format!("{select} WHERE u.id = $1 ORDER BY u.id DESC LIMIT $2")))
            .bind(id)
            .bind(limit)
            .fetch_all(db)
            .await?
    } else {
        let like = format!("%{}%", q);
        sqlx::query(sqlx::AssertSqlSafe(format!("{select} WHERE u.username LIKE $1 OR u.email LIKE $2 ORDER BY u.id DESC LIMIT $3")))
            .bind(&like)
            .bind(&like)
            .bind(limit)
            .fetch_all(db)
            .await?
    };

    Ok(rows.into_iter().map(map_user_row).collect())
}

async fn fetch_user_by_id(db: &PgPool, id: i64) -> anyhow::Result<Option<UserRow>> {
    let select = r#"
        SELECT u.id,
               u.username,
               COALESCE(u.email,'') AS email,
               u.is_banned,
               u.created_at,
               COALESCE(p.is_online, FALSE) AS is_online,
               COALESCE(p.status, 'offline') AS presence_status,
               COALESCE(p.updated_at, '') AS presence_updated_at,
               up.avatar_file_id AS avatar_file_id,
               COALESCE((SELECT me.reason FROM moderation_events me WHERE me.user_id = u.id AND me.kind = 'ban' ORDER BY me.id DESC LIMIT 1), '') AS ban_reason,
               COALESCE((SELECT me.created_at FROM moderation_events me WHERE me.user_id = u.id AND me.kind = 'ban' ORDER BY me.id DESC LIMIT 1), '') AS ban_at,
               COALESCE(u.cookie_consent_status, 'unknown') AS cookie_consent_status,
               COALESCE(u.cookie_consent_at, '') AS cookie_consent_at,
               COALESCE(u.trust_factor, 100) AS trust_factor,
               COALESCE(u.trust_review_status, 'clear') AS trust_review_status,
               COALESCE(u.trust_review_reason, '') AS trust_review_reason,
               COALESCE(u.trust_review_at, '') AS trust_review_at
        FROM users u
        LEFT JOIN user_presence p ON p.user_id = u.id
        LEFT JOIN user_profile up ON up.user_id = u.id
        WHERE u.id = $1
        LIMIT 1
    "#;
    let row = sqlx::query(select).bind(id).fetch_optional(db).await?;
    Ok(row.map(map_user_row))
}

async fn fetch_user_reports(db: &PgPool, user_id: i64, limit: i64) -> anyhow::Result<Vec<UserReportRow>> {
    let rows = sqlx::query(
        r#"
        SELECT r.id,
               r.reporter_id,
               COALESCE(u.username, 'deleted') AS reporter_username,
               r.target_user_id,
               r.message_id,
               r.reason,
               COALESCE(r.message, '') AS message,
               r.status,
               r.created_at
        FROM user_reports r
        LEFT JOIN users u ON u.id = r.reporter_id
        WHERE r.target_user_id = $1
        ORDER BY CASE r.status WHEN 'open' THEN 0 ELSE 1 END, r.id DESC
        LIMIT $2
        "#,
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| UserReportRow {
            id: r.get("id"),
            reporter_id: r.get("reporter_id"),
            reporter_username: r.get("reporter_username"),
            target_user_id: r.get("target_user_id"),
            message_id: r.get("message_id"),
            reason: r.get("reason"),
            message: r.get("message"),
            status: r.get("status"),
            created_at: r.get("created_at"),
        })
        .collect())
}

async fn fetch_suggestions(db: &PgPool, status: &str, limit: i64) -> anyhow::Result<Vec<UserSuggestionRow>> {
    let status = normalized_suggestion_status(Some(status));
    let base = r#"
        SELECT s.id,
               s.user_id,
               COALESCE(u.username, 'deleted') AS username,
               COALESCE(s.title, '') AS title,
               s.message,
               s.status,
               s.created_at,
               s.reviewed_at,
               COALESCE(s.admin_note, '') AS admin_note
        FROM user_suggestions s
        LEFT JOIN users u ON u.id = s.user_id
    "#;

    let rows = if status == "all" {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "{base} ORDER BY CASE s.status WHEN 'open' THEN 0 ELSE 1 END, s.id DESC LIMIT $1"
        )))
        .bind(limit)
        .fetch_all(db)
        .await?
    } else {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "{base} WHERE s.status = $1 ORDER BY s.id DESC LIMIT $2"
        )))
        .bind(status)
        .bind(limit)
        .fetch_all(db)
        .await?
    };

    Ok(rows
        .into_iter()
        .map(|r| UserSuggestionRow {
            id: r.get("id"),
            user_id: r.get("user_id"),
            username: r.get("username"),
            title: r.get("title"),
            message: r.get("message"),
            status: r.get("status"),
            created_at: r.get("created_at"),
            reviewed_at: r.try_get("reviewed_at").ok(),
            admin_note: r.get("admin_note"),
        })
        .collect())
}

async fn fetch_test_users(db: &PgPool, re: &Regex, limit: i64) -> anyhow::Result<Vec<UserRow>> {
    let rows = sqlx::query(
        r#"SELECT id, username, COALESCE(email,'') AS email, is_banned, created_at
           FROM users
           ORDER BY id DESC
           LIMIT $1"#,
    )
    .bind(limit)
    .fetch_all(db)
    .await?;

    let mut out = Vec::new();
    for r in rows {
        let username: String = r.get("username");
        let email: String = r.get("email");
        if is_test_user(re, &username, &email) {
            out.push(UserRow {
                id: r.get("id"),
                username,
                email,
                is_banned: r.get::<bool, _>("is_banned"),
                created_at: r.get("created_at"),
                is_online: false,
                presence_status: "offline".to_string(),
                presence_updated_at: String::new(),
                avatar_file_id: None,
                ban_reason: String::new(),
                ban_at: String::new(),
                cookie_consent_status: "unknown".to_string(),
                cookie_consent_at: String::new(),
                trust_factor: 100,
                trust_review_status: "clear".to_string(),
                trust_review_reason: String::new(),
                trust_review_at: String::new(),
            });
        }
    }
    Ok(out)
}

// moved to servers.rs

// moved to content.rs

async fn purge_server_exec(db: &PgPool, server_id: i64) -> anyhow::Result<()> {
    use std::path::PathBuf;

    let mut tx = db.begin().await?;

    // Collect file paths first (so we can delete from FS after commit)
    let file_rows = sqlx::query("SELECT storage_path, filename FROM files WHERE server_id = $1")
        .bind(server_id)
        .fetch_all(&mut *tx)
        .await?;

    let mut file_paths: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    for fr in file_rows {
        let p: String = fr.get("storage_path");
        let stored_filename: String = fr.get("filename");
        let main = PathBuf::from(p);
        let stem = std::path::Path::new(&stored_filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&stored_filename);
        let thumb = PathBuf::from("storage/files/thumbs").join(format!("{}.png", stem));
        file_paths.push((main, Some(thumb)));
    }

    let profile_rows =
        sqlx::query("SELECT storage_path FROM profile_files WHERE server_id = $1")
            .bind(server_id)
            .fetch_all(&mut *tx)
            .await?;
    let mut profile_paths: Vec<PathBuf> = Vec::new();
    for pr in profile_rows {
        let p: String = pr.get("storage_path");
        profile_paths.push(PathBuf::from(p));
    }

    let _ = sqlx::query("DELETE FROM message_reactions WHERE message_id IN (SELECT id FROM messages WHERE server_id = $1)")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;
    let _ = sqlx::query("DELETE FROM pinned_messages WHERE server_id = $1")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;
    let _ = sqlx::query("DELETE FROM pinned_messages WHERE message_id IN (SELECT id FROM messages WHERE server_id = $1)")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;
    let _ = sqlx::query("DELETE FROM server_members WHERE server_id = $1")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;
    let _ = sqlx::query("DELETE FROM server_categories WHERE server_id = $1")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;
    let _ = sqlx::query("DELETE FROM server_roles WHERE server_id = $1")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;
    let _ = sqlx::query("DELETE FROM server_bans WHERE server_id = $1")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;
    let _ = sqlx::query("DELETE FROM server_invites WHERE server_id = $1")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;
    let _ = sqlx::query("DELETE FROM server_backups WHERE server_id = $1")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;
    let _ = sqlx::query("DELETE FROM server_webhooks WHERE server_id = $1")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM gif_assets WHERE source_file_id IN (SELECT id FROM files WHERE server_id = $1)")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM files WHERE server_id = $1")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM profile_files WHERE server_id = $1")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM messages WHERE server_id = $1")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM servers WHERE id = $1")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    // Best-effort filesystem cleanup
    let _ = file_paths;
    let _ = cleanup_file_storage_orphans_db(db).await;
    for p in profile_paths {
        let _ = std::fs::remove_file(&p);
    }

    Ok(())
}

// moved to users.rs

// moved to content.rs
