use std::path::PathBuf;

use axum::{
    http::{self, HeaderMap, Uri},
    response::{Html, IntoResponse, Redirect, Response},
};

/// Простой SPA-роутер — возвращает index.html для "/" и "/app"
pub async fn index() -> Html<String> {
    Html(std::fs::read_to_string(static_path("start.html")).unwrap_or_default())
}

pub async fn login() -> Html<String> {
    Html(std::fs::read_to_string(static_path("index.html")).unwrap_or_default())
}

pub async fn app() -> Html<String> {
    Html(std::fs::read_to_string(static_path("app.html")).unwrap_or_default())
}

pub async fn start() -> Html<String> {
    Html(std::fs::read_to_string(static_path("start.html")).unwrap_or_default())
}

pub async fn cookie_agreement() -> Html<String> {
    Html(std::fs::read_to_string(static_path("cookie-agreement.html")).unwrap_or_default())
}

pub async fn license_agreement() -> Html<String> {
    Html(std::fs::read_to_string(static_path("license-agreement.html")).unwrap_or_default())
}


pub fn admin_panel_base_url() -> String {
    let host = std::env::var("LB_ADMIN_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("LB_ADMIN_PORT").unwrap_or_else(|_| "5002".to_string());
    format!("http://{}:{}", host, port)
}

fn request_scheme(headers: &HeaderMap) -> &str {
    if let Some(proto) = headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()) {
        if proto.eq_ignore_ascii_case("https") {
            return "https";
        }
    }
    "http"
}

fn normalize_host_header(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            return &host[..=end];
        }
    }
    if let Some((prefix, suffix)) = host.rsplit_once(':') {
        if suffix.parse::<u16>().is_ok() {
            return prefix;
        }
    }
    host
}

pub fn admin_panel_url(path: &str) -> String {
    let path = path.trim();
    let path = if path.is_empty() || path == "/" {
        "/admin/".to_string()
    } else if path.starts_with("/admin/") {
        path.to_string()
    } else if path.starts_with("/admin") {
        path.to_string()
    } else if path.starts_with('/') {
        format!("/admin{}", path)
    } else {
        format!("/admin/{}", path)
    };
    format!("{}{}", admin_panel_base_url(), path)
}

pub fn admin_panel_url_for_request(headers: &HeaderMap, path: &str) -> String {
    let host_header = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let path = path.trim();
    let path = if path.is_empty() || path == "/" {
        "/admin/".to_string()
    } else if path.starts_with("/admin/") {
        path.to_string()
    } else if path.starts_with("/admin") {
        path.to_string()
    } else if path.starts_with('/') {
        format!("/admin{}", path)
    } else {
        format!("/admin/{}", path)
    };

    if let Some(host) = host_header {
        let host = normalize_host_header(host);
        let port = std::env::var("LB_ADMIN_PORT").unwrap_or_else(|_| "5002".to_string());
        let scheme = request_scheme(headers);
        return format!("{}://{}:{}{}", scheme, host, port, path);
    }

    admin_panel_url(path)
}

/// Redirect any /admin/* path on the main listener to the admin port.
pub async fn admin_redirect_fallback(
    headers: HeaderMap,
    uri: Uri,
    method: axum::http::Method,
) -> Response {
    let target = admin_panel_url_for_request(&headers, uri.path());
    if method == axum::http::Method::GET || method == axum::http::Method::HEAD {
        return Redirect::temporary(&target).into_response();
    }
    Response::builder()
        .status(axum::http::StatusCode::TEMPORARY_REDIRECT)
        .header(axum::http::header::LOCATION, target)
        .body(axum::body::Body::empty())
        .expect("admin redirect response")
}

pub async fn admin_hint(headers: HeaderMap) -> Html<String> {
    let url = admin_panel_url_for_request(&headers, "/admin/");
    let login_url = admin_panel_url_for_request(&headers, "/admin/login");

    Html(format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>Admin</title>
<style>
body {{ font-family: system-ui, -apple-system, Segoe UI, Roboto, Arial, sans-serif; background:#0b0d12; color:#e6e6e6; margin:0; }}
main {{ padding:18px; max-width:900px; margin:0 auto; }}
.card {{ background:#121624; border:1px solid #202742; border-radius:16px; padding:14px; margin-top:14px; }}
a {{ color:#cfe2ff; }}
code {{ background:#0f1220; padding:2px 6px; border-radius:8px; border:1px solid #202742; }}
</style>
</head>
<body>
<main>
  <div class="card">
    <h2>Admin panel</h2>
    <div>Admin panel is served by a dedicated admin listener (local-only by default).</div>
    <div style="height:10px"></div>
    <div>Main app is on port <code>LB_PORT</code> (default 5001). The admin UI is <strong>not</strong> on that port.</div>
    <div style="height:10px"></div>
    <div>Open admin panel: <a href="{login_url}">{login_url}</a></div>
    <div style="height:8px"></div>
    <div class="small">Home: <a href="{url}">{url}</a></div>
    <div style="height:10px"></div>
    <div class="small">If you use a reverse proxy, forward <code>/admin</code> to the admin listener (port {port}).</div>
  </div>
</main>
</body>
</html>"#,
        url = url,
        login_url = login_url,
        port = std::env::var("LB_ADMIN_PORT").unwrap_or_else(|_| "5002".to_string())
    ))
}

/// Путь к статическим файлам
fn static_path(file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("static")
        .join(file)
}
