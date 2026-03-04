use crate::server::{AdminSession, AppState};

use anyhow::Context;
use axum::{
    extract::{Form, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect},
    routing::{get, post},
    Router,
};
use chrono::{Duration as ChronoDuration, Utc};
use password_hash::{PasswordHash, PasswordVerifier};
use regex::Regex;
use serde::Deserialize;
use sqlx::{Row, SqlitePool};
use std::{env, net::IpAddr};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(root))
        .route("/login", get(login_get).post(login_post))
        .route("/logout", post(logout_post))
        .route("/users", get(users_list))
        .route("/users/:id/ban", post(user_ban))
        .route("/users/:id/ban_forever", post(user_ban_forever))
        .route("/users/:id/purge", post(user_purge_content))
        .route("/test-users", get(test_users_page).post(test_users_delete))
        .route("/servers", get(servers_list))
        .route("/servers/:id/delete", post(server_delete))
        .route("/db", get(db_tools_page))
        .route("/db/wipe_messages", post(db_wipe_messages_post))
        .route("/db/wipe_servers", post(db_wipe_servers_post))
        .route("/db/reset_keep_users", post(db_reset_keep_users_post))
        .route("/db/vacuum", post(db_vacuum_post))
}

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
        anyhow::bail!("Wrong admin password");
    }

    if let Ok(hash) = env::var("LB_ADMIN_PASSWORD_HASH") {
        let parsed = PasswordHash::new(&hash).context("Invalid LB_ADMIN_PASSWORD_HASH")?;
        let argon2 = argon2::Argon2::default();
        argon2
            .verify_password(pw.as_bytes(), &parsed)
            .context("Wrong admin password")?;
        return Ok(());
    }

    anyhow::bail!(
        "Admin password is not configured (set LB_ADMIN_PASSWORD_HASH or LB_ADMIN_PASSWORD)"
    );
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
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
    let p = format!("{}?msg={}", path, url_encode_component(msg));
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

fn page(title: &str, body: &str, msg: Option<&str>) -> Html<String> {
    let msg_html = msg
        .filter(|m| !m.trim().is_empty())
        .map(|m| format!("<div class='msg'>{}</div>", escape_html(m)))
        .unwrap_or_default();

    let html = format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>{title}</title>
<style>
body {{ font-family: system-ui, -apple-system, Segoe UI, Roboto, Arial, sans-serif; background:#0b0d12; color:#e6e6e6; margin:0; }}
header {{ padding:14px 18px; background:#121624; border-bottom:1px solid #202742; display:flex; gap:12px; align-items:center; }}
header a {{ color:#cfe2ff; text-decoration:none; padding:6px 10px; border:1px solid #2a355c; border-radius:10px; }}
header a:hover {{ background:#1b2240; }}
main {{ padding:18px; max-width:1200px; margin:0 auto; }}
.card {{ background:#121624; border:1px solid #202742; border-radius:16px; padding:14px; margin-bottom:14px; }}
input, button {{ font-size:14px; }}
input[type=text], input[type=password] {{ width:100%; max-width:520px; padding:10px 12px; border-radius:12px; border:1px solid #2a355c; background:#0b0d12; color:#e6e6e6; }}
button {{ padding:9px 12px; border-radius:12px; border:1px solid #2a355c; background:#1b2240; color:#e6e6e6; cursor:pointer; }}
button:hover {{ background:#242d57; }}
.table {{ width:100%; border-collapse:collapse; }}
.table th, .table td {{ border-bottom:1px solid #202742; padding:10px 8px; text-align:left; vertical-align:top; }}
.small {{ color:#9aa6c3; font-size:12px; }}
.row-actions form {{ display:inline-block; margin:0 6px 6px 0; }}
.msg {{ background:#1b2240; border:1px solid #2a355c; padding:10px 12px; border-radius:12px; margin-bottom:14px; }}
.warn {{ background:#2b1d1d; border:1px solid #5c2a2a; padding:10px 12px; border-radius:12px; margin-top:10px; }}
</style>
</head>
<body>
<header>
  <div style="font-weight:700;">LaBerry Admin</div>
  <a href="/admin/users">Users</a>
  <a href="/admin/servers">Servers</a>
  <a href="/admin/test-users">Test users</a>
  <a href="/admin/db">DB</a>
  <form method="post" action="/admin/logout" style="margin-left:auto;">
    <button type="submit">Logout</button>
  </form>
</header>
<main>
{msg_html}
{body}
</main>
</body>
</html>"#,
        title = escape_html(title),
        msg_html = msg_html,
        body = body
    );

    Html(html)
}

fn require_admin_panel_enabled() -> Result<(), (StatusCode, String)> {
    if !admin_enabled() {
        return Err((
            StatusCode::NOT_FOUND,
            "Admin panel is disabled (set LB_ENABLE_ADMIN_PANEL=1 or configure LB_ADMIN_PASSWORD[_HASH])".to_string(),
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
        return Err((StatusCode::FORBIDDEN, "IP allowlist enabled, but no remote IP header".to_string()));
    };

    let ip: IpAddr = remote
        .parse()
        .map_err(|_| (StatusCode::FORBIDDEN, "Invalid remote IP".to_string()))?;

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

    Err((StatusCode::FORBIDDEN, "Not allowed from this IP".to_string()))
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
    Redirect::to("/admin/users").into_response()
}

#[derive(Deserialize, Default)]
struct MsgQuery {
    msg: Option<String>,
}

async fn login_get(State(st): State<AppState>, headers: HeaderMap, Query(q): Query<MsgQuery>) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&headers) {
        return e.into_response();
    }
    if session_get(&st, &headers).is_some() {
        return Redirect::to("/admin/users").into_response();
    }

    let warn = if !admin_password_configured() {
        "<div class='warn'>LB_ADMIN_PASSWORD_HASH / LB_ADMIN_PASSWORD is not configured. You will not be able to do destructive ops on non-test users/servers.</div>"
    } else {
        ""
    };

    let body = format!(
        r#"<div class='card'>
<h2>Login</h2>
<form method='post' action='/admin/login'>
  <div class='small'>Admin password</div>
  <input type='password' name='password' autocomplete='current-password' required />
  <div style='height:10px'></div>
  <button type='submit'>Login</button>
</form>
{warn}
</div>"#,
        warn = warn
    );

    page("Admin login", &body, q.msg.as_deref()).into_response()
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
    (h, Redirect::to("/admin/users")).into_response()
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

#[derive(Deserialize, Default)]
struct ListQuery {
    q: Option<String>,
    msg: Option<String>,
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
    let users = fetch_users(&st.db, &query, 200).await;

    let mut rows_html = String::new();
    match users {
        Ok(list) => {
            for u in list {
                let id = u.id;
                let phrase_ban = format!("BAN USER {}", id);
                let phrase_del = format!("DELETE USER {}", id);
                let phrase_purge = format!("PURGE USER CONTENT {}", id);

                rows_html.push_str(&format!(
                    r#"<tr>
<td>#{id}</td>
<td>{username}<div class='small'>{email}</div></td>
<td>{banned}</td>
<td class='small'>{created_at}</td>
<td class='row-actions'>
  <form method='post' action='/admin/users/{id}/ban'>
    <input type='hidden' name='csrf' value='{csrf}' />
    <input type='hidden' name='phrase' value='{phrase_ban}' />
    <input type='text' name='confirm' placeholder='Type: {phrase_ban}' required />
    <input type='password' name='admin_password' placeholder='Admin password (only for non-test)' />
    <button type='submit'>Ban</button>
  </form>
  <form method='post' action='/admin/users/{id}/purge'>
    <input type='hidden' name='csrf' value='{csrf}' />
    <input type='hidden' name='phrase' value='{phrase_purge}' />
    <input type='text' name='confirm' placeholder='Type: {phrase_purge}' required />
    <input type='password' name='admin_password' placeholder='Admin password (only for non-test)' />
    <button type='submit'>Purge content</button>
  </form>
  <form method='post' action='/admin/users/{id}/ban_forever'>
    <input type='hidden' name='csrf' value='{csrf}' />
    <input type='hidden' name='phrase' value='{phrase_del}' />
    <input type='text' name='confirm' placeholder='Type: {phrase_del}' required />
    <input type='password' name='admin_password' placeholder='Admin password (only for non-test)' />
    <button type='submit'>Ban forever (delete)</button>
  </form>
</td>
</tr>"#,
                    id = id,
                    username = escape_html(&u.username),
                    email = escape_html(&u.email),
                    banned = if u.is_banned { "banned" } else { "" },
                    created_at = escape_html(&u.created_at),
                    csrf = escape_html(&sess.csrf),
                    phrase_ban = escape_html(&phrase_ban),
                    phrase_del = escape_html(&phrase_del),
                    phrase_purge = escape_html(&phrase_purge),
                ));
            }
        }
        Err(err) => {
            rows_html.push_str(&format!(
                "<tr><td colspan='5'>DB error: {}</td></tr>",
                escape_html(&format!("{}", err))
            ));
        }
    }

    let body = format!(
        r#"<div class='card'>
<h2>Users</h2>
<form method='get' action='/admin/users'>
  <input type='text' name='q' value='{qval}' placeholder='Search: username / email / id (empty = latest)' />
  <button type='submit'>Search</button>
</form>
<div class='small'>For non-test users, destructive actions require admin password configured via LB_ADMIN_PASSWORD_HASH or LB_ADMIN_PASSWORD.</div>
</div>
<div class='card'>
<table class='table'>
<thead><tr><th>ID</th><th>User</th><th>Status</th><th>Created</th><th>Actions</th></tr></thead>
<tbody>
{rows}
</tbody>
</table>
</div>"#,
        qval = escape_html(&query),
        rows = rows_html
    );

    page("Admin • Users", &body, q.msg.as_deref()).into_response()
}

#[derive(Deserialize)]
struct ActionForm {
    csrf: String,
    phrase: String,
    confirm: String,
    #[serde(default)]
    admin_password: String,
}

async fn user_ban(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    action_user_common(st, headers, id, f, UserAction::Ban).await
}

async fn user_purge_content(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    action_user_common(st, headers, id, f, UserAction::PurgeContent).await
}

async fn user_ban_forever(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    action_user_common(st, headers, id, f, UserAction::BanForever).await
}

enum UserAction {
    Ban,
    PurgeContent,
    BanForever,
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
        return admin_redirect_with_msg("/admin/users", "CSRF token mismatch").into_response();
    }

    if f.confirm.trim() != f.phrase.trim() {
        return admin_redirect_with_msg("/admin/users", "Confirmation phrase mismatch").into_response();
    }

    let user_row = sqlx::query("SELECT username, COALESCE(email,'') AS email FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(&st.db)
        .await;

    let Ok(Some(r)) = user_row else {
        return admin_redirect_with_msg("/admin/users", "User not found").into_response();
    };
    let username: String = r.get("username");
    let email: String = r.get("email");

    let re = test_user_re();
    let is_test = is_test_user(&re, &username, &email);

    if !is_test {
        if !admin_password_configured() {
            return admin_redirect_with_msg("/admin/users", "Admin password not configured; refusing non-test operation").into_response();
        }
        if f.admin_password.trim().is_empty() {
            return admin_redirect_with_msg("/admin/users", "Admin password required for non-test operation").into_response();
        }
        if let Err(e) = verify_admin_password(f.admin_password.trim()) {
            return admin_redirect_with_msg("/admin/users", &format!("{}", e)).into_response();
        }
    }

    let res = match act {
        UserAction::Ban => ban_user_exec(&st.db, user_id).await,
        UserAction::PurgeContent => purge_user_content_exec(&st.db, user_id).await,
        UserAction::BanForever => purge_user_exec(&st.db, user_id).await,
    };

    match res {
        Ok(_) => admin_redirect_with_msg("/admin/users", "OK").into_response(),
        Err(e) => admin_redirect_with_msg("/admin/users", &format!("Error: {}", e)).into_response(),
    }
}

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
                    banned = if u.is_banned { "banned" } else { "" },
                    created_at = escape_html(&u.created_at),
                ));
            }

            let body = format!(
                r#"<div class='card'>
<h2>Test users</h2>
<div class='small'>Regex: <code>{re}</code> (set LB_TEST_USER_REGEX to change)</div>
<form method='post' action='/admin/test-users'>
  <input type='hidden' name='csrf' value='{csrf}' />
  <table class='table'>
  <thead><tr><th></th><th>ID</th><th>User</th><th>Status</th><th>Created</th></tr></thead>
  <tbody>
  {rows}
  </tbody>
  </table>
  <div style='height:10px'></div>
  <input type='text' name='confirm' placeholder='Type: DELETE N TEST USERS' required />
  <button type='submit'>Delete selected</button>
</form>
</div>"#,
                re = escape_html(re.as_str()),
                csrf = escape_html(&sess.csrf),
                rows = rows
            );

            return page("Admin • Test users", &body, q.msg.as_deref()).into_response();
        }
        Err(e) => {
            let body = format!(
                "<div class='card'>DB error: {}</div>",
                escape_html(&format!("{}", e))
            );
            return page("Admin • Test users", &body, q.msg.as_deref()).into_response();
        }
    }
}

#[derive(Deserialize)]
struct DeleteTestUsersForm {
    csrf: String,
    #[serde(default)]
    user_ids: Vec<i64>,
    confirm: String,
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
        return admin_redirect_with_msg("/admin/test-users", "CSRF token mismatch").into_response();
    }

    if f.user_ids.is_empty() {
        return admin_redirect_with_msg("/admin/test-users", "Nothing selected").into_response();
    }

    let phrase = format!("DELETE {} TEST USERS", f.user_ids.len());
    if f.confirm.trim() != phrase {
        return admin_redirect_with_msg("/admin/test-users", &format!("Type exactly: {}", phrase)).into_response();
    }

    let re = test_user_re();
    for id in &f.user_ids {
        let row = sqlx::query("SELECT username, COALESCE(email,'') AS email FROM users WHERE id = ?")
            .bind(*id)
            .fetch_optional(&st.db)
            .await;
        let Ok(Some(r)) = row else {
            return admin_redirect_with_msg("/admin/test-users", "User not found").into_response();
        };
        let username: String = r.get("username");
        let email: String = r.get("email");
        if !is_test_user(&re, &username, &email) {
            return admin_redirect_with_msg("/admin/test-users", "Refusing: selection contains non-test user").into_response();
        }
    }

    for id in &f.user_ids {
        if let Err(e) = purge_user_exec(&st.db, *id).await {
            return admin_redirect_with_msg("/admin/test-users", &format!("Error: {}", e)).into_response();
        }
    }

    admin_redirect_with_msg("/admin/test-users", "OK").into_response()
}

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
            for s in list {
                let id = s.id;
                let phrase = format!("DELETE SERVER {}", id);

                rows_html.push_str(&format!(
                    r#"<tr>
<td>#{id}</td>
<td>{name}<div class='small'>owner: #{owner_id} {owner_name}</div></td>
<td class='small'>{created_at}</td>
<td>
  <form method='post' action='/admin/servers/{id}/delete'>
    <input type='hidden' name='csrf' value='{csrf}' />
    <input type='hidden' name='phrase' value='{phrase}' />
    <input type='text' name='confirm' placeholder='Type: {phrase}' required />
    <input type='password' name='admin_password' placeholder='Admin password (only for non-test)' />
    <button type='submit'>Delete server</button>
  </form>
</td>
</tr>"#,
                    id = id,
                    name = escape_html(&s.name),
                    owner_id = s.owner_id,
                    owner_name = escape_html(&s.owner_username),
                    created_at = escape_html(&s.created_at),
                    csrf = escape_html(&sess.csrf),
                    phrase = escape_html(&phrase),
                ));
            }
        }
        Err(err) => {
            rows_html.push_str(&format!(
                "<tr><td colspan='4'>DB error: {}</td></tr>",
                escape_html(&format!("{}", err))
            ));
        }
    }

    let body = format!(
        r#"<div class='card'>
<h2>Servers</h2>
<form method='get' action='/admin/servers'>
  <input type='text' name='q' value='{qval}' placeholder='Search: server name / id (empty = latest)' />
  <button type='submit'>Search</button>
</form>
</div>
<div class='card'>
<table class='table'>
<thead><tr><th>ID</th><th>Server</th><th>Created</th><th>Actions</th></tr></thead>
<tbody>
{rows}
</tbody>
</table>
</div>"#,
        qval = escape_html(&query),
        rows = rows_html
    );

    page("Admin • Servers", &body, q.msg.as_deref()).into_response()
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
        return admin_redirect_with_msg("/admin/servers", "CSRF token mismatch").into_response();
    }

    if f.confirm.trim() != f.phrase.trim() {
        return admin_redirect_with_msg("/admin/servers", "Confirmation phrase mismatch").into_response();
    }

    let row = sqlx::query("SELECT name FROM servers WHERE id = ?")
        .bind(id)
        .fetch_optional(&st.db)
        .await;

    let Ok(Some(r)) = row else {
        return admin_redirect_with_msg("/admin/servers", "Server not found").into_response();
    };
    let name: String = r.get("name");

    let re = test_server_re();
    let is_test = is_test_server(&re, &name);

    if !is_test {
        if !admin_password_configured() {
            return admin_redirect_with_msg("/admin/servers", "Admin password not configured; refusing non-test operation").into_response();
        }
        if f.admin_password.trim().is_empty() {
            return admin_redirect_with_msg("/admin/servers", "Admin password required for non-test operation").into_response();
        }
        if let Err(e) = verify_admin_password(f.admin_password.trim()) {
            return admin_redirect_with_msg("/admin/servers", &format!("{}", e)).into_response();
        }
    }

    match purge_server_exec(&st.db, id).await {
        Ok(_) => admin_redirect_with_msg("/admin/servers", "OK").into_response(),
        Err(e) => admin_redirect_with_msg("/admin/servers", &format!("Error: {}", e)).into_response(),
    }
}

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

    let phrase_wipe_messages = "WIPE ALL MESSAGES".to_string();
    let phrase_wipe_servers = "WIPE ALL SERVERS".to_string();
    let phrase_reset_keep_users = "RESET DB KEEP USERS".to_string();
    let phrase_vacuum = "VACUUM DB".to_string();

    let body = format!(
        r#"
<div class='card'>
<h2>DB tools</h2>
<div class='small'>
These actions are destructive. They require admin password and a confirmation phrase.
</div>
</div>

<div class='card'>
<h3>Wipe: messages + attachments (keep users/servers/channels)</h3>
<form method='post' action='/admin/db/wipe_messages'>
  <input type='hidden' name='csrf' value='{csrf}' />
  <input type='hidden' name='phrase' value='{phrase}' />
  <input type='text' name='confirm' placeholder='Type: {phrase}' required />
  <input type='password' name='admin_password' placeholder='Admin password' required />
  <button type='submit'>Wipe messages</button>
</form>
<div class='small'>Deletes: messages, reactions, pins, chat_reads, files table rows + stored files (and thumbnails).</div>
</div>

<div class='card'>
<h3>Wipe: servers + channels + everything inside (keep users + DMs)</h3>
<form method='post' action='/admin/db/wipe_servers'>
  <input type='hidden' name='csrf' value='{csrf}' />
  <input type='hidden' name='phrase' value='{phrase2}' />
  <input type='text' name='confirm' placeholder='Type: {phrase2}' required />
  <input type='password' name='admin_password' placeholder='Admin password' required />
  <button type='submit'>Wipe servers</button>
</form>
<div class='small'>Deletes all servers and their channels/messages/files. DMs are not touched.</div>
</div>

<div class='card'>
<h3>Reset: delete everything except users</h3>
<form method='post' action='/admin/db/reset_keep_users'>
  <input type='hidden' name='csrf' value='{csrf}' />
  <input type='hidden' name='phrase' value='{phrase3}' />
  <input type='text' name='confirm' placeholder='Type: {phrase3}' required />
  <input type='password' name='admin_password' placeholder='Admin password' required />
  <button type='submit'>Reset (keep users)</button>
</form>
<div class='small'>Deletes everything except rows in <code>users</code>. Profile/settings/sessions/friends/servers/dms/messages/files are removed. Global server will be recreated automatically.</div>
</div>

<div class='card'>
<h3>Maintenance: VACUUM</h3>
<form method='post' action='/admin/db/vacuum'>
  <input type='hidden' name='csrf' value='{csrf}' />
  <input type='hidden' name='phrase' value='{phrase4}' />
  <input type='text' name='confirm' placeholder='Type: {phrase4}' required />
  <input type='password' name='admin_password' placeholder='Admin password' required />
  <button type='submit'>VACUUM</button>
</form>
<div class='small'>VACUUM rebuilds the database file and can require up to ~2x free disk space while running.</div>
</div>
"#,
        csrf = escape_html(&sess.csrf),
        phrase = escape_html(&phrase_wipe_messages),
        phrase2 = escape_html(&phrase_wipe_servers),
        phrase3 = escape_html(&phrase_reset_keep_users),
        phrase4 = escape_html(&phrase_vacuum),
    );

    page("Admin • DB tools", &body, q.msg.as_deref()).into_response()
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
        return admin_redirect_with_msg("/admin/db", "CSRF token mismatch").into_response();
    }
    if f.confirm.trim() != f.phrase.trim() {
        return admin_redirect_with_msg("/admin/db", "Confirmation phrase mismatch").into_response();
    }

    if !admin_password_configured() {
        return admin_redirect_with_msg("/admin/db", "Admin password is not configured (LB_ADMIN_PASSWORD[_HASH])").into_response();
    }
    if f.admin_password.trim().is_empty() {
        return admin_redirect_with_msg("/admin/db", "Admin password required").into_response();
    }
    if let Err(e) = verify_admin_password(f.admin_password.trim()) {
        return admin_redirect_with_msg("/admin/db", &format!("{}", e)).into_response();
    }

    let res = match act {
        DbAction::WipeMessages => wipe_all_messages_exec(&st.db).await,
        DbAction::WipeServers => wipe_all_servers_exec(&st.db).await,
        DbAction::ResetKeepUsers => reset_db_keep_users_exec(&st.db).await,
        DbAction::Vacuum => vacuum_exec(&st.db).await,
    };

    match res {
        Ok(_) => admin_redirect_with_msg("/admin/db", "OK").into_response(),
        Err(e) => admin_redirect_with_msg("/admin/db", &format!("Error: {}", e)).into_response(),
    }
}

#[derive(Clone)]
struct UserRow {
    id: i64,
    username: String,
    email: String,
    is_banned: bool,
    created_at: String,
}

async fn fetch_users(db: &SqlitePool, q: &str, limit: i64) -> anyhow::Result<Vec<UserRow>> {
    if q.is_empty() {
        let rows = sqlx::query(
            r#"SELECT id, username, COALESCE(email,'') AS email, is_banned, created_at
               FROM users
               ORDER BY id DESC
               LIMIT ?"#,
        )
        .bind(limit)
        .fetch_all(db)
        .await?;
        return Ok(rows
            .into_iter()
            .map(|r| UserRow {
                id: r.get("id"),
                username: r.get("username"),
                email: r.get("email"),
                is_banned: r.get::<i64, _>("is_banned") != 0,
                created_at: r.get("created_at"),
            })
            .collect());
    }

    if let Ok(id) = q.parse::<i64>() {
        let rows = sqlx::query(
            r#"SELECT id, username, COALESCE(email,'') AS email, is_banned, created_at
               FROM users
               WHERE id = ?
               LIMIT ?"#,
        )
        .bind(id)
        .bind(limit)
        .fetch_all(db)
        .await?;
        return Ok(rows
            .into_iter()
            .map(|r| UserRow {
                id: r.get("id"),
                username: r.get("username"),
                email: r.get("email"),
                is_banned: r.get::<i64, _>("is_banned") != 0,
                created_at: r.get("created_at"),
            })
            .collect());
    }

    let like = format!("%{}%", q);
    let rows = sqlx::query(
        r#"SELECT id, username, COALESCE(email,'') AS email, is_banned, created_at
           FROM users
           WHERE username LIKE ? OR email LIKE ?
           ORDER BY id DESC
           LIMIT ?"#,
    )
    .bind(&like)
    .bind(&like)
    .bind(limit)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| UserRow {
            id: r.get("id"),
            username: r.get("username"),
            email: r.get("email"),
            is_banned: r.get::<i64, _>("is_banned") != 0,
            created_at: r.get("created_at"),
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

async fn ban_user_exec(db: &SqlitePool, user_id: i64) -> anyhow::Result<()> {
    let affected = sqlx::query("UPDATE users SET is_banned = 1, token_version = token_version + 1 WHERE id = ?")
        .bind(user_id)
        .execute(db)
        .await?
        .rows_affected();
    if affected == 0 {
        anyhow::bail!("User not found")
    }
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

    let _ = sqlx::query("DELETE FROM files WHERE uploaded_by = ?")
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

    for (main, thumb) in file_paths {
        let _ = std::fs::remove_file(&main);
        if let Some(t) = thumb {
            let _ = std::fs::remove_file(&t);
        }
    }
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
        anyhow::bail!("Server not found")
    }

    tx.commit().await?;

    for (main, thumb) in file_paths {
        let _ = std::fs::remove_file(&main);
        if let Some(t) = thumb {
            let _ = std::fs::remove_file(&t);
        }
    }

    Ok(())
}

async fn purge_user_exec(db: &SqlitePool, user_id: i64) -> anyhow::Result<()> {
    use std::path::PathBuf;
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

    let _ = sqlx::query("DELETE FROM files WHERE uploaded_by = ?")
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
        anyhow::bail!("User not found")
    }

    tx.commit().await?;

    for (main, thumb) in file_paths {
        let _ = std::fs::remove_file(&main);
        if let Some(t) = thumb {
            let _ = std::fs::remove_file(&t);
        }
    }
    for p in profile_paths {
        let _ = std::fs::remove_file(&p);
    }

    Ok(())
}

async fn wipe_all_messages_exec(db: &SqlitePool) -> anyhow::Result<()> {
    use std::path::{Path, PathBuf};

    let mut tx = db.begin().await?;
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

    sqlx::query("DELETE FROM message_reactions")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM pinned_messages")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM chat_reads")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM files")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM messages")
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    for (main, thumb) in file_paths {
        let _ = std::fs::remove_file(&main);
        if let Some(t) = thumb {
            let _ = std::fs::remove_file(&t);
        }
    }

    Ok(())
}

async fn wipe_all_servers_exec(db: &SqlitePool) -> anyhow::Result<()> {
    let server_ids = sqlx::query_scalar::<_, i64>("SELECT id FROM servers ORDER BY id")
        .fetch_all(db)
        .await?;

    for sid in server_ids {
        let _ = purge_server_exec(db, sid).await;
    }
    crate::db::bootstrap::ensure_global_server(db).await?;
    Ok(())
}

async fn reset_db_keep_users_exec(db: &SqlitePool) -> anyhow::Result<()> {
    use std::path::{Path, PathBuf};

    let mut tx = db.begin().await?;
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

    let profile_rows = sqlx::query("SELECT storage_path FROM profile_files")
        .fetch_all(&mut *tx)
        .await?;
    let mut profile_paths: Vec<PathBuf> = Vec::new();
    for pr in profile_rows {
        let p: String = pr.get("storage_path");
        profile_paths.push(PathBuf::from(p));
    }

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

    sqlx::query("DELETE FROM files")
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

    for (main, thumb) in file_paths {
        let _ = std::fs::remove_file(&main);
        if let Some(t) = thumb {
            let _ = std::fs::remove_file(&t);
        }
    }
    for p in profile_paths {
        let _ = std::fs::remove_file(&p);
    }

    crate::db::bootstrap::ensure_global_server(db).await?;

    Ok(())
}

async fn vacuum_exec(db: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query("VACUUM;").execute(db).await?;
    Ok(())
}