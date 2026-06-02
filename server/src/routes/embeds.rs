use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Router,
};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    net::IpAddr,
    time::Duration,
};
use tokio::net::lookup_host;
use url::Url;

use crate::middleware::auth_guard::AuthUser;
use crate::middleware::rate_limit;
use crate::server::AppState;

const MAX_HTML_BYTES: usize = 512 * 1024; // 512KB
const HTTP_TIMEOUT_SECS: u64 = 6;

pub fn router() -> Router<AppState> {
    Router::new().route("/", get(get_preview))
}

#[derive(Deserialize)]
struct PreviewQuery {
    url: String,
}

#[derive(Serialize)]
struct PreviewResponse {
    url: String,
    title: String,
    description: String,
    image: String,
    site_name: String,
}

static RE_META_TAG: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<meta\s+[^>]*>").expect("meta regex"));

static RE_ATTR: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?is)\b([a-zA-Z_:][a-zA-Z0-9_:.\-]*)\s*=\s*("([^"]*)"|'([^']*)')"#)
        .expect("attr regex")
});

static RE_TITLE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<title[^>]*>(.*?)</title>").expect("title regex"));

static RE_WHITESPACE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\s+").expect("ws regex"));

fn is_disallowed_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
        }
    }
}

async fn resolve_and_validate_host(host: &str, port: u16) -> Result<(), StatusCode> {
    let host_lc = host.to_ascii_lowercase();

    if host_lc == "localhost" || host_lc.ends_with(".local") {
        return Err(StatusCode::BAD_REQUEST);
    }

    // If host is already an IP literal, validate it directly
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_disallowed_ip(ip) {
            return Err(StatusCode::BAD_REQUEST);
        }
        return Ok(());
    }

    let addrs = lookup_host((host, port)).await.map_err(|_| StatusCode::BAD_REQUEST)?;
    for addr in addrs {
        if is_disallowed_ip(addr.ip()) {
            return Err(StatusCode::BAD_REQUEST);
        }
    }

    Ok(())
}

fn attr_value(tag: &str, key: &str) -> Option<String> {
    let key_lc = key.to_ascii_lowercase();
    for caps in RE_ATTR.captures_iter(tag) {
        let k = caps.get(1).map(|m| m.as_str()).unwrap_or("").to_ascii_lowercase();
        if k != key_lc {
            continue;
        }

        let v = caps.get(3).map(|m| m.as_str()).or_else(|| caps.get(4).map(|m| m.as_str())).unwrap_or("");
        if v.is_empty() {
            continue;
        }
        return Some(v.to_string());
    }
    None
}

fn extract_meta(html: &str, wanted: &str) -> Option<String> {
    let wanted_lc = wanted.to_ascii_lowercase();

    for m in RE_META_TAG.find_iter(html) {
        let tag = m.as_str();

        let prop = attr_value(tag, "property").unwrap_or_default().to_ascii_lowercase();
        let name = attr_value(tag, "name").unwrap_or_default().to_ascii_lowercase();

        if prop != wanted_lc && name != wanted_lc {
            continue;
        }

        if let Some(content) = attr_value(tag, "content") {
            let cleaned = RE_WHITESPACE.replace_all(content.trim(), " ");
            let cleaned = cleaned.trim().to_string();
            if !cleaned.is_empty() {
                return Some(cleaned);
            }
        }
    }

    None
}

fn extract_title(html: &str) -> Option<String> {
    if let Some(v) = extract_meta(html, "og:title") {
        return Some(v);
    }
    if let Some(v) = extract_meta(html, "twitter:title") {
        return Some(v);
    }
    if let Some(caps) = RE_TITLE.captures(html) {
        let raw = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let cleaned = RE_WHITESPACE.replace_all(raw.trim(), " ");
        let cleaned = cleaned.trim().to_string();
        if !cleaned.is_empty() {
            return Some(cleaned);
        }
    }
    None
}

fn extract_description(html: &str) -> Option<String> {
    if let Some(v) = extract_meta(html, "og:description") {
        return Some(v);
    }
    if let Some(v) = extract_meta(html, "description") {
        return Some(v);
    }
    if let Some(v) = extract_meta(html, "twitter:description") {
        return Some(v);
    }
    None
}

fn extract_image(html: &str) -> Option<String> {
    if let Some(v) = extract_meta(html, "og:image") {
        return Some(v);
    }
    if let Some(v) = extract_meta(html, "twitter:image") {
        return Some(v);
    }
    None
}

fn extract_site_name(html: &str, fallback_host: &str) -> String {
    extract_meta(html, "og:site_name").unwrap_or_else(|| fallback_host.to_string())
}

async fn get_preview(
    State(_st): State<AppState>,
    me: AuthUser,
    Query(q): Query<PreviewQuery>,
) -> impl IntoResponse {
    // Rate-limit: max 20 preview requests per user per minute to prevent abuse.
    let rl_key = format!("embed_preview:{}", me.id);
    if !rate_limit::allow(&rl_key, 20, 60) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }

    let url_str = q.url.clone();
    let Ok(parsed) = Url::parse(&q.url) else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let Some(host) = parsed.host_str() else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    let port = parsed.port_or_known_default().unwrap_or(if scheme == "https" { 443 } else { 80 });
    if resolve_and_validate_host(host, port).await.is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        // SECURITY: Redirects are disabled. A validated public URL could redirect
        // to a private/internal address, bypassing the SSRF check above.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("LaBerry/1.0 (+https://laberry.ru)")
        .build()
    {
        Ok(c) => c,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let resp = match client
        .get(parsed.as_str())
        .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml")
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => {
            let body = axum::Json(PreviewResponse {
                url: url_str.clone(),
                title: host.to_string(),
                description: "".to_string(),
                image: "".to_string(),
                site_name: host.to_string(),
            });
            return (StatusCode::OK, body).into_response();
        }
    };

    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    if !(ct.starts_with("text/html") || ct.starts_with("application/xhtml+xml") || ct.is_empty()) {
        // not HTML -> no preview
        let body = axum::Json(PreviewResponse {
            url: url_str.clone(),
            title: host.to_string(),
            description: "".to_string(),
            image: "".to_string(),
            site_name: host.to_string(),
        });
        return (StatusCode::OK, body).into_response();
    }

    if let Some(len) = resp.content_length() {
        if len as usize > MAX_HTML_BYTES {
            let body = axum::Json(PreviewResponse {
                url: url_str.clone(),
                title: host.to_string(),
                description: "".to_string(),
                image: "".to_string(),
                site_name: host.to_string(),
            });
            return (StatusCode::OK, body).into_response();
        }
    }

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(_) => {
            let body = axum::Json(PreviewResponse {
                url: url_str.clone(),
                title: host.to_string(),
                description: "".to_string(),
                image: "".to_string(),
                site_name: host.to_string(),
            });
            return (StatusCode::OK, body).into_response();
        }
    };

    let bytes = if bytes.len() > MAX_HTML_BYTES {
        &bytes[..MAX_HTML_BYTES]
    } else {
        &bytes[..]
    };

    let html = String::from_utf8_lossy(bytes);

    let title = extract_title(&html).unwrap_or_else(|| host.to_string());
    let description = extract_description(&html).unwrap_or_default();
    let image = extract_image(&html).unwrap_or_default();
    let site_name = extract_site_name(&html, host);

    let body = axum::Json(PreviewResponse {
        url: url_str.clone(),
        title,
        description,
        image,
        site_name,
    });

    (StatusCode::OK, body).into_response()
}
