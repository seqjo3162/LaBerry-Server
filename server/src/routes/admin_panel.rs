use crate::{auth, server::{AdminSession, AppState}};

use anyhow::Context;
use axum::{
    extract::{Form, Multipart, Path, Query, State},
    Json,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect},
    routing::{get, post},
    Router,
};
use chrono::{Duration as ChronoDuration, TimeZone, Utc};
use password_hash::{PasswordHash, PasswordVerifier};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use std::{env, net::IpAddr, path::PathBuf};
use tokio::{fs, fs::File};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

// =============================
// Router
// =============================

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(root))
        .route("/login", get(login_get).post(login_post))
        .route("/logout", post(logout_post))
        .route("/users", get(users_list))
        .route("/users/:id/card", get(admin_user_card_fragment))
        .route("/users/:id/details", get(admin_user_details_fragment))
        .route("/users/:id/ban", post(user_ban))
        .route("/users/:id/unban", post(user_unban))
        .route("/users/:id/ban_forever", post(user_ban_forever))
        .route("/users/:id/purge", post(user_purge_content))
        .route("/reports/:id/status", post(admin_report_status))
        .route("/suggestions", get(suggestions_page))
        .route("/suggestions/:id/status", post(admin_suggestion_status))
        .route("/gifs", get(gifs_page))
        .route("/gifs/upload", post(admin_gif_upload))
        .route("/gifs/:id/delete", post(admin_gif_delete))
        .route("/gifs/:id/raw", get(admin_gif_raw))
        .route("/downloads", get(downloads_page))
        .route("/downloads/upload", post(admin_download_upload))
        .route("/downloads/:id/delete", post(admin_download_delete))
        .route("/test-users", get(test_users_page).post(test_users_delete))
        .route("/servers", get(servers_list))
        .route("/servers/:id/delete", post(server_delete))
        .route("/servers/:id/add_all_users", post(server_add_all_users))
        .route("/center", get(center_page))
        .route("/files/:id/raw", get(admin_file_raw))
        .route("/profile-files/:id/raw", get(admin_profile_file_raw))
        .route("/db", get(db_tools_page))
        .route("/db/wipe_messages", post(db_wipe_messages_post))
        .route("/db/wipe_servers", post(db_wipe_servers_post))
        .route("/db/reset_keep_users", post(db_reset_keep_users_post))
        .route("/db/vacuum", post(db_vacuum_post))
        .route("/db/cleanup_expired_files", post(db_cleanup_expired_files_post))
        .route("/homie/health", get(homie_health_get))
        .route("/homie/tools", get(homie_tools_get))
        .route("/homie/chat", post(homie_chat_post))
        .route("/homie/reset", post(homie_reset_post))
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

fn test_server_re() -> Regex {
    Regex::new(&env::var("LB_TEST_SERVER_REGEX").unwrap_or_else(|_| "^test_".to_string()))
        .unwrap_or_else(|_| Regex::new("^test_").unwrap())
}

fn admin_password_configured() -> bool {
    env::var("LB_ADMIN_PASSWORD_HASH").ok().is_some() || env::var("LB_ADMIN_PASSWORD").ok().is_some()
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
    if let Ok(plain) = env::var("LB_ADMIN_PASSWORD") {
        if constant_time_eq(pw, &plain) {
            return Ok(());
        }
        anyhow::bail!("Неверный пароль администратора");
    }

    if let Ok(hash) = env::var("LB_ADMIN_PASSWORD_HASH") {
        let parsed = PasswordHash::new(&hash).context("Некорректный LB_ADMIN_PASSWORD_HASH")?;
        let argon2 = argon2::Argon2::default();
        argon2
            .verify_password(pw.as_bytes(), &parsed)
            .context("Неверный пароль администратора")?;
        return Ok(());
    }

    anyhow::bail!(
        "Пароль администратора не настроен (укажи LB_ADMIN_PASSWORD_HASH или LB_ADMIN_PASSWORD)"
    );
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn escape_html_lines(s: &str) -> String {
    escape_html(s).replace("\r\n", "\n").replace('\r', "\n").replace('\n', "<br>")
}

fn admin_sanitize_filename(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        let ok = ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-';
        if ok { out.push(ch); } else if ch.is_whitespace() { out.push('_'); } else { out.push('_'); }
    }
    if out.is_empty() { "file".to_string() } else { out }
}

fn admin_format_bytes(size: i64) -> String {
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
    // allow multiple Set-Cookie
    headers.append(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).unwrap_or_else(|_| HeaderValue::from_static("")),
    );
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

fn admin_redirect_with_msg(path: &str, msg: &str) -> impl IntoResponse {
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

fn is_test_server(re: &Regex, name: &str) -> bool {
    re.is_match(name)
}

fn now_ts() -> i64 {
    Utc::now().timestamp()
}

fn fmt_admin_dt(raw: &str) -> String {
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

fn page(title: &str, body: &str, msg: Option<&str>) -> Html<String> {
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
        "<script src='/static/js/admin-center.js?v=6' defer></script>"
    } else {
        ""
    };
    let users_script = if title.contains("Центр") || title.contains("Пользователи") {
        "<script src='/static/js/admin-users.js?v=2' defer></script>"
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
  <form method='post' action='/admin/logout'>
    <button type='submit'>Выйти</button>
  </form>
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
        users_script = users_script
    );

    Html(html)
}

fn embedded_page(title: &str, body: &str, msg: Option<&str>) -> Html<String> {
    let mut html = page(title, body, msg).0;
    html = html.replacen("<header>", "<header style='display:none'>", 1);
    html = html.replacen("<main class='admin-main'>", "<main class='admin-main' style='max-width:none;padding:12px;'>", 1);
    Html(html)
}

async fn admin_file_raw(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(file_id): Path<i64>,
) -> impl IntoResponse {
    if let Err((code, msg)) = require_admin_panel_enabled() { return (code, msg).into_response(); }
    if let Err((code, msg)) = require_allow_ip(&headers) { return (code, msg).into_response(); }
    if let Err(redir) = require_auth(&st, &headers) { return redir.into_response(); }

    let row = sqlx::query(
        r#"
        SELECT original_name, mime_type, storage_path, file_size
        FROM files
        WHERE id = ?
        LIMIT 1
        "#,
    )
    .bind(file_id)
    .fetch_optional(&st.db)
    .await;

    let row = match row {
        Ok(Some(v)) => v,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("db_error: {e}")).into_response(),
    };

    let original_name: String = row.get("original_name");
    let mime_type: String = row.get("mime_type");
    let storage_path: String = row.get("storage_path");
    let file_size: i64 = row.get("file_size");

    let path = PathBuf::from(storage_path);
    let meta = match fs::metadata(&path).await {
        Ok(m) if m.is_file() && m.len() > 0 => m,
        _ => return (StatusCode::NOT_FOUND, "Файл отсутствует на диске").into_response(),
    };

    let file = match File::open(&path).await {
        Ok(f) => f,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("open_failed: {e}")).into_response(),
    };

    let safe_name = admin_sanitize_filename(&original_name);
    let ct = HeaderValue::from_str(mime_type.split(';').next().unwrap_or("application/octet-stream").trim())
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    let cd = HeaderValue::from_str(&format!("attachment; filename=\"{}\"", safe_name))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment"));
    let len_value = std::cmp::max(file_size, meta.len() as i64).to_string();
    let len = HeaderValue::from_str(&len_value).unwrap_or_else(|_| HeaderValue::from_static("0"));
    let body = axum::body::Body::from_stream(ReaderStream::new(file));

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, ct),
            (header::CONTENT_DISPOSITION, cd),
            (header::CONTENT_LENGTH, len),
            (header::HeaderName::from_static("x-content-type-options"), HeaderValue::from_static("nosniff")),
        ],
        body,
    ).into_response()
}


async fn admin_profile_file_raw(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(file_id): Path<i64>,
) -> impl IntoResponse {
    if let Err((code, msg)) = require_admin_panel_enabled() { return (code, msg).into_response(); }
    if let Err((code, msg)) = require_allow_ip(&headers) { return (code, msg).into_response(); }
    if let Err(redir) = require_auth(&st, &headers) { return redir.into_response(); }

    let row = sqlx::query(
        r#"
        SELECT original_name, mime_type, storage_path, file_size
        FROM profile_files
        WHERE id = ?
        LIMIT 1
        "#,
    )
    .bind(file_id)
    .fetch_optional(&st.db)
    .await;

    let row = match row {
        Ok(Some(v)) => v,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("db_error: {e}")).into_response(),
    };

    let original_name: String = row.get("original_name");
    let mime_type: String = row.get("mime_type");
    let storage_path: String = row.get("storage_path");
    let file_size: i64 = row.get("file_size");

    let path = PathBuf::from(storage_path);
    let meta = match fs::metadata(&path).await {
        Ok(m) if m.is_file() && m.len() > 0 => m,
        _ => return (StatusCode::NOT_FOUND, "Файл отсутствует на диске").into_response(),
    };

    let file = match File::open(&path).await {
        Ok(f) => f,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("open_failed: {e}")).into_response(),
    };

    let safe_name = admin_sanitize_filename(&original_name);
    let ct = HeaderValue::from_str(mime_type.split(';').next().unwrap_or("application/octet-stream").trim())
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    let cd = HeaderValue::from_str(&format!("inline; filename=\"{}\"", safe_name))
        .unwrap_or_else(|_| HeaderValue::from_static("inline"));
    let len_value = std::cmp::max(file_size, meta.len() as i64).to_string();
    let len = HeaderValue::from_str(&len_value).unwrap_or_else(|_| HeaderValue::from_static("0"));
    let body = axum::body::Body::from_stream(ReaderStream::new(file));

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, ct),
            (header::CONTENT_DISPOSITION, cd),
            (header::CONTENT_LENGTH, len),
            (header::HeaderName::from_static("x-content-type-options"), HeaderValue::from_static("nosniff")),
        ],
        body,
    ).into_response()
}

async fn admin_gif_raw(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(asset_id): Path<i64>,
) -> impl IntoResponse {
    if let Err((code, msg)) = require_admin_panel_enabled() { return (code, msg).into_response(); }
    if let Err((code, msg)) = require_allow_ip(&headers) { return (code, msg).into_response(); }
    if let Err(redir) = require_auth(&st, &headers) { return redir.into_response(); }

    let row = sqlx::query(
        r#"
        SELECT original_name, storage_path, file_size
        FROM gif_assets
        WHERE id = ? AND scope = 'global'
        LIMIT 1
        "#,
    )
    .bind(asset_id)
    .fetch_optional(&st.db)
    .await;

    let row = match row {
        Ok(Some(v)) => v,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("db_error: {e}")).into_response(),
    };

    let original_name: String = row.get("original_name");
    let storage_path: String = row.get("storage_path");
    let file_size: i64 = row.get("file_size");
    let path = PathBuf::from(storage_path);
    let meta = match fs::metadata(&path).await {
        Ok(m) if m.is_file() && m.len() > 0 => m,
        _ => return (StatusCode::NOT_FOUND, "GIF отсутствует на диске").into_response(),
    };
    let file = match File::open(&path).await {
        Ok(f) => f,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("open_failed: {e}")).into_response(),
    };

    let safe_name = admin_sanitize_filename(&original_name);
    let cd = HeaderValue::from_str(&format!("inline; filename=\"{}\"", safe_name))
        .unwrap_or_else(|_| HeaderValue::from_static("inline"));
    let len_value = std::cmp::max(file_size, meta.len() as i64).to_string();
    let len = HeaderValue::from_str(&len_value).unwrap_or_else(|_| HeaderValue::from_static("0"));
    let body = axum::body::Body::from_stream(ReaderStream::new(file));

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("image/gif")),
            (header::CONTENT_DISPOSITION, cd),
            (header::CONTENT_LENGTH, len),
            (header::HeaderName::from_static("x-content-type-options"), HeaderValue::from_static("nosniff")),
        ],
        body,
    ).into_response()
}

// =============================
// Auth/session
// =============================

fn require_admin_panel_enabled() -> Result<(), (StatusCode, String)> {
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

fn require_auth(st: &AppState, headers: &HeaderMap) -> Result<(String, AdminSession), Redirect> {
    match session_get(st, headers) {
        Some(v) => Ok(v),
        None => Err(Redirect::to("/admin/login")),
    }
}

fn require_allow_ip(headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    // Optional simple allow-list by exact IPs.
    // NOTE: If you run behind proxy, ensure X-Forwarded-For is trusted.
    let allow = env::var("LB_ADMIN_ALLOW_IPS").unwrap_or_default();
    let allow = allow.trim();
    if allow.is_empty() {
        return Ok(());
    }

    let remote = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .or_else(|| headers.get("x-real-ip").and_then(|v| v.to_str().ok()).map(|s| s.trim().to_string()));

    let Some(remote) = remote else {
        return Err((StatusCode::FORBIDDEN, "Включён список разрешённых IP, но заголовок с IP не передан".to_string()));
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
    use rand::RngCore;

    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    let sid = URL_SAFE_NO_PAD.encode(buf);

    let mut csrf_buf = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut csrf_buf);
    let csrf = URL_SAFE_NO_PAD.encode(csrf_buf);

    let expires_at = (Utc::now() + ChronoDuration::hours(8)).timestamp();
    let sess = AdminSession { expires_at, csrf };

    st.admin_sessions.insert(sid.clone(), sess.clone());
    (sid, sess)
}

fn cookie_for_session(sid: &str, secure: bool) -> String {
    let mut c = format!("lb_admin_sid={}; Path=/admin; HttpOnly; SameSite=Strict", sid);
    if secure {
        c.push_str("; Secure");
    }
    c
}

fn cookie_clear(secure: bool) -> String {
    let mut c = "lb_admin_sid=deleted; Path=/admin; Max-Age=0; HttpOnly; SameSite=Strict".to_string();
    if secure {
        c.push_str("; Secure");
    }
    c
}

// =============================
// Pages
// =============================

async fn root(State(st): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }
    if session_get(&st, &headers).is_none() {
        return Redirect::to("/admin/login").into_response();
    }
    Redirect::to("/admin/center").into_response()
}

#[derive(Deserialize, Default)]
struct MsgQuery {
    msg: Option<String>,
    embed: Option<u8>,
    view: Option<String>,
    q: Option<String>,
    mode: Option<String>,
    status: Option<String>,
    user_id: Option<i64>,
}

async fn login_get(State(st): State<AppState>, headers: HeaderMap, Query(q): Query<MsgQuery>) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
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
<form method='post' action='/admin/login'>
  <div class='small'>Пароль администратора</div>
  <input type='password' name='password' autocomplete='current-password' required />
  <div style='height:10px'></div>
  <button type='submit'>Войти</button>
</form>
{warn}
</div>"#,
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
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }

    if let Err(err) = verify_admin_password(&form.password) {
        return admin_redirect_with_msg("/admin/login", &format!("{}", err)).into_response();
    }

    let (sid, _sess) = new_session(&st);
    let mut h = HeaderMap::new();
    set_cookie(&mut h, cookie_for_session(&sid, admin_cookie_secure(&headers)));
    (h, Redirect::to("/admin/center")).into_response()
}

async fn logout_post(State(st): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Some((sid, _)) = session_get(&st, &headers) {
        st.admin_sessions.remove(&sid);
    }
    let mut h = HeaderMap::new();
    set_cookie(&mut h, cookie_clear(admin_cookie_secure(&headers)));
    (h, Redirect::to("/admin/login")).into_response()
}

// =============================
// Пользователи list + actions
// =============================

#[derive(Deserialize, Default)]
struct ListQuery {
    q: Option<String>,
    msg: Option<String>,
    embed: Option<u8>,
    return_to: Option<String>,
    mode: Option<String>,
    user_id: Option<i64>,
}


fn safe_admin_return_to(input: &str, fallback: &str) -> String {
    let s = input.trim();
    if s.starts_with("/admin/") && !s.contains("\n") && !s.contains("\r") {
        s.to_string()
    } else {
        fallback.to_string()
    }
}

fn normalized_user_mode(input: Option<&str>) -> &'static str {
    match input.unwrap_or("all").trim().to_ascii_lowercase().as_str() {
        "active" => "active",
        "banned" => "banned",
        _ => "all",
    }
}

fn user_mode_matches(user: &UserRow, mode: &str) -> bool {
    match mode {
        "active" => !user.is_banned,
        "banned" => user.is_banned,
        _ => true,
    }
}

fn user_page_url(base_path: &str, embedded: bool, q: &str, mode: &str, user_id: Option<i64>) -> String {
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    if embedded {
        ser.append_pair("view", "users");
    }
    let q = q.trim();
    if !q.is_empty() {
        ser.append_pair("q", q);
    }
    if mode != "all" {
        ser.append_pair("mode", mode);
    }
    if let Some(user_id) = user_id {
        ser.append_pair("user_id", &user_id.to_string());
    }
    let query = ser.finish();
    if query.is_empty() { base_path.to_string() } else { format!("{base_path}?{query}") }
}

fn reason_label(reason: &str) -> &'static str {
    match reason {
        "spam" => "Спам",
        "abuse" => "Оскорбления",
        "avatar" => "Аватар",
        "username" => "Ник",
        "ads" => "Реклама",
        "scam" => "Скам",
        _ => "Другое",
    }
}

fn report_status_label(status: &str) -> &'static str {
    match status {
        "reviewed" => "Просмотрено",
        "rejected" => "Отклонено",
        _ => "Новая",
    }
}

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

fn render_user_reports_html(sess: &AdminSession, reports: &[UserReportRow], return_to: &str) -> String {
    if reports.is_empty() {
        return "<div class='admin-user-emptyline'>Репортов по этому пользователю нет.</div>".to_string();
    }

    let mut out = String::new();
    for r in reports {
        let message = if r.message.trim().is_empty() {
            "Без комментария".to_string()
        } else {
            escape_html(&r.message)
        };
        let msg_id = r.message_id.map(|id| format!(" · сообщение #{id}")).unwrap_or_default();
        let actions = if r.status == "open" {
            format!(
                r#"<div class='admin-report-actions'>
  <form method='post' action='/admin/reports/{id}/status' data-ajax-report-status>
    <input type='hidden' name='csrf' value='{csrf}' />
    <input type='hidden' name='return_to' value='{return_to}' />
    <input type='hidden' name='status' value='reviewed' />
    <button type='submit' class='btn-soft'>Просмотрено</button>
  </form>
  <form method='post' action='/admin/reports/{id}/status' data-ajax-report-status>
    <input type='hidden' name='csrf' value='{csrf}' />
    <input type='hidden' name='return_to' value='{return_to}' />
    <input type='hidden' name='status' value='rejected' />
    <button type='submit' class='btn-soft'>Отклонить</button>
  </form>
</div>"#,
                id = r.id,
                csrf = escape_html(&sess.csrf),
                return_to = escape_html(return_to),
            )
        } else {
            String::new()
        };
        out.push_str(&format!(
            r#"<div class='admin-report-row'>
  <div class='admin-report-top'>
    <span class='admin-report-reason'>{reason}</span>
    <span class='admin-report-status {status_class}'>{status}</span>
  </div>
  <div class='admin-report-text'>{message}</div>
  <div class='admin-report-meta'>От: #{reporter_id} {reporter_name} · {created_at}{msg_id}</div>
  {actions}
</div>"#,
            reason = reason_label(&r.reason),
            status = report_status_label(&r.status),
            status_class = if r.status == "open" { "open" } else { "done" },
            message = message,
            reporter_id = r.reporter_id,
            reporter_name = escape_html(&r.reporter_username),
            created_at = escape_html(&fmt_admin_dt(&r.created_at)),
            msg_id = escape_html(&msg_id),
            actions = actions,
        ));
    }
    out
}

fn render_user_detail_card(
    sess: &AdminSession,
    user: &UserRow,
    reports: &[UserReportRow],
    current_return_to: &str,
) -> String {
    let email_html = if user.email.trim().is_empty() { "без e-mail".to_string() } else { escape_html(&user.email) };
    let initial = user.username.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_else(|| "?".to_string());
    let online_label = if user.is_online { "Онлайн" } else { "Оффлайн" };
    let online_class = if user.is_online { "online" } else { "offline" };
    let access_label = if user.is_banned { "Ограничен" } else { "Без ограничений" };
    let access_class = if user.is_banned { "banned" } else { "clear" };
    let last_seen = if user.presence_updated_at.trim().is_empty() { "нет данных".to_string() } else { fmt_admin_dt(&user.presence_updated_at) };
    let avatar_html = if let Some(file_id) = user.avatar_file_id {
        format!("<img class='admin-user-avatar-img' src='/admin/profile-files/{file_id}/raw' alt='avatar' />")
    } else {
        escape_html(&initial)
    };
    let ban_reason_html = if user.is_banned && !user.ban_reason.trim().is_empty() {
        format!(
            "<div class='admin-user-section compact-ban-reason'><strong>Причина бана</strong><div class='admin-user-section-muted'>{}</div><div class='admin-report-meta'>{}</div></div>",
            escape_html(&user.ban_reason),
            if user.ban_at.trim().is_empty() { "".to_string() } else { format!("Выдан: {}", escape_html(&fmt_admin_dt(&user.ban_at))) }
        )
    } else {
        String::new()
    };
    let main_action = if user.is_banned {
        format!(
            r#"<form method='post' action='/admin/users/{id}/unban' data-ajax-user-action>
  <input type='hidden' name='csrf' value='{csrf}' />
  <input type='hidden' name='return_to' value='{return_to}' />
  <button type='submit' class='btn-ok'>Разбанить</button>
</form>"#,
            id = user.id,
            csrf = escape_html(&sess.csrf),
            return_to = escape_html(current_return_to),
        )
    } else {
        format!(
            r#"<form method='post' action='/admin/users/{id}/ban' data-ajax-user-action>
  <input type='hidden' name='csrf' value='{csrf}' />
  <input type='hidden' name='return_to' value='{return_to}' />
  <input type='text' name='reason' class='admin-ban-reason-input' placeholder='Причина бана' maxlength='180' />
  <button type='submit' class='btn-soft'>Заблокировать</button>
</form>"#,
            id = user.id,
            csrf = escape_html(&sess.csrf),
            return_to = escape_html(current_return_to),
        )
    };

    format!(
        r#"<div class='admin-user-card' data-admin-user-detail-card data-user-id='{id}' data-user-banned='{banned}' data-user-online='{online_data}'>
  <div class='admin-user-card-head'>
    <div class='admin-user-card-ident'>
      <button type='button' class='admin-user-avatar' data-admin-user-details='{id}' data-details-url='/admin/users/{id}/details'>{avatar_html}</button>
      <div class='admin-user-card-title'>
        <div class='admin-user-name'>{username}</div>
        <div class='admin-user-sub'>ID #{id} · {email_html}</div>
        <div class='admin-user-pills'>
          <span class='admin-user-pill {online_class}'>{online_label}</span>
          <span class='admin-user-pill {access_class}'>{access_label}</span>
        </div>
      </div>
    </div>
    <button type='button' class='admin-user-gear' data-admin-user-details='{id}' data-details-url='/admin/users/{id}/details' title='Детали и аватар'>⚙</button>
  </div>

  <div class='admin-user-info-grid compact'>
    <div class='admin-user-info'><span>Регистрация</span><strong>{created_at}</strong></div>
    <div class='admin-user-info'><span>Последняя активность</span><strong>{last_seen}</strong></div>
  </div>

  {ban_reason_html}

  <div class='admin-user-section'>
    <div class='admin-user-section-head'>
      <strong>Репорты</strong>
      <span>{report_count}</span>
    </div>
    <div class='admin-report-list'>{reports_html}</div>
  </div>

  <div class='admin-user-actions'>
    {main_action}
    <form method='post' action='/admin/users/{id}/purge' data-ajax-user-action>
      <input type='hidden' name='csrf' value='{csrf}' />
      <input type='hidden' name='return_to' value='{return_to}' />
      <button type='submit' class='btn-soft'>Удалить контент</button>
    </form>
    <form method='post' action='/admin/users/{id}/ban_forever' data-ajax-user-action data-danger-action='1'>
      <input type='hidden' name='csrf' value='{csrf}' />
      <input type='hidden' name='return_to' value='{return_to}' />
      <button type='submit' class='btn-danger'>Удалить аккаунт</button>
    </form>
  </div>
</div>"#,
        id = user.id,
        banned = if user.is_banned { "1" } else { "0" },
        online_data = if user.is_online { "1" } else { "0" },
        avatar_html = avatar_html,
        username = escape_html(&user.username),
        email_html = email_html,
        online_class = online_class,
        online_label = online_label,
        access_class = access_class,
        access_label = access_label,
        created_at = escape_html(&fmt_admin_dt(&user.created_at)),
        last_seen = escape_html(&last_seen),
        ban_reason_html = ban_reason_html,
        report_count = reports.len(),
        reports_html = render_user_reports_html(sess, reports, current_return_to),
        main_action = main_action,
        csrf = escape_html(&sess.csrf),
        return_to = escape_html(current_return_to),
    )
}

fn render_user_details_modal(user: &UserRow) -> String {
    let email_html = if user.email.trim().is_empty() { "без e-mail".to_string() } else { escape_html(&user.email) };
    let initial = user.username.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_else(|| "?".to_string());
    let avatar_big = if let Some(file_id) = user.avatar_file_id {
        format!(r#"<img class='admin-modal-avatar-img' src='/admin/profile-files/{file_id}/raw' alt='avatar' />"#)
    } else {
        format!("<div class='admin-modal-avatar-empty'>{}</div>", escape_html(&initial))
    };
    let avatar_actions = if let Some(file_id) = user.avatar_file_id {
        format!(
            r#"<div class='admin-modal-actions'>
  <a href='/admin/profile-files/{file_id}/raw' target='_blank' rel='noopener'>Открыть аватар</a>
  <a href='/admin/profile-files/{file_id}/raw' download>Скачать</a>
</div>"#,
        )
    } else {
        "<div class='admin-user-emptyline'>Аватар не установлен.</div>".to_string()
    };
    format!(
        r#"<div class='admin-modal-head'>
  <div>
    <div class='admin-modal-title'>{username}</div>
    <div class='admin-modal-sub'>ID #{id} · {email_html}</div>
  </div>
  <button type='button' class='admin-modal-close' data-admin-modal-close>✕</button>
</div>
<div class='admin-modal-body'>
  <div class='admin-modal-avatar'>{avatar_big}</div>
  {avatar_actions}
  <div class='admin-user-info-grid'>
    <div class='admin-user-info'><span>Регистрация</span><strong>{created_at}</strong></div>
    <div class='admin-user-info'><span>Последняя активность</span><strong>{last_seen}</strong></div>
    <div class='admin-user-info'><span>Статус</span><strong>{status}</strong></div>
    <div class='admin-user-info'><span>Бан</span><strong>{ban}</strong></div>
  </div>
</div>"#,
        username = escape_html(&user.username),
        id = user.id,
        email_html = email_html,
        avatar_big = avatar_big,
        avatar_actions = avatar_actions,
        created_at = escape_html(&fmt_admin_dt(&user.created_at)),
        last_seen = escape_html(&if user.presence_updated_at.trim().is_empty() { "нет данных".to_string() } else { fmt_admin_dt(&user.presence_updated_at) }),
        status = if user.is_online { "онлайн" } else { "оффлайн" },
        ban = if user.is_banned { "ограничен" } else { "нет" },
    )
}

fn render_users_panel_body(
    sess: &AdminSession,
    users: &[UserRow],
    query: &str,
    embedded: bool,
    mode: &str,
    requested_user_id: Option<i64>,
    current_return_to: &str,
    selected_reports: &[UserReportRow],
) -> String {
    let base_path = if embedded { "/admin/center" } else { "/admin/users" };
    let mode = normalized_user_mode(Some(mode));
    let filtered: Vec<&UserRow> = users.iter().filter(|u| user_mode_matches(u, mode)).collect();
    let selected = requested_user_id
        .and_then(|id| filtered.iter().copied().find(|u| u.id == id))
        .or_else(|| filtered.first().copied());
    let selected_id = selected.map(|u| u.id);

    let all_href = user_page_url(base_path, embedded, query, "all", None);
    let active_href = user_page_url(base_path, embedded, query, "active", None);
    let banned_href = user_page_url(base_path, embedded, query, "banned", None);
    let search_html = if embedded {
        format!(
            r#"<div class='admin-user-search'>
  <input type='text' data-persist-key='admin-center-users-search' data-filter-input='users' value='{qval}' placeholder='Поиск: имя / e-mail / id' />
  <button type='button' class='btn-soft' data-clear-filter='users'>Сбросить</button>
</div>"#,
            qval = escape_html(query),
        )
    } else {
        format!(
            r#"<form method='get' action='{action}' class='admin-user-search'>
  <input type='hidden' name='mode' value='{mode}' />
  <input type='text' name='q' value='{qval}' placeholder='Поиск: имя / e-mail / id' />
  <button type='submit'>Найти</button>
</form>"#,
            action = base_path,
            mode = escape_html(mode),
            qval = escape_html(query),
        )
    };

    let mut rows_html = String::new();
    if filtered.is_empty() {
        rows_html.push_str("<div class='admin-user-emptyline'>Пользователи не найдены.</div>");
    } else {
        for user in &filtered {
            let initial = user.username.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_else(|| "?".to_string());
            let row_href = user_page_url(base_path, embedded, query, mode, Some(user.id));
            let card_url = format!("/admin/users/{}/card?return_to={}", user.id, url::form_urlencoded::byte_serialize(current_return_to.as_bytes()).collect::<String>());
            let details_url = format!("/admin/users/{}/details", user.id);
            let active = if selected_id == Some(user.id) { " active" } else { "" };
            let pill_class = if user.is_banned { "banned" } else if user.is_online { "online" } else { "offline" };
            let pill_text = if user.is_banned { "Бан" } else if user.is_online { "Онлайн" } else { "Оффлайн" };
            let avatar_html = if let Some(file_id) = user.avatar_file_id {
                format!("<img class='admin-user-row-avatar-img' src='/admin/profile-files/{file_id}/raw' alt='avatar' />")
            } else {
                escape_html(&initial)
            };
            let filter = format!("#{} {} {}", user.id, user.username.to_lowercase(), user.email.to_lowercase());
            rows_html.push_str(&format!(
                r#"<a class='admin-user-row{active}' href='{href}' data-admin-user-row data-user-id='{id}' data-card-url='{card_url}' data-details-url='{details_url}' data-filter-item='users' data-filter='{filter}'>
  <div class='admin-user-row-avatar'>{avatar_html}</div>
  <div class='admin-user-row-main'>
    <div class='admin-user-row-name'>{username}</div>
    <div class='admin-user-row-meta'>ID #{id}</div>
  </div>
  <span class='admin-user-pill {pill_class}'>{pill_text}</span>
</a>"#,
                active = active,
                href = escape_html(&row_href),
                id = user.id,
                card_url = escape_html(&card_url),
                details_url = escape_html(&details_url),
                filter = escape_html(&filter),
                avatar_html = avatar_html,
                username = escape_html(&user.username),
                pill_class = pill_class,
                pill_text = pill_text,
            ));
        }
    }

    let detail_html = if let Some(user) = selected {
        render_user_detail_card(sess, user, selected_reports, current_return_to)
    } else {
        "<div class='admin-user-empty-detail'>Выбери пользователя слева.</div>".to_string()
    };

    format!(
        r#"<div class='admin-users-shell'>
  <div class='admin-users-titlebar'>
    <strong>Пользователи</strong>
  </div>
  <div class='admin-users-grid'>
    <aside class='admin-users-list'>
      <div class='admin-user-tabs'>
        <a href='{all_href}' class='{all_cls}'>Все</a>
        <a href='{active_href}' class='{active_cls}'>Обычные</a>
        <a href='{banned_href}' class='{banned_cls}'>Забаненные</a>
      </div>
      {search_html}
      <div class='admin-user-list-scroll'>{rows_html}</div>
    </aside>
    <section class='admin-users-detail' data-admin-user-detail>{detail_html}</section>
  </div>
</div>"#,
        all_href = escape_html(&all_href),
        active_href = escape_html(&active_href),
        banned_href = escape_html(&banned_href),
        all_cls = if mode == "all" { "active" } else { "" },
        active_cls = if mode == "active" { "active" } else { "" },
        banned_cls = if mode == "banned" { "active" } else { "" },
        search_html = search_html,
        rows_html = rows_html,
        detail_html = detail_html,
    )
}

fn render_servers_panel_body(query: &str, rows_html: &str, embedded: bool) -> String {
    if embedded {
        return format!(
            r#"<div class='card'>
  <div class='search-row'>
    <div class='hstack'>
      <h2 style='margin:0;'>Серверы</h2>
      <span class='pill'>UTC</span>
    </div>
    <div class='center-inline-search'>
      <input type='text' data-persist-key='admin-center-servers-search' data-filter-input='servers' value='{qval}' placeholder='Поиск: название сервера / id' />
      <button type='button' class='btn-soft' data-clear-filter='servers'>Сбросить</button>
    </div>
  </div>
</div>
<div class='card'>
  <div class='servers-list' data-filter-list='servers'>{rows}</div>
</div>"#,
            qval = escape_html(query),
            rows = rows_html,
        );
    }
    format!(
        r#"<div class='card'>
  <div class='search-row'>
    <div class='hstack'>
      <h2 style='margin:0;'>Серверы</h2>
      <span class='pill'>UTC</span>
    </div>
    <form method='get' action='/admin/servers'>
      <input type='text' name='q' value='{qval}' placeholder='Поиск: название сервера / id (пусто = последние)' />
      <button type='submit'>Найти</button>
    </form>
  </div>
</div>
<div class='card'>
  <div class='servers-list'>{rows}</div>
</div>"#,
        qval = escape_html(query),
        rows = rows_html,
    )
}

fn render_db_panel_body(sess: &AdminSession, return_to: &str) -> String {
    let rt = escape_html(return_to);
    format!(
        r#"
<div class='card'>
  <div class='hstack'>
    <h2 style='margin:0;'>База данных</h2>
    <span class='pill'>Прямое выполнение</span>
  </div>
  <div class='small' style='margin-top:10px;'>Операции на этой странице запускаются сразу по нажатию. Интерфейс использует единый UTC-формат времени.</div>
</div>

<div class='db-list'>
  <div class='db-card'>
    <h3>Очистить сообщения и вложения</h3>
    <div class='small'>Удаляет сообщения, реакции, закрепы, chat_reads, записи files и сами файлы. Пользователи, серверы и каналы остаются.</div>
    <div style='height:12px'></div>
    <form method='post' action='/admin/db/wipe_messages'>
      <input type='hidden' name='csrf' value='{csrf}' />
      <input type='hidden' name='return_to' value='{rt}' />
      <button type='submit' class='btn-soft'>Очистить сообщения</button>
    </form>
  </div>

  <div class='db-card'>
    <h3>Очистить серверы и каналы</h3>
    <div class='small danger-note'>Удаляет все серверы, каналы, сообщения и файлы внутри них. Личные сообщения не трогаются.</div>
    <div style='height:12px'></div>
    <form method='post' action='/admin/db/wipe_servers'>
      <input type='hidden' name='csrf' value='{csrf}' />
      <input type='hidden' name='return_to' value='{rt}' />
      <button type='submit' class='btn-danger'>Очистить серверы</button>
    </form>
  </div>

  <div class='db-card'>
    <h3>Сбросить всё, кроме пользователей</h3>
    <div class='small danger-note'>Профили, настройки, сессии, друзья, серверы, ЛС, сообщения и файлы будут удалены. Глобальный сервер создастся заново автоматически.</div>
    <div style='height:12px'></div>
    <form method='post' action='/admin/db/reset_keep_users'>
      <input type='hidden' name='csrf' value='{csrf}' />
      <input type='hidden' name='return_to' value='{rt}' />
      <button type='submit' class='btn-danger'>Сбросить, оставить пользователей</button>
    </form>
  </div>

  <div class='db-card'>
    <h3>Очистить просроченные файлы</h3>
    <div class='small'>Удаляет истёкшие временные вложения и мусорные файлы/thumbs без записей в БД.</div>
    <div style='height:12px'></div>
    <form method='post' action='/admin/db/cleanup_expired_files'>
      <input type='hidden' name='csrf' value='{csrf}' />
      <input type='hidden' name='return_to' value='{rt}' />
      <button type='submit' class='btn-soft'>Очистить файлы</button>
    </form>
  </div>

  <div class='db-card'>
    <h3>Выполнить VACUUM</h3>
    <div class='small'>Пересобирает файл базы данных. Во время работы может потребоваться до ~2× свободного места на диске.</div>
    <div style='height:12px'></div>
    <form method='post' action='/admin/db/vacuum'>
      <input type='hidden' name='csrf' value='{csrf}' />
      <input type='hidden' name='return_to' value='{rt}' />
      <button type='submit' class='btn-soft'>Запустить VACUUM</button>
    </form>
  </div>
</div>
"#,
        csrf = escape_html(&sess.csrf),
        rt = rt,
    )
}

async fn users_list(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let query = q.q.clone().unwrap_or_default().trim().to_string();
    let mode = normalized_user_mode(q.mode.as_deref());
    let embed = q.embed == Some(1);
    let base_path = if embed { "/admin/center" } else { "/admin/users" };

    let body = match fetch_users(&st.db, &query, 200).await {
        Ok(list) => {
            let selected_id = q.user_id.or_else(|| list.iter().find(|u| user_mode_matches(u, mode)).map(|u| u.id));
            let fallback_return_to = user_page_url(base_path, embed, &query, mode, selected_id);
            let current_return_to = safe_admin_return_to(q.return_to.as_deref().unwrap_or(""), &fallback_return_to);
            let reports = match selected_id {
                Some(id) => fetch_user_reports(&st.db, id, 8).await.unwrap_or_default(),
                None => Vec::new(),
            };
            render_users_panel_body(
                &sess,
                &list,
                &query,
                embed,
                mode,
                selected_id,
                &current_return_to,
                &reports,
            )
        }
        Err(err) => format!(
            "<div class='card'><div class='empty-state'>Ошибка БД: {}</div></div>",
            escape_html(&format!("{}", err))
        ),
    };

    if embed {
        embedded_page("Админка • Пользователи", &body, q.msg.as_deref()).into_response()
    } else {
        page("Админка • Пользователи", &body, q.msg.as_deref()).into_response()
    }
}



async fn admin_user_card_fragment(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() { return e.into_response(); }
    if let Err(e) = require_allow_ip(&headers) { return e.into_response(); }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let user = match fetch_user_by_id(&st.db, id).await {
        Ok(Some(v)) => v,
        Ok(None) => return (StatusCode::NOT_FOUND, "Пользователь не найден").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Ошибка БД: {e}")).into_response(),
    };
    let reports = fetch_user_reports(&st.db, id, 8).await.unwrap_or_default();
    let fallback = user_page_url("/admin/users", false, "", "all", Some(id));
    let return_to = safe_admin_return_to(q.return_to.as_deref().unwrap_or(""), &fallback);
    Html(render_user_detail_card(&sess, &user, &reports, &return_to)).into_response()
}

async fn admin_user_details_fragment(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() { return e.into_response(); }
    if let Err(e) = require_allow_ip(&headers) { return e.into_response(); }
    if let Err(r) = require_auth(&st, &headers) { return r.into_response(); }

    match fetch_user_by_id(&st.db, id).await {
        Ok(Some(user)) => Html(render_user_details_modal(&user)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "Пользователь не найден").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Ошибка БД: {e}")).into_response(),
    }
}

#[derive(Deserialize)]
struct ActionForm {
    csrf: String,
    #[serde(default)]
    phrase: String,
    #[serde(default)]
    admin_password: String,
    #[serde(default)]
    return_to: String,
    #[serde(default)]
    reason: String,
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
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ReportStatusForm>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() { return e.into_response(); }
    if let Err(e) = require_allow_ip(&headers) { return e.into_response(); }
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
        sqlx::query("UPDATE user_reports SET status = 'open', resolved_at = NULL, resolved_by = NULL WHERE id = ?")
            .bind(id)
            .execute(&st.db)
            .await
    } else {
        sqlx::query("UPDATE user_reports SET status = ?, resolved_at = ?, resolved_by = NULL WHERE id = ?")
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
    headers: HeaderMap,
    Query(q): Query<SuggestionsQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
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
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<SuggestionStatusForm>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
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
            "UPDATE user_suggestions SET status = 'open', reviewed_at = NULL, reviewed_by = NULL WHERE id = ?",
        )
        .bind(id)
        .execute(&st.db)
        .await
    } else {
        sqlx::query(
            "UPDATE user_suggestions SET status = ?, reviewed_at = ?, reviewed_by = NULL, admin_note = ? WHERE id = ?",
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

async fn gifs_page(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<MsgQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let embedded = q.embed == Some(1);
    let return_to = if embedded { "/admin/gifs?embed=1" } else { "/admin/gifs" };

    let rows = fetch_admin_global_gifs(&st.db).await;

    let body = render_admin_gifs_panel_body(&sess, &rows, return_to);

    if embedded {
        embedded_page("Админка • GIF", &body, q.msg.as_deref()).into_response()
    } else {
        page("Админка • GIF", &body, q.msg.as_deref()).into_response()
    }
}

struct AdminGifRow {
    id: i64,
    original_name: String,
    file_size: i64,
    created_at: String,
}

async fn fetch_admin_global_gifs(db: &SqlitePool) -> Vec<AdminGifRow> {
    sqlx::query(
        r#"
        SELECT id, original_name, file_size, created_at
        FROM gif_assets
        WHERE scope = 'global'
        ORDER BY id DESC
        LIMIT 240
        "#,
    )
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|r| AdminGifRow {
        id: r.get("id"),
        original_name: r.get("original_name"),
        file_size: r.get("file_size"),
        created_at: r.get("created_at"),
    })
    .collect()
}

fn render_admin_gifs_panel_body(sess: &AdminSession, rows: &[AdminGifRow], return_to: &str) -> String {
    let cards = if rows.is_empty() {
        "<div class='empty-state'>Глобальных GIF пока нет. Загрузи первую анимацию слева.</div>".to_string()
    } else {
        rows.iter()
            .map(|r| {
                format!(
                    r#"<div class='admin-gif-card'>
  <div class='admin-gif-thumb'><img src='/admin/gifs/{id}/raw' alt='{name}' loading='lazy'></div>
  <div class='admin-gif-body'>
    <div class='admin-gif-name' title='{name}'>{name}</div>
    <div class='small'>{size} • {created_at}</div>
    <div class='admin-gif-actions'>
      <form method='post' action='/admin/gifs/{id}/delete'>
        <input type='hidden' name='csrf' value='{csrf}'>
        <input type='hidden' name='return_to' value='{return_to}'>
        <button type='submit' class='btn-danger'>Удалить</button>
      </form>
    </div>
  </div>
</div>"#,
                    id = r.id,
                    name = escape_html(&r.original_name),
                    size = escape_html(&admin_format_bytes(r.file_size)),
                    created_at = escape_html(&fmt_admin_dt(&r.created_at)),
                    csrf = escape_html(&sess.csrf),
                    return_to = escape_html(return_to),
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };

    format!(
        r#"<div class='admin-gif-shell'>
  <aside class='admin-gif-upload'>
    <h2>Глобальные GIF</h2>
    <p class='small'>Эти GIF видят все пользователи в пикере. Личные избранные пользователи добавляют сами из чата.</p>
    <form method='post' action='/admin/gifs/upload' enctype='multipart/form-data'>
      <input type='hidden' name='csrf' value='{csrf}'>
      <input type='hidden' name='return_to' value='{return_to}'>
      <input type='file' name='file' accept='image/gif,.gif' required>
      <button type='submit'>Добавить GIF</button>
    </form>
  </aside>
  <section>
    <div class='admin-gif-grid'>{cards}</div>
  </section>
</div>"#,
        csrf = escape_html(&sess.csrf),
        return_to = escape_html(return_to),
        cards = cards,
    )
}

async fn admin_gif_upload(
    State(st): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let mut csrf = String::new();
    let mut return_to = "/admin/gifs".to_string();
    let mut original_name = "global.gif".to_string();
    let mut bytes: Option<Vec<u8>> = None;

    loop {
        let next = match multipart.next_field().await {
            Ok(v) => v,
            Err(_) => return admin_redirect_with_msg("/admin/gifs", "Некорректная форма загрузки").into_response(),
        };
        let Some(field) = next else { break; };
        let name = field.name().unwrap_or("").to_string();
        if name == "csrf" {
            csrf = field.text().await.unwrap_or_default();
        } else if name == "return_to" {
            return_to = safe_admin_return_to(&field.text().await.unwrap_or_default(), "/admin/gifs");
        } else if name == "file" {
            original_name = field.file_name().unwrap_or("global.gif").to_string();
            let data = match field.bytes().await {
                Ok(v) => v,
                Err(_) => return admin_redirect_with_msg("/admin/gifs", "Не удалось прочитать GIF").into_response(),
            };
            bytes = Some(data.to_vec());
        }
    }

    if csrf != sess.csrf {
        return admin_redirect_with_msg(&return_to, "CSRF-токен не совпадает").into_response();
    }
    let Some(data) = bytes else {
        return admin_redirect_with_msg(&return_to, "GIF не выбран").into_response();
    };

    match crate::routes::gifs::save_global_gif_asset(&st.db, &original_name, &data).await {
        Ok(_) => admin_redirect_with_msg(&return_to, "GIF добавлен в глобальный список").into_response(),
        Err(e) => {
            let msg = if e.to_string().contains("gif_required") {
                "Нужен корректный GIF до 50 МБ".to_string()
            } else {
                format!("Ошибка: {e}")
            };
            admin_redirect_with_msg(&return_to, &msg).into_response()
        }
    }
}

async fn admin_gif_delete(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let return_to = safe_admin_return_to(&f.return_to, "/admin/gifs");
    if f.csrf != sess.csrf {
        return admin_redirect_with_msg(&return_to, "CSRF-токен не совпадает").into_response();
    }

    let res = sqlx::query("DELETE FROM gif_assets WHERE id = ? AND scope = 'global'")
        .bind(id)
        .execute(&st.db)
        .await;
    match res {
        Ok(done) if done.rows_affected() > 0 => {
            let _ = cleanup_file_storage_orphans_db(&st.db).await;
            admin_redirect_with_msg(&return_to, "GIF удалён из глобального списка").into_response()
        }
        Ok(_) => admin_redirect_with_msg(&return_to, "GIF не найден").into_response(),
        Err(e) => admin_redirect_with_msg(&return_to, &format!("Ошибка: {e}")).into_response(),
    }
}

#[derive(Clone)]
struct AdminDownloadRow {
    id: i64,
    platform: String,
    version: String,
    original_name: String,
    file_size: i64,
    uploaded_at: String,
    is_active: bool,
}

fn admin_download_platform(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "android" | "mobile" | "apk" => Some("android"),
        "pc" | "windows" | "desktop" => Some("pc"),
        _ => None,
    }
}

fn admin_download_platform_label(platform: &str) -> &'static str {
    match platform {
        "android" => "Android APK",
        "pc" => "ПК клиент",
        _ => "Клиент",
    }
}

fn admin_download_ext(original_name: &str, platform: &str) -> Option<String> {
    let sanitized = admin_sanitize_filename(original_name);
    let ext = std::path::Path::new(&sanitized)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    match platform {
        "android" if ext == "apk" => Some(ext),
        "pc" if matches!(ext.as_str(), "exe" | "msi" | "zip" | "7z" | "rar") => Some(ext),
        _ => None,
    }
}

fn admin_download_mime(ext: &str) -> &'static str {
    match ext {
        "apk" => "application/vnd.android.package-archive",
        "exe" => "application/vnd.microsoft.portable-executable",
        "msi" => "application/x-msi",
        "zip" => "application/zip",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        _ => "application/octet-stream",
    }
}

async fn fetch_admin_downloads(db: &SqlitePool) -> Vec<AdminDownloadRow> {
    sqlx::query(
        r#"
        SELECT id, platform, version, original_name, file_size, uploaded_at, is_active
        FROM app_downloads
        ORDER BY platform ASC, is_active DESC, id DESC
        LIMIT 80
        "#,
    )
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|r| AdminDownloadRow {
        id: r.get("id"),
        platform: r.get("platform"),
        version: r.try_get("version").unwrap_or_default(),
        original_name: r.try_get("original_name").unwrap_or_default(),
        file_size: r.try_get("file_size").unwrap_or(0),
        uploaded_at: r.try_get("uploaded_at").unwrap_or_default(),
        is_active: r.try_get::<i64, _>("is_active").unwrap_or(0) != 0,
    })
    .collect()
}

fn render_admin_downloads_panel_body(sess: &AdminSession, rows: &[AdminDownloadRow], return_to: &str) -> String {
    let cards = if rows.is_empty() {
        "<div class='empty-state'>Загрузок пока нет. Загрузите APK или ПК клиент слева.</div>".to_string()
    } else {
        rows.iter()
            .map(|r| {
                let platform = admin_download_platform_label(&r.platform);
                let active = if r.is_active { "Активна" } else { "Скрыта" };
                let active_cls = if r.is_active { "status-active" } else { "" };
                let download_link = if r.is_active {
                    format!(
                        "<a href='/api/downloads/{}/file' target='_blank' rel='noopener'>Скачать</a>",
                        escape_html(&r.platform)
                    )
                } else {
                    String::new()
                };
                let version_text = if r.version.trim().is_empty() { "без версии" } else { r.version.as_str() };
                format!(
                    r#"<div class='admin-download-card'>
  <div class='admin-download-top'>
    <div>
      <div class='admin-download-title'>{platform}</div>
      <div class='admin-download-meta'>{name}</div>
    </div>
    <span class='status-badge {active_cls}'>{active}</span>
  </div>
  <div class='admin-download-meta'>Версия: {version}</div>
  <div class='admin-download-meta'>{size} • {uploaded_at}</div>
  <div class='admin-download-actions'>
    {download_link}
    <form method='post' action='/admin/downloads/{id}/delete'>
      <input type='hidden' name='csrf' value='{csrf}'>
      <input type='hidden' name='return_to' value='{return_to}'>
      <button type='submit' class='btn-danger'>Удалить</button>
    </form>
  </div>
</div>"#,
                    id = r.id,
                    platform = escape_html(platform),
                    name = escape_html(&r.original_name),
                    version = escape_html(version_text),
                    size = escape_html(&admin_format_bytes(r.file_size)),
                    uploaded_at = escape_html(&fmt_admin_dt(&r.uploaded_at)),
                    active = active,
                    active_cls = active_cls,
                    download_link = download_link,
                    csrf = escape_html(&sess.csrf),
                    return_to = escape_html(return_to),
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };

    format!(
        r#"<div class='admin-download-shell'>
  <aside class='admin-download-upload'>
    <h2>Загрузки приложения</h2>
    <p class='small'>Файлы отсюда раздаются сервером на странице скачивания LaBerry. Новая загрузка заменяет активную версию выбранной платформы.</p>
    <form method='post' action='/admin/downloads/upload' enctype='multipart/form-data'>
      <input type='hidden' name='csrf' value='{csrf}'>
      <input type='hidden' name='return_to' value='{return_to}'>
      <label class='small'>Платформа</label>
      <select name='platform' required>
        <option value='android'>Android APK</option>
        <option value='pc'>ПК клиент</option>
      </select>
      <label class='small'>Версия</label>
      <input type='text' name='version' maxlength='64' placeholder='Например: 0.9.3'>
      <input type='file' name='file' accept='.apk,.exe,.msi,.zip,.7z,.rar' required>
      <button type='submit'>Загрузить</button>
    </form>
  </aside>
  <section>
    <div class='admin-download-grid'>{cards}</div>
  </section>
</div>"#,
        csrf = escape_html(&sess.csrf),
        return_to = escape_html(return_to),
        cards = cards,
    )
}

async fn downloads_page(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<MsgQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let embedded = q.embed == Some(1);
    let return_to = if embedded { "/admin/downloads?embed=1" } else { "/admin/downloads" };
    let rows = fetch_admin_downloads(&st.db).await;
    let body = render_admin_downloads_panel_body(&sess, &rows, return_to);

    if embedded {
        embedded_page("Админка • Загрузки", &body, q.msg.as_deref()).into_response()
    } else {
        page("Админка • Загрузки", &body, q.msg.as_deref()).into_response()
    }
}

async fn admin_download_upload(
    State(st): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let mut csrf = String::new();
    let mut return_to = "/admin/downloads".to_string();
    let mut platform_raw = "android".to_string();
    let mut version = String::new();
    let mut original_name = "laberry.apk".to_string();
    let mut bytes: Option<Vec<u8>> = None;

    loop {
        let next = match multipart.next_field().await {
            Ok(v) => v,
            Err(_) => return admin_redirect_with_msg("/admin/downloads", "Некорректная форма загрузки").into_response(),
        };
        let Some(field) = next else { break; };
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "csrf" => csrf = field.text().await.unwrap_or_default(),
            "return_to" => {
                return_to = safe_admin_return_to(&field.text().await.unwrap_or_default(), "/admin/downloads");
            }
            "platform" => platform_raw = field.text().await.unwrap_or_else(|_| "android".to_string()),
            "version" => version = field.text().await.unwrap_or_default().trim().chars().take(64).collect(),
            "file" => {
                original_name = field.file_name().unwrap_or("laberry.bin").to_string();
                let data = match field.bytes().await {
                    Ok(v) => v,
                    Err(_) => return admin_redirect_with_msg("/admin/downloads", "Не удалось прочитать файл").into_response(),
                };
                bytes = Some(data.to_vec());
            }
            _ => {}
        }
    }

    if csrf != sess.csrf {
        return admin_redirect_with_msg(&return_to, "CSRF-токен не совпадает").into_response();
    }

    let Some(platform) = admin_download_platform(&platform_raw) else {
        return admin_redirect_with_msg(&return_to, "Неизвестная платформа").into_response();
    };
    let Some(data) = bytes else {
        return admin_redirect_with_msg(&return_to, "Файл не выбран").into_response();
    };
    if data.is_empty() {
        return admin_redirect_with_msg(&return_to, "Файл пустой").into_response();
    }
    if data.len() > 512 * 1024 * 1024 {
        return admin_redirect_with_msg(&return_to, "Файл слишком большой (максимум 512 МБ)").into_response();
    }
    let Some(ext) = admin_download_ext(&original_name, platform) else {
        return admin_redirect_with_msg(&return_to, "Для Android нужен .apk, для ПК: .exe, .msi, .zip, .7z или .rar").into_response();
    };

    let storage_dir = PathBuf::from("storage/app_downloads");
    if let Err(e) = fs::create_dir_all(&storage_dir).await {
        return admin_redirect_with_msg(&return_to, &format!("Не удалось создать каталог: {e}")).into_response();
    }
    let stored_filename = format!("{}.{}", Uuid::new_v4(), ext);
    let storage_path = storage_dir.join(stored_filename);
    if let Err(e) = fs::write(&storage_path, &data).await {
        return admin_redirect_with_msg(&return_to, &format!("Не удалось сохранить файл: {e}")).into_response();
    }

    let now = auth::now_iso();
    let mime = admin_download_mime(&ext);
    let storage_path_str = storage_path.to_string_lossy().to_string();
    let mut tx = match st.db.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            let _ = fs::remove_file(&storage_path).await;
            return admin_redirect_with_msg(&return_to, &format!("Ошибка БД: {e}")).into_response();
        }
    };

    let res = async {
        sqlx::query("UPDATE app_downloads SET is_active = 0 WHERE platform = ?")
            .bind(platform)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            r#"
            INSERT INTO app_downloads(platform, version, original_name, mime_type, file_size, storage_path, uploaded_at, is_active)
            VALUES(?, ?, ?, ?, ?, ?, ?, 1)
            "#,
        )
        .bind(platform)
        .bind(&version)
        .bind(&original_name)
        .bind(mime)
        .bind(data.len() as i64)
        .bind(&storage_path_str)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }
    .await;

    match res {
        Ok(_) => admin_redirect_with_msg(&return_to, "Файл приложения загружен").into_response(),
        Err(e) => {
            let _ = fs::remove_file(&storage_path).await;
            admin_redirect_with_msg(&return_to, &format!("Ошибка: {e}")).into_response()
        }
    }
}

async fn admin_download_delete(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let return_to = safe_admin_return_to(&f.return_to, "/admin/downloads");
    if f.csrf != sess.csrf {
        return admin_redirect_with_msg(&return_to, "CSRF-токен не совпадает").into_response();
    }

    let row = sqlx::query("SELECT storage_path FROM app_downloads WHERE id = ? LIMIT 1")
        .bind(id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten();
    let Some(row) = row else {
        return admin_redirect_with_msg(&return_to, "Загрузка не найдена").into_response();
    };
    let storage_path: String = row.try_get("storage_path").unwrap_or_default();

    let res = sqlx::query("DELETE FROM app_downloads WHERE id = ?")
        .bind(id)
        .execute(&st.db)
        .await;

    match res {
        Ok(done) if done.rows_affected() > 0 => {
            if !storage_path.trim().is_empty() {
                let _ = fs::remove_file(PathBuf::from(storage_path)).await;
            }
            admin_redirect_with_msg(&return_to, "Загрузка удалена").into_response()
        }
        Ok(_) => admin_redirect_with_msg(&return_to, "Загрузка не найдена").into_response(),
        Err(e) => admin_redirect_with_msg(&return_to, &format!("Ошибка: {e}")).into_response(),
    }
}

async fn user_ban(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    action_user_common(st, headers, id, f, UserAction::Заблокировать).await
}

async fn user_purge_content(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    action_user_common(st, headers, id, f, UserAction::PurgeContent).await
}

async fn user_unban(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    action_user_common(st, headers, id, f, UserAction::Unban).await
}

async fn user_ban_forever(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    action_user_common(st, headers, id, f, UserAction::DeleteAccount).await
}

enum UserAction {
    Заблокировать,
    Unban,
    PurgeContent,
    DeleteAccount,
}

async fn action_user_common(
    st: AppState,
    headers: HeaderMap,
    user_id: i64,
    f: ActionForm,
    act: UserAction,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    if f.csrf != sess.csrf {
        return admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/users"), "CSRF-токен не совпадает").into_response();
    }

    let user_row = sqlx::query("SELECT username, COALESCE(email,'') AS email FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(&st.db)
        .await;

    let Ok(Some(r)) = user_row else {
        return admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/users"), "Пользователь не найден").into_response();
    };
    let username: String = r.get("username");
    let email: String = r.get("email");

    let _re = test_user_re();
    let _is_test = is_test_user(&_re, &username, &email);

    let res = match act {
        UserAction::Заблокировать => ban_user_exec(&st.db, user_id, &f.reason).await,
        UserAction::Unban => unban_user_exec(&st.db, user_id).await,
        UserAction::PurgeContent => purge_user_content_exec(&st.db, user_id).await,
        UserAction::DeleteAccount => purge_user_exec(&st.db, user_id).await,
    };

    match res {
        Ok(_) => admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/users"), "Готово").into_response(),
        Err(e) => admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/users"), &format!("Ошибка: {}", e)).into_response(),
    }
}

// =============================
// Тестовые пользователи page
// =============================

#[derive(Deserialize, Default)]
struct TestUsersQuery {
    msg: Option<String>,
}

async fn test_users_page(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TestUsersQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let re = test_user_re();
    let list = fetch_test_users(&st.db, &re, 1000).await;

    let mut rows = String::new();
    match list {
        Ok(users) => {
            for u in &users {
                rows.push_str(&format!(
                    r#"<tr>
<td><input type='checkbox' name='user_ids' value='{id}' /></td>
<td>#{id}</td>
<td>{username}<div class='small'>{email}</div></td>
<td>{banned}</td>
<td class='small'>{created_at}</td>
</tr>"#,
                    id = u.id,
                    username = escape_html(&u.username),
                    email = escape_html(&u.email),
                    banned = if u.is_banned { "заблокирован" } else { "" },
                    created_at = escape_html(&fmt_admin_dt(&u.created_at)),
                ));
            }

            let body = format!(
                r#"<div class='card'>
<h2>Тестовые пользователи</h2>
<div class='small'>Шаблон: <code>{re}</code> (измени LB_TEST_USER_REGEX, если нужно)</div>
<form method='post' action='/admin/test-users'>
  <input type='hidden' name='csrf' value='{csrf}' />
  <table class='table'>
  <thead><tr><th></th><th>ID</th><th>Пользователь</th><th>Статус</th><th>Создан</th></tr></thead>
  <tbody>
  {rows}
  </tbody>
  </table>
  <div style='height:10px'></div>
  <button type='submit' class='btn-danger'>Удалить выбранных</button>
</form>
</div>"#,
                re = escape_html(re.as_str()),
                csrf = escape_html(&sess.csrf),
                rows = rows
            );

            return page("Админка • Тестовые пользователи", &body, q.msg.as_deref()).into_response();
        }
        Err(e) => {
            let body = format!(
                "<div class='card'>Ошибка БД: {}</div>",
                escape_html(&format!("{}", e))
            );
            return page("Админка • Тестовые пользователи", &body, q.msg.as_deref()).into_response();
        }
    }
}

#[derive(Deserialize)]
struct DeleteTestUsersForm {
    csrf: String,
    #[serde(default)]
    user_ids: Vec<i64>,
}

async fn test_users_delete(
    State(st): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<DeleteTestUsersForm>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    if f.csrf != sess.csrf {
        return admin_redirect_with_msg("/admin/test-users", "CSRF-токен не совпадает").into_response();
    }

    if f.user_ids.is_empty() {
        return admin_redirect_with_msg("/admin/test-users", "Ничего не выбрано").into_response();
    }

    // Safety: verify they are actually test users
    let re = test_user_re();
    for id in &f.user_ids {
        let row = sqlx::query("SELECT username, COALESCE(email,'') AS email FROM users WHERE id = ?")
            .bind(*id)
            .fetch_optional(&st.db)
            .await;
        let Ok(Some(r)) = row else {
            return admin_redirect_with_msg("/admin/test-users", "Пользователь не найден").into_response();
        };
        let username: String = r.get("username");
        let email: String = r.get("email");
        if !is_test_user(&re, &username, &email) {
            return admin_redirect_with_msg("/admin/test-users", "Операция отклонена: в списке есть обычный пользователь").into_response();
        }
    }

    for id in &f.user_ids {
        if let Err(e) = purge_user_exec(&st.db, *id).await {
            return admin_redirect_with_msg("/admin/test-users", &format!("Ошибка: {}", e)).into_response();
        }
    }

    admin_redirect_with_msg("/admin/test-users", "Готово").into_response()
}

// =============================
// Серверы list + actions
// =============================

async fn servers_list(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let query = q.q.clone().unwrap_or_default().trim().to_string();
    let servers = fetch_servers(&st.db, &query, 200).await;

    let mut rows_html = String::new();
    match servers {
        Ok(list) => {
            if list.is_empty() {
                rows_html.push_str("<div class='empty-state'>Серверы не найдены.</div>");
            } else {
                for s in list {
                    rows_html.push_str(&format!(
                        r#"<div class='server-row-card'>
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
                            <input type='hidden' name='return_to' value='{return_to}' />
                            <button type='submit' class='btn-soft'>Добавить всех пользователей</button>
                            </form>
                            <form method='post' action='/admin/servers/{id}/delete' class='inline-form'>
                            <input type='hidden' name='csrf' value='{csrf}' />
                            <input type='hidden' name='return_to' value='{return_to}' />
                            <button type='submit' class='btn-danger'>Удалить сервер</button>
                            </form>
                        </div>
                        </div>"#,
                        id = s.id,
                        name = escape_html(&s.name),
                        owner_id = s.owner_id,
                        owner_name = escape_html(&s.owner_username),
                        created_at = escape_html(&fmt_admin_dt(&s.created_at)),
                        csrf = escape_html(&sess.csrf),
                        return_to = escape_html(&safe_admin_return_to(q.return_to.as_deref().unwrap_or(""), if q.embed == Some(1) { "/admin/center?view=servers" } else { "/admin/servers" })),
                    ));
                }
            }
        }
        Err(err) => {
            rows_html.push_str(&format!(
                "<div class='empty-state'>Ошибка БД: {}</div>",
                escape_html(&format!("{}", err))
            ));
        }
    }
    
    let body = render_servers_panel_body(&query, &rows_html, q.embed == Some(1));

    if q.embed == Some(1) {
        embedded_page("Админка • Серверы", &body, q.msg.as_deref()).into_response()
    } else {
        page("Админка • Серверы", &body, q.msg.as_deref()).into_response()
    }
}

async fn server_delete(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    if f.csrf != sess.csrf {
        return admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/servers"), "CSRF-токен не совпадает").into_response();
    }

    let row = sqlx::query("SELECT name FROM servers WHERE id = ?")
        .bind(id)
        .fetch_optional(&st.db)
        .await;

    let Ok(Some(r)) = row else {
        return admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/servers"), "Сервер не найден").into_response();
    };
    let name: String = r.get("name");

    let _re = test_server_re();
    let _is_test = is_test_server(&_re, &name);

    match purge_server_exec(&st.db, id).await {
        Ok(_) => admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/servers"), "Готово").into_response(),
        Err(e) => admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/servers"), &format!("Ошибка: {}", e)).into_response(),
    }
}


async fn server_add_all_users(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let return_to = safe_admin_return_to(&f.return_to, "/admin/servers");
    if f.csrf != sess.csrf {
        return admin_redirect_with_msg(&return_to, "CSRF-токен не совпадает").into_response();
    }

    let exists = sqlx::query_scalar::<_, i64>("SELECT 1 FROM servers WHERE id = ? LIMIT 1")
        .bind(id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten()
        .is_some();

    if !exists {
        return admin_redirect_with_msg(&return_to, "Сервер не найден").into_response();
    }

    let res = sqlx::query(
        r#"
        INSERT OR IGNORE INTO server_members(server_id, user_id, role)
        SELECT ?, id, 'member'
        FROM users
        WHERE is_banned = 0
        "#,
    )
    .bind(id)
    .execute(&st.db)
    .await;

    match res {
        Ok(done) => admin_redirect_with_msg(
            &return_to,
            &format!("Готово. Добавлено пользователей: {}", done.rows_affected()),
        )
        .into_response(),
        Err(e) => admin_redirect_with_msg(&return_to, &format!("Ошибка: {}", e)).into_response(),
    }
}


// =============================
// Инструменты базы данных
// =============================

async fn db_tools_page(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<MsgQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let body = render_db_panel_body(
        &sess,
        if q.embed == Some(1) { "/admin/center?view=db" } else { "/admin/db" },
    );

    if q.embed == Some(1) {
        embedded_page("Админка • База данных", &body, q.msg.as_deref()).into_response()
    } else {
        page("Админка • База данных", &body, q.msg.as_deref()).into_response()
    }
}

async fn db_wipe_messages_post(
    State(st): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    db_action_common(st, headers, f, DbAction::WipeMessages).await
}

async fn db_wipe_servers_post(
    State(st): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    db_action_common(st, headers, f, DbAction::WipeServers).await
}

async fn db_reset_keep_users_post(
    State(st): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    db_action_common(st, headers, f, DbAction::ResetKeepUsers).await
}

async fn db_vacuum_post(
    State(st): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    db_action_common(st, headers, f, DbAction::Vacuum).await
}

async fn db_cleanup_expired_files_post(
    State(st): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let return_to = safe_admin_return_to(&f.return_to, "/admin/db");
    if f.csrf != sess.csrf {
        return admin_redirect_with_msg(&return_to, "CSRF-токен не совпадает").into_response();
    }

    crate::routes::files::cleanup_expired_files(&st).await;
    admin_redirect_with_msg(&return_to, "Готово. Просроченные файлы и мусорные thumbs очищены.").into_response()
}


enum DbAction {
    WipeMessages,
    WipeServers,
    ResetKeepUsers,
    Vacuum,
}

async fn db_action_common(
    st: AppState,
    headers: HeaderMap,
    f: ActionForm,
    act: DbAction,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    if f.csrf != sess.csrf {
        return admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/db"), "CSRF-токен не совпадает").into_response();
    }
    let res = match act {
        DbAction::WipeMessages => wipe_all_messages_exec(&st.db).await,
        DbAction::WipeServers => wipe_all_servers_exec(&st.db).await,
        DbAction::ResetKeepUsers => reset_db_keep_users_exec(&st.db).await,
        DbAction::Vacuum => vacuum_exec(&st.db).await,
    };

    match res {
        Ok(_) => admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/db"), "Готово").into_response(),
        Err(e) => admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/db"), &format!("Ошибка: {}", e)).into_response(),
    }
}





#[derive(Deserialize)]
struct HomieJsonForm {
    csrf: String,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    message: String,
}

#[derive(Serialize)]
struct HomieJsonResponse {
    ok: bool,
    answer: String,
    error: String,
}

#[derive(Serialize)]
struct HomieProxyResponse {
    ok: bool,
    error: String,
    upstream: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct HomieUpstreamRequest {
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

async fn homie_health_get(
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

async fn homie_tools_get(
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

async fn homie_chat_post(
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

async fn homie_reset_post(
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

fn render_homie_center_panel(sess: &AdminSession) -> String {
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
        csrf = escape_html(&sess.csrf),
    )
}

async fn center_page(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<MsgQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let users_total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users").fetch_one(&st.db).await.unwrap_or(0);
    let servers_total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM servers").fetch_one(&st.db).await.unwrap_or(0);
    let messages_total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages").fetch_one(&st.db).await.unwrap_or(0);
    let banned_total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE is_banned = 1").fetch_one(&st.db).await.unwrap_or(0);

    let users_query = q.q.clone().unwrap_or_default().trim().to_string();
    let users = fetch_users(&st.db, &users_query, 200).await.unwrap_or_default();
    let users_mode = normalized_user_mode(q.mode.as_deref());
    let selected_id = q.user_id.or_else(|| users.iter().find(|u| user_mode_matches(u, users_mode)).map(|u| u.id));
    let users_return_to = user_page_url("/admin/center", true, &users_query, users_mode, selected_id);
    let user_reports = match selected_id {
        Some(id) => fetch_user_reports(&st.db, id, 8).await.unwrap_or_default(),
        None => Vec::new(),
    };
    let users_panel = render_users_panel_body(
        &sess,
        &users,
        &users_query,
        true,
        users_mode,
        selected_id,
        &users_return_to,
        &user_reports,
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
    let gif_rows = fetch_admin_global_gifs(&st.db).await;
    let gifs_panel = render_admin_gifs_panel_body(&sess, &gif_rows, "/admin/center?view=gifs");
    let download_rows = fetch_admin_downloads(&st.db).await;
    let downloads_panel = render_admin_downloads_panel_body(&sess, &download_rows, "/admin/center?view=downloads");
    let suggestion_status = normalized_suggestion_status(q.status.as_deref());
    let suggestions = fetch_suggestions(&st.db, suggestion_status, 120).await.unwrap_or_default();
    let suggestions_return_to = suggestions_page_url("/admin/center", true, suggestion_status);
    let suggestions_panel = render_suggestions_panel_body(&sess, &suggestions, suggestion_status, true, &suggestions_return_to);

    let msg_rows = sqlx::query(
        r#"SELECT m.id, COALESCE(m.content,'') AS content, m.timestamp AS created_at, COALESCE(u.username,'Системный') AS username,
                  COALESCE(c.name,'ЛС/скрытый чат') AS chat_name, c.id AS chat_id, c.server_id
           FROM messages m
           LEFT JOIN users u ON u.id = m.sender_id
           LEFT JOIN chats c ON c.id = m.chat_id
           ORDER BY m.id DESC
           LIMIT 120"#,
    ).fetch_all(&st.db).await.unwrap_or_default();
    let mut chat_items = String::new();
    let mut feed_items = String::new();
    use std::collections::BTreeMap;
    let mut chat_names: BTreeMap<i64, (String, i64)> = BTreeMap::new();
    for r in msg_rows {
        let chat_id: i64 = r.get("chat_id");
        let chat_name: String = r.get("chat_name");
        let username: String = r.get("username");
        let content: String = r.get("content");
        let created_at: String = r.get("created_at");
        let server_id: Option<i64> = r.get("server_id");
        let text = if content.trim().is_empty() { "[вложение или пустое сообщение]".to_string() } else { content.clone() };
        let preview = if text.chars().count() > 48 { format!("{}…", text.chars().take(48).collect::<String>()) } else { text.clone() };
        let location = match server_id { Some(sid) => format!("Сервер #{sid}"), None => "Личные сообщения".to_string() };
        chat_names.entry(chat_id).or_insert((chat_name.clone(), 0)).1 += 1;
        feed_items.push_str(&format!(
            r#"<div class='center-feed-item' data-chat-feed='{chat_id}'>
  <div class='center-feed-head'>
    <div>
      <div class='center-feed-author'>{author}</div>
      <div class='center-feed-loc'>{location} · {chat_name}</div>
    </div>
    <div class='center-feed-time'>{time}</div>
  </div>
  <div class='center-feed-text'>{text}</div>
</div>"#,
            chat_id=chat_id, author=escape_html(&username), location=escape_html(&location), chat_name=escape_html(&chat_name),
            time=escape_html(&fmt_admin_dt(&created_at)), text=render_admin_message_html(&text),
        ));
        let _ = preview;
    }
    for (chat_id, (chat_name, count)) in chat_names.iter() {
        chat_items.push_str(&format!(
            r#"<button type='button' class='center-chat-item' data-chat-select='{chat_id}'>
  <div class='center-chat-title'>{chat_name}</div>
  <div class='center-chat-meta'>{count} сообщений</div>
</button>"#,
            chat_id=chat_id, chat_name=escape_html(chat_name), count=count,
        ));
    }
    if chat_items.is_empty() {
        chat_items.push_str("<div class='empty-state'>Пока нет чатов для просмотра.</div>");
        feed_items.push_str("<div class='empty-state'>Пока нет сообщений.</div>");
    }
    let messenger_panel = format!(
        r#"<div class='card'>
  <div class='hstack'>
    <h2 style='margin:0;'>Мессенджер</h2>
    <span class='pill'>Read-only</span>
    <span class='pill'>Без iframe</span>
  </div>
  <div class='small' style='margin-top:10px;'>Одна вкладка браузера: слева чаты, справа поток сообщений. Секция запоминает выбранный чат.</div>
</div>
<div class='admin-messenger'>
  <div class='admin-messenger-sidebar'>
    <div class='center-inline-search'>
      <input type='text' data-persist-key='admin-center-messenger-search' data-chat-search placeholder='Фильтр по чатам' />
      <button type='button' class='btn-soft' data-clear-chat-search>Сбросить</button>
    </div>
    <div class='admin-chat-list'>{chat_items}</div>
  </div>
  <div class='admin-messenger-main'>
    <div class='admin-chat-header'>
      <div>
        <div class='panel-stage-title' style='font-size:18px;'>Поток сообщений</div>
        <div class='panel-stage-sub'>Последние 120 сообщений. Переключай чаты без потери состояния панели.</div>
      </div>
      <span class='pill'>UTC</span>
    </div>
    <div class='center-feed-list admin-chat-feed'>{feed_items}</div>
  </div>
</div>"#,
        chat_items=chat_items, feed_items=feed_items,
    );

    let overview_panel = format!(
        r#"<div class='center-hero'>
  <div>
    <h2 class='center-hero-title'>Центр управления</h2>
    <div class='center-hero-sub'>Одна рабочая область для админки и мессенджера. Секции раскрываются по всей доступной ширине, а поиски и выбранная панель сохраняются.</div>
  </div>
  <div class='center-stat-row'>
    <div class='center-stat'><div class='center-stat-label'>Пользователи</div><div class='center-stat-value'>{users_total}</div></div>
    <div class='center-stat'><div class='center-stat-label'>Серверы</div><div class='center-stat-value'>{servers_total}</div></div>
    <div class='center-stat'><div class='center-stat-label'>Сообщения</div><div class='center-stat-value'>{messages_total}</div></div>
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
    <div class='center-note-line'>• мессенджер открыт в read-only режиме;</div>
    <div class='center-note-line'>• Homie AI остаётся отдельным инструментом;</div>
    <div class='center-note-line'>• время в админке показывается в UTC.</div>
  </div></div>
</div>"#,
        users_total=users_total, servers_total=servers_total, messages_total=messages_total, banned_total=banned_total,
    );

    let homie_panel = render_homie_center_panel(&sess);

    let body = format!(
        r#"<div class='panel-shell'>
  <aside class='panel-sidebar'>
    <button type='button' class='center-nav-item panel-switch' data-center-switch='overview'><strong>Центр</strong><span class='small'>Общий вид и точка входа в остальные панели.</span></button>
    <button type='button' class='center-nav-item panel-switch' data-center-switch='users'><strong>Пользователи</strong><span class='small'>Почта, ник и действия по аккаунтам без прыжков по страницам.</span></button>
    <button type='button' class='center-nav-item panel-switch' data-center-switch='suggestions'><strong>Предложения</strong><span class='small'>Идеи пользователей из настроек и быстрый просмотр статусов.</span></button>
    <button type='button' class='center-nav-item panel-switch' data-center-switch='servers'><strong>Серверы</strong><span class='small'>Проверка владельцев и удаление прямо внутри рабочей области.</span></button>
    <button type='button' class='center-nav-item panel-switch' data-center-switch='gifs'><strong>GIF</strong><span class='small'>Глобальный список анимированных стикеров для пользователей.</span></button>
    <button type='button' class='center-nav-item panel-switch' data-center-switch='downloads'><strong>Загрузки</strong><span class='small'>APK и ПК клиент, которые сервер отдает на странице скачивания.</span></button>
    <button type='button' class='center-nav-item panel-switch' data-center-switch='db'><strong>База данных</strong><span class='small'>Сервисные действия и обслуживание без отдельной вкладки.</span></button>
    <button type='button' class='center-nav-item panel-switch' data-center-switch='messenger'><strong>Мессенджер</strong><span class='small'>Read-only поток и переключение чатов в той же странице.</span></button>
    <button type='button' class='center-nav-item panel-switch' data-center-switch='homie'><strong>Homie AI</strong><span class='small'>Личный агент только для админки.</span></button>
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
      <div class='panel-view' data-panel-view='gifs' data-stage-title='Глобальные GIF' data-stage-sub='Анимированные стикеры, доступные всем пользователям.'>{gifs_panel}</div>
      <div class='panel-view' data-panel-view='downloads' data-stage-title='Загрузки приложения' data-stage-sub='APK и ПК клиент, которые пользователи скачивают с сервера.'>{downloads_panel}</div>
      <div class='panel-view' data-panel-view='db' data-stage-title='Панель базы данных' data-stage-sub='Сервисные инструменты открываются здесь же, без переходов по страницам.'>{db_panel}</div>
      <div class='panel-view' data-panel-view='messenger' data-stage-title='Мессенджер внутри админки' data-stage-sub='Read-only поток сообщений и переключение чатов без второй вкладки браузера.'>{messenger_panel}</div>
      <div class='panel-view' data-panel-view='homie' data-stage-title='Homie AI' data-stage-sub='Локальный агент админ-панели.'>{homie_panel}</div>
    </div>
  </section>
</div>"#,
        overview_panel=overview_panel, users_panel=users_panel, suggestions_panel=suggestions_panel, servers_panel=servers_panel, gifs_panel=gifs_panel, downloads_panel=downloads_panel, db_panel=db_panel, messenger_panel=messenger_panel, homie_panel=homie_panel,
    );

    page("Админка • Центр", &body, q.msg.as_deref()).into_response()
}


// =============================
// DB helpers
// =============================

#[derive(Clone)]
struct UserRow {
    id: i64,
    username: String,
    email: String,
    is_banned: bool,
    created_at: String,
    is_online: bool,
    presence_status: String,
    presence_updated_at: String,
    avatar_file_id: Option<i64>,
    ban_reason: String,
    ban_at: String,
}

#[derive(Clone)]
struct UserReportRow {
    id: i64,
    reporter_id: i64,
    reporter_username: String,
    target_user_id: i64,
    message_id: Option<i64>,
    reason: String,
    message: String,
    status: String,
    created_at: String,
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

fn map_user_row(r: sqlx::sqlite::SqliteRow) -> UserRow {
    UserRow {
        id: r.get("id"),
        username: r.get("username"),
        email: r.get("email"),
        is_banned: r.get::<i64, _>("is_banned") != 0,
        created_at: r.get("created_at"),
        is_online: r.get::<i64, _>("is_online") != 0,
        presence_status: r.get("presence_status"),
        presence_updated_at: r.get("presence_updated_at"),
        avatar_file_id: r.get("avatar_file_id"),
        ban_reason: r.get("ban_reason"),
        ban_at: r.get("ban_at"),
    }
}

async fn fetch_users(db: &SqlitePool, q: &str, limit: i64) -> anyhow::Result<Vec<UserRow>> {
    let select = r#"
        SELECT u.id,
               u.username,
               COALESCE(u.email,'') AS email,
               u.is_banned,
               u.created_at,
               COALESCE(p.is_online, 0) AS is_online,
               COALESCE(p.status, 'offline') AS presence_status,
               COALESCE(p.updated_at, '') AS presence_updated_at,
               up.avatar_file_id AS avatar_file_id,
               COALESCE((SELECT me.reason FROM moderation_events me WHERE me.user_id = u.id AND me.kind = 'ban' ORDER BY me.id DESC LIMIT 1), '') AS ban_reason,
               COALESCE((SELECT me.created_at FROM moderation_events me WHERE me.user_id = u.id AND me.kind = 'ban' ORDER BY me.id DESC LIMIT 1), '') AS ban_at
        FROM users u
        LEFT JOIN user_presence p ON p.user_id = u.id
        LEFT JOIN user_profile up ON up.user_id = u.id
    "#;

    let rows = if q.is_empty() {
        sqlx::query(&format!("{select} ORDER BY u.id DESC LIMIT ?"))
            .bind(limit)
            .fetch_all(db)
            .await?
    } else if let Ok(id) = q.parse::<i64>() {
        sqlx::query(&format!("{select} WHERE u.id = ? ORDER BY u.id DESC LIMIT ?"))
            .bind(id)
            .bind(limit)
            .fetch_all(db)
            .await?
    } else {
        let like = format!("%{}%", q);
        sqlx::query(&format!("{select} WHERE u.username LIKE ? OR u.email LIKE ? ORDER BY u.id DESC LIMIT ?"))
            .bind(&like)
            .bind(&like)
            .bind(limit)
            .fetch_all(db)
            .await?
    };

    Ok(rows.into_iter().map(map_user_row).collect())
}

async fn fetch_user_by_id(db: &SqlitePool, id: i64) -> anyhow::Result<Option<UserRow>> {
    let select = r#"
        SELECT u.id,
               u.username,
               COALESCE(u.email,'') AS email,
               u.is_banned,
               u.created_at,
               COALESCE(p.is_online, 0) AS is_online,
               COALESCE(p.status, 'offline') AS presence_status,
               COALESCE(p.updated_at, '') AS presence_updated_at,
               up.avatar_file_id AS avatar_file_id,
               COALESCE((SELECT me.reason FROM moderation_events me WHERE me.user_id = u.id AND me.kind = 'ban' ORDER BY me.id DESC LIMIT 1), '') AS ban_reason,
               COALESCE((SELECT me.created_at FROM moderation_events me WHERE me.user_id = u.id AND me.kind = 'ban' ORDER BY me.id DESC LIMIT 1), '') AS ban_at
        FROM users u
        LEFT JOIN user_presence p ON p.user_id = u.id
        LEFT JOIN user_profile up ON up.user_id = u.id
        WHERE u.id = ?
        LIMIT 1
    "#;
    let row = sqlx::query(select).bind(id).fetch_optional(db).await?;
    Ok(row.map(map_user_row))
}

async fn fetch_user_reports(db: &SqlitePool, user_id: i64, limit: i64) -> anyhow::Result<Vec<UserReportRow>> {
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
        WHERE r.target_user_id = ?
        ORDER BY CASE r.status WHEN 'open' THEN 0 ELSE 1 END, r.id DESC
        LIMIT ?
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

async fn fetch_suggestions(db: &SqlitePool, status: &str, limit: i64) -> anyhow::Result<Vec<UserSuggestionRow>> {
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
        sqlx::query(&format!(
            "{base} ORDER BY CASE s.status WHEN 'open' THEN 0 ELSE 1 END, s.id DESC LIMIT ?"
        ))
        .bind(limit)
        .fetch_all(db)
        .await?
    } else {
        sqlx::query(&format!(
            "{base} WHERE s.status = ? ORDER BY s.id DESC LIMIT ?"
        ))
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

async fn fetch_test_users(db: &SqlitePool, re: &Regex, limit: i64) -> anyhow::Result<Vec<UserRow>> {
    let rows = sqlx::query(
        r#"SELECT id, username, COALESCE(email,'') AS email, is_banned, created_at
           FROM users
           ORDER BY id DESC
           LIMIT ?"#,
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
                is_banned: r.get::<i64, _>("is_banned") != 0,
                created_at: r.get("created_at"),
                is_online: false,
                presence_status: "offline".to_string(),
                presence_updated_at: String::new(),
                avatar_file_id: None,
                ban_reason: String::new(),
                ban_at: String::new(),
            });
        }
    }
    Ok(out)
}

#[derive(Clone)]
struct ServerRow {
    id: i64,
    name: String,
    owner_id: i64,
    owner_username: String,
    created_at: String,
}

async fn fetch_servers(db: &SqlitePool, q: &str, limit: i64) -> anyhow::Result<Vec<ServerRow>> {
    if q.is_empty() {
        let rows = sqlx::query(
            r#"SELECT s.id, s.name, s.owner_id, COALESCE(u.username,'') AS owner_username, s.created_at
               FROM servers s
               LEFT JOIN users u ON u.id = s.owner_id
               ORDER BY s.id DESC
               LIMIT ?"#,
        )
        .bind(limit)
        .fetch_all(db)
        .await?;

        return Ok(rows
            .into_iter()
            .map(|r| ServerRow {
                id: r.get("id"),
                name: r.get("name"),
                owner_id: r.get("owner_id"),
                owner_username: r.get("owner_username"),
                created_at: r.get("created_at"),
            })
            .collect());
    }

    if let Ok(id) = q.parse::<i64>() {
        let rows = sqlx::query(
            r#"SELECT s.id, s.name, s.owner_id, COALESCE(u.username,'') AS owner_username, s.created_at
               FROM servers s
               LEFT JOIN users u ON u.id = s.owner_id
               WHERE s.id = ?
               LIMIT ?"#,
        )
        .bind(id)
        .bind(limit)
        .fetch_all(db)
        .await?;

        return Ok(rows
            .into_iter()
            .map(|r| ServerRow {
                id: r.get("id"),
                name: r.get("name"),
                owner_id: r.get("owner_id"),
                owner_username: r.get("owner_username"),
                created_at: r.get("created_at"),
            })
            .collect());
    }

    let like = format!("%{}%", q);
    let rows = sqlx::query(
        r#"SELECT s.id, s.name, s.owner_id, COALESCE(u.username,'') AS owner_username, s.created_at
           FROM servers s
           LEFT JOIN users u ON u.id = s.owner_id
           WHERE s.name LIKE ?
           ORDER BY s.id DESC
           LIMIT ?"#,
    )
    .bind(&like)
    .bind(limit)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| ServerRow {
            id: r.get("id"),
            name: r.get("name"),
            owner_id: r.get("owner_id"),
            owner_username: r.get("owner_username"),
            created_at: r.get("created_at"),
        })
        .collect())
}

// =============================
// Destructive ops (copied from CLI)
// =============================

fn admin_thumb_path_for(stored_filename: &str) -> std::path::PathBuf {
    let stem = std::path::Path::new(stored_filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(stored_filename);
    std::path::PathBuf::from("storage/files/thumbs").join(format!("{}.png", stem))
}

async fn cleanup_file_storage_orphans_db(db: &SqlitePool) -> anyhow::Result<()> {
    use std::collections::HashSet;
    use std::path::PathBuf;

    let rows = sqlx::query(
        r#"
        SELECT filename, storage_path
        FROM files
        WHERE deleted_at IS NULL
          AND (expires_at IS NULL OR expires_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
        "#,
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let mut active_paths: HashSet<String> = HashSet::new();
    let mut active_thumbs: HashSet<String> = HashSet::new();

    for r in rows {
        let storage_path: String = r.get("storage_path");
        let filename: String = r.get("filename");
        active_paths.insert(PathBuf::from(storage_path).to_string_lossy().to_string());
        active_thumbs.insert(admin_thumb_path_for(&filename).to_string_lossy().to_string());
    }

    let gif_rows = sqlx::query("SELECT filename, storage_path FROM gif_assets")
        .fetch_all(db)
        .await
        .unwrap_or_default();

    for r in gif_rows {
        let storage_path: String = r.get("storage_path");
        let filename: String = r.get("filename");
        active_paths.insert(PathBuf::from(storage_path).to_string_lossy().to_string());
        active_thumbs.insert(admin_thumb_path_for(&filename).to_string_lossy().to_string());
    }

    let storage_dir = PathBuf::from("storage/files");
    if let Ok(entries) = std::fs::read_dir(&storage_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                continue;
            }
            if path.extension().and_then(|s| s.to_str()) == Some("uploading") {
                continue;
            }
            let key = path.to_string_lossy().to_string();
            if !active_paths.contains(&key) {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    let thumbs_dir = PathBuf::from("storage/files/thumbs");
    if let Ok(entries) = std::fs::read_dir(&thumbs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                continue;
            }
            let key = path.to_string_lossy().to_string();
            if !active_thumbs.contains(&key) {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    Ok(())
}

fn clean_admin_reason(raw: &str) -> String {
    let mut out = raw.trim().chars().take(180).collect::<String>();
    if out.is_empty() {
        out = "Без указанной причины".to_string();
    }
    out
}

async fn insert_moderation_event(db: &SqlitePool, user_id: i64, kind: &str, reason: &str, details: &str) -> anyhow::Result<()> {
    let now = auth::now_iso();
    sqlx::query(
        r#"INSERT INTO moderation_events(user_id, admin_id, kind, reason, details, created_at)
           VALUES(?, NULL, ?, ?, ?, ?)"#,
    )
    .bind(user_id)
    .bind(kind)
    .bind(reason)
    .bind(details)
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

async fn ban_user_exec(db: &SqlitePool, user_id: i64, reason: &str) -> anyhow::Result<()> {
    let clean_reason = clean_admin_reason(reason);
    let affected = sqlx::query("UPDATE users SET is_banned = 1, token_version = token_version + 1 WHERE id = ?")
        .bind(user_id)
        .execute(db)
        .await?
        .rows_affected();
    if affected == 0 {
        anyhow::bail!("Пользователь не найден")
    }
    insert_moderation_event(db, user_id, "ban", &clean_reason, "admin_panel").await?;
    Ok(())
}

async fn unban_user_exec(db: &SqlitePool, user_id: i64) -> anyhow::Result<()> {
    let affected = sqlx::query("UPDATE users SET is_banned = 0, token_version = token_version + 1 WHERE id = ?")
        .bind(user_id)
        .execute(db)
        .await?
        .rows_affected();
    if affected == 0 {
        anyhow::bail!("Пользователь не найден")
    }
    insert_moderation_event(db, user_id, "unban", "Разбан через админ-панель", "admin_panel").await?;
    Ok(())
}

async fn purge_user_content_exec(db: &SqlitePool, user_id: i64) -> anyhow::Result<()> {
    use std::path::PathBuf;

    let mut tx = db.begin().await?;

    let file_rows = sqlx::query("SELECT storage_path, filename FROM files WHERE uploaded_by = ?")
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
    let mut file_paths: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    for fr in file_rows {
        let p: String = fr.get("storage_path");
        let stored_filename: String = fr.get("filename");
        let main = PathBuf::from(p);
        let thumb = PathBuf::from("storage/files/thumbs").join(format!(
            "{}.png",
            std::path::Path::new(&stored_filename)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&stored_filename)
        ));
        file_paths.push((main, Some(thumb)));
    }

    let profile_rows = sqlx::query("SELECT storage_path FROM profile_files WHERE uploaded_by = ?")
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
    let mut profile_paths: Vec<PathBuf> = Vec::new();
    for pr in profile_rows {
        let p: String = pr.get("storage_path");
        profile_paths.push(PathBuf::from(p));
    }

    let _ = sqlx::query(
        r#"DELETE FROM message_reactions
           WHERE message_id IN (SELECT id FROM messages WHERE sender_id = ?)"#,
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;

    let _ = sqlx::query(
        r#"DELETE FROM pinned_messages
           WHERE message_id IN (SELECT id FROM messages WHERE sender_id = ?)"#,
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;

    let _ = sqlx::query("DELETE FROM gif_assets WHERE owner_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query(
        r#"UPDATE gif_assets
           SET source_file_id = NULL
           WHERE source_file_id IN (SELECT id FROM files WHERE uploaded_by = ?)"#,
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;

    let _ = sqlx::query("DELETE FROM files WHERE uploaded_by = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query(
        r#"UPDATE user_reports
           SET message_id = NULL
           WHERE message_id IN (SELECT id FROM messages WHERE sender_id = ?)"#,
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;

    let _ = sqlx::query("DELETE FROM messages WHERE sender_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM profile_files WHERE uploaded_by = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    let _ = file_paths;
    let _ = cleanup_file_storage_orphans_db(db).await;
    for p in profile_paths {
        let _ = std::fs::remove_file(&p);
    }

    Ok(())
}

async fn purge_server_exec(db: &SqlitePool, server_id: i64) -> anyhow::Result<()> {
    use std::path::PathBuf;

    let mut tx = db.begin().await?;

    let file_rows = sqlx::query(
        r#"SELECT f.storage_path, f.filename
           FROM files f
           JOIN chats c ON c.id = f.chat_id
           WHERE c.server_id = ?"#,
    )
    .bind(server_id)
    .fetch_all(&mut *tx)
    .await?;

    let mut file_paths: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    for fr in file_rows {
        let p: String = fr.get("storage_path");
        let stored_filename: String = fr.get("filename");
        let main = PathBuf::from(p);
        let thumb = PathBuf::from("storage/files/thumbs").join(format!(
            "{}.png",
            std::path::Path::new(&stored_filename)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&stored_filename)
        ));
        file_paths.push((main, Some(thumb)));
    }

    let chat_ids = sqlx::query_scalar::<_, i64>("SELECT id FROM chats WHERE server_id = ?")
        .bind(server_id)
        .fetch_all(&mut *tx)
        .await?;

    for chat_id in &chat_ids {
        let _ = sqlx::query(
            r#"DELETE FROM message_reactions
               WHERE message_id IN (SELECT id FROM messages WHERE chat_id = ?)"#,
        )
        .bind(*chat_id)
        .execute(&mut *tx)
        .await?;

        let _ = sqlx::query("DELETE FROM pinned_messages WHERE chat_id = ?")
            .bind(*chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query("DELETE FROM chat_reads WHERE chat_id = ?")
            .bind(*chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query(
            r#"UPDATE gif_assets
               SET source_file_id = NULL
               WHERE source_file_id IN (SELECT id FROM files WHERE chat_id = ?)"#,
        )
        .bind(*chat_id)
        .execute(&mut *tx)
        .await?;

        let _ = sqlx::query("DELETE FROM files WHERE chat_id = ?")
            .bind(*chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query("DELETE FROM messages WHERE chat_id = ?")
            .bind(*chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query("DELETE FROM chat_participants WHERE chat_id = ?")
            .bind(*chat_id)
            .execute(&mut *tx)
            .await?;
    }

    let _ = sqlx::query("DELETE FROM chats WHERE server_id = ?")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM server_members WHERE server_id = ?")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;

    let affected = sqlx::query("DELETE FROM servers WHERE id = ?")
        .bind(server_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();

    if affected == 0 {
        anyhow::bail!("Сервер не найден")
    }

    tx.commit().await?;

    let _ = file_paths;
    let _ = cleanup_file_storage_orphans_db(db).await;

    Ok(())
}

async fn purge_user_exec(db: &SqlitePool, user_id: i64) -> anyhow::Result<()> {
    use std::path::PathBuf;

    // delete owned servers first (needs separate tx because purge_server_exec uses its own tx)
    {
        let mut tx = db.begin().await?;
        let owned_servers = sqlx::query_scalar::<_, i64>("SELECT id FROM servers WHERE owner_id = ?")
            .bind(user_id)
            .fetch_all(&mut *tx)
            .await?;
        tx.commit().await?;

        for sid in owned_servers {
            purge_server_exec(db, sid).await?;
        }
    }

    let mut tx = db.begin().await?;

    let dm_chat_ids = sqlx::query_scalar::<_, i64>(
        "SELECT chat_id FROM dm_chats WHERE user_a = ? OR user_b = ?",
    )
    .bind(user_id)
    .bind(user_id)
    .fetch_all(&mut *tx)
    .await?;

    let dm_file_rows = if dm_chat_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query(
            r#"SELECT f.storage_path, f.filename
               FROM files f
               WHERE f.chat_id IN (SELECT chat_id FROM dm_chats WHERE user_a = ? OR user_b = ?)"#,
        )
        .bind(user_id)
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?
    };

    let mut file_paths: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    for fr in dm_file_rows {
        let p: String = fr.get("storage_path");
        let stored_filename: String = fr.get("filename");
        let main = PathBuf::from(p);
        let thumb = PathBuf::from("storage/files/thumbs").join(format!(
            "{}.png",
            std::path::Path::new(&stored_filename)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&stored_filename)
        ));
        file_paths.push((main, Some(thumb)));
    }

    let user_file_rows = sqlx::query("SELECT storage_path, filename FROM files WHERE uploaded_by = ?")
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
    for fr in user_file_rows {
        let p: String = fr.get("storage_path");
        let stored_filename: String = fr.get("filename");
        let main = PathBuf::from(p);
        let thumb = PathBuf::from("storage/files/thumbs").join(format!(
            "{}.png",
            std::path::Path::new(&stored_filename)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&stored_filename)
        ));
        file_paths.push((main, Some(thumb)));
    }

    let profile_rows = sqlx::query("SELECT storage_path FROM profile_files WHERE uploaded_by = ?")
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
    let mut profile_paths: Vec<PathBuf> = Vec::new();
    for pr in profile_rows {
        let p: String = pr.get("storage_path");
        profile_paths.push(PathBuf::from(p));
    }

    let _ = sqlx::query("DELETE FROM message_reactions WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query(
        r#"DELETE FROM message_reactions
           WHERE message_id IN (SELECT id FROM messages WHERE sender_id = ?)"#,
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;

    let _ = sqlx::query("DELETE FROM pinned_messages WHERE pinned_by = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query(
        r#"DELETE FROM pinned_messages
           WHERE message_id IN (SELECT id FROM messages WHERE sender_id = ?)"#,
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;

    let _ = sqlx::query("DELETE FROM chat_reads WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM chat_participants WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM server_members WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM friendships WHERE user_id = ? OR friend_id = ?")
        .bind(user_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM friend_requests WHERE sender_id = ? OR receiver_id = ?")
        .bind(user_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM user_reports WHERE reporter_id = ? OR target_user_id = ?")
        .bind(user_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM user_suggestions WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM gif_assets WHERE owner_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM user_blocks WHERE blocker_id = ? OR blocked_id = ?")
        .bind(user_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM user_presence WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM user_settings WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM user_profile WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM user_sessions WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM refresh_sessions WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM email_codes WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM profile_files WHERE uploaded_by = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query(
        r#"UPDATE gif_assets
           SET source_file_id = NULL
           WHERE source_file_id IN (SELECT id FROM files WHERE uploaded_by = ?)"#,
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;

    let _ = sqlx::query("DELETE FROM files WHERE uploaded_by = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query(
        r#"UPDATE user_reports
           SET message_id = NULL
           WHERE message_id IN (SELECT id FROM messages WHERE sender_id = ?)"#,
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;

    let _ = sqlx::query("DELETE FROM messages WHERE sender_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    for chat_id in dm_chat_ids {
        let _ = sqlx::query(
            r#"DELETE FROM message_reactions WHERE message_id IN (SELECT id FROM messages WHERE chat_id = ?)"#,
        )
        .bind(chat_id)
        .execute(&mut *tx)
        .await?;

        let _ = sqlx::query("DELETE FROM pinned_messages WHERE chat_id = ?")
            .bind(chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query("DELETE FROM chat_reads WHERE chat_id = ?")
            .bind(chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query(
            r#"UPDATE gif_assets
               SET source_file_id = NULL
               WHERE source_file_id IN (SELECT id FROM files WHERE chat_id = ?)"#,
        )
        .bind(chat_id)
        .execute(&mut *tx)
        .await?;

        let _ = sqlx::query("DELETE FROM files WHERE chat_id = ?")
            .bind(chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query("DELETE FROM messages WHERE chat_id = ?")
            .bind(chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query("DELETE FROM chat_participants WHERE chat_id = ?")
            .bind(chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query("DELETE FROM dm_chats WHERE chat_id = ?")
            .bind(chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query("DELETE FROM chats WHERE id = ?")
            .bind(chat_id)
            .execute(&mut *tx)
            .await?;
    }

    let affected = sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();

    if affected == 0 {
        anyhow::bail!("Пользователь не найден")
    }

    tx.commit().await?;

    let _ = file_paths;
    let _ = cleanup_file_storage_orphans_db(db).await;
    for p in profile_paths {
        let _ = std::fs::remove_file(&p);
    }

    Ok(())
}

// =============================
// DB Tools (global wipe/reset)
// =============================

/// Remove all messages + message-related tables + all uploaded files (and thumbnails).
/// Keeps: users, servers, chats, participants.
async fn wipe_all_messages_exec(db: &SqlitePool) -> anyhow::Result<()> {
    use std::path::{Path, PathBuf};

    let mut tx = db.begin().await?;

    // Collect file paths first (so we can delete from FS after commit)
    let file_rows = sqlx::query("SELECT storage_path, filename FROM files")
        .fetch_all(&mut *tx)
        .await?;

    let mut file_paths: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    for fr in file_rows {
        let p: String = fr.get("storage_path");
        let stored_filename: String = fr.get("filename");

        let main = PathBuf::from(p);
        let stem = Path::new(&stored_filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&stored_filename);
        let thumb = PathBuf::from("storage/files/thumbs").join(format!("{}.png", stem));

        file_paths.push((main, Some(thumb)));
    }

    // Delete in FK-safe order
    sqlx::query("DELETE FROM user_reports")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM message_reactions")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM pinned_messages")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM chat_reads")
        .execute(&mut *tx)
        .await?;

    sqlx::query("UPDATE gif_assets SET source_file_id = NULL")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM files")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE user_reports SET message_id = NULL")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM messages")
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    // Best-effort filesystem cleanup
    let _ = file_paths;
    let _ = cleanup_file_storage_orphans_db(db).await;

    Ok(())
}

/// Remove ALL servers (including Global) with all dependent data (channels/messages/files).
/// Then recreates Global server.
async fn wipe_all_servers_exec(db: &SqlitePool) -> anyhow::Result<()> {
    let server_ids = sqlx::query_scalar::<_, i64>("SELECT id FROM servers ORDER BY id")
        .fetch_all(db)
        .await?;

    for sid in server_ids {
        // purge_server_exec uses its own transaction and removes files from disk.
        let _ = purge_server_exec(db, sid).await;
    }

    // Recreate Global server if missing
    crate::db::bootstrap::ensure_global_server(db).await?;
    Ok(())
}

/// Deletes everything except rows in `users`.
/// Intended for DEV/testing.
/// Also removes stored files/avatars from disk and recreates Global server.
async fn reset_db_keep_users_exec(db: &SqlitePool) -> anyhow::Result<()> {
    use std::path::{Path, PathBuf};

    let mut tx = db.begin().await?;

    // Collect stored file paths (attachments)
    let file_rows = sqlx::query("SELECT storage_path, filename FROM files")
        .fetch_all(&mut *tx)
        .await?;

    let mut file_paths: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    for fr in file_rows {
        let p: String = fr.get("storage_path");
        let stored_filename: String = fr.get("filename");

        let main = PathBuf::from(p);
        let stem = Path::new(&stored_filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&stored_filename);
        let thumb = PathBuf::from("storage/files/thumbs").join(format!("{}.png", stem));

        file_paths.push((main, Some(thumb)));
    }

    // Collect profile file paths (avatars/banners)
    let profile_rows = sqlx::query("SELECT storage_path FROM profile_files")
        .fetch_all(&mut *tx)
        .await?;
    let mut profile_paths: Vec<PathBuf> = Vec::new();
    for pr in profile_rows {
        let p: String = pr.get("storage_path");
        profile_paths.push(PathBuf::from(p));
    }

    // Delete in FK-safe order
    sqlx::query("DELETE FROM user_reports")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM user_suggestions")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM message_reactions")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM pinned_messages")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM chat_reads")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM dm_chats")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM gif_assets")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM files")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE user_reports SET message_id = NULL")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM messages")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM chat_participants")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM chats")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM server_members")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM servers")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM friendships")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM friend_requests")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM user_blocks")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM user_presence")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM user_settings")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM user_sessions")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM refresh_sessions")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM email_codes")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM user_profile")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM profile_files")
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    // Best-effort filesystem cleanup
    let _ = file_paths;
    let _ = cleanup_file_storage_orphans_db(db).await;
    for p in profile_paths {
        let _ = std::fs::remove_file(&p);
    }

    // Recreate Global server if missing
    crate::db::bootstrap::ensure_global_server(db).await?;

    Ok(())
}

/// Shrinks the sqlite DB file by rebuilding it.
/// NOTE: VACUUM may require up to ~2x free disk space while running.
async fn vacuum_exec(db: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query("VACUUM;").execute(db).await?;
    Ok(())
}
