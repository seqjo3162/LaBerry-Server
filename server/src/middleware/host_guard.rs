use axum::{
    extract::Request,
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Response},
    Json,
};
use serde_json::json;
use std::{collections::HashSet, env, net::IpAddr};

pub async fn host_guard(req: Request, next: Next) -> Response {
    if env_bool("LB_DISABLE_HOST_GUARD", false) {
        return next.run(req).await;
    }

    let path = req.uri().path().to_string();
    let Some(host) = request_host(req.headers()) else {
        return blocked_host_response(&path, "missing_host");
    };

    if !host_allowed(&host) {
        return blocked_host_response(&path, "host_not_allowed");
    }

    next.run(req).await
}

fn request_host(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(normalize_host)
}

fn host_allowed(host: &str) -> bool {
    const DEFAULT_ALLOWED_DOMAINS: &[&str] = &["laberry.ru"];

    if DEFAULT_ALLOWED_DOMAINS.contains(&host) {
        return true;
    }

    let allowed = configured_allowed_hosts();
    let is_loopback = is_loopback_host(host);
    let is_ip = host.parse::<IpAddr>().is_ok();
    let allow_localhost = env_bool("LB_ALLOW_LOCALHOST_HOSTS", true);
    let allow_bare_ip = env_bool("LB_ALLOW_BARE_IP_HOSTS", false);

    if !allowed.is_empty() {
        return allowed_host_match(host, &allowed) || (allow_localhost && is_loopback);
    }

    if allow_bare_ip && is_ip {
        return true;
    }

    allow_localhost && is_loopback
}

fn configured_allowed_hosts() -> HashSet<String> {
    let mut values = Vec::new();
    if let Ok(v) = env::var("LB_ALLOWED_HOSTS") {
        values.extend(v.split(',').map(str::to_string));
    }
    if let Ok(v) = env::var("LB_PUBLIC_DOMAIN") {
        values.push(v);
    }

    values
        .into_iter()
        .filter_map(|value| normalize_host(&value))
        .collect()
}

fn allowed_host_match(host: &str, allowed: &HashSet<String>) -> bool {
    if allowed.contains(host) {
        return true;
    }

    allowed.iter().any(|item| {
        let suffix = item.strip_prefix("*.").unwrap_or("");
        !suffix.is_empty()
            && host.len() > suffix.len() + 1
            && host.ends_with(suffix)
            && host.as_bytes().get(host.len() - suffix.len() - 1) == Some(&b'.')
    })
}

fn is_loopback_host(host: &str) -> bool {
    host == "localhost"
        || host
            .parse::<IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

fn normalize_host(raw: &str) -> Option<String> {
    let mut value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }

    if let Some(rest) = value.strip_prefix("http://") {
        value = rest.to_string();
    } else if let Some(rest) = value.strip_prefix("https://") {
        value = rest.to_string();
    }

    if let Some(pos) = value.find('/') {
        value.truncate(pos);
    }
    if let Some(pos) = value.find('@') {
        value = value[pos + 1..].to_string();
    }

    let host = if value.starts_with('[') {
        let end = value.find(']')?;
        value[1..end].to_string()
    } else if value.matches(':').count() == 1 {
        value.split(':').next().unwrap_or("").to_string()
    } else {
        value
    };

    let host = host.trim().trim_end_matches('.').to_string();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

fn blocked_host_response(path: &str, detail: &str) -> Response {
    let status = StatusCode::MISDIRECTED_REQUEST;
    if path.starts_with("/api/") || path == "/ws" {
        return (
            status,
            Json(json!({
                "error": "host_not_allowed",
                "detail": detail,
                "message": "Use the configured domain name instead of a bare IP address."
            })),
        )
            .into_response();
    }

    (
        status,
        Html(
            r#"<!doctype html>
<html lang="ru">
<head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>LaBerry</title></head>
<body style="margin:0;min-height:100vh;display:grid;place-items:center;background:#0b1018;color:#f4f7fb;font:16px system-ui,sans-serif">
  <main style="max-width:520px;padding:28px;border:1px solid #273142;border-radius:16px;background:#111824">
    <h1 style="margin:0 0 10px;font-size:24px">Откройте LaBerry по домену</h1>
    <p style="margin:0;color:#aeb7c7;line-height:1.5">Доступ по прямому IP-адресу отключён. Используйте официальный домен сервиса.</p>
  </main>
</body>
</html>"#,
        ),
    )
        .into_response()
}

fn env_bool(key: &str, default: bool) -> bool {
    env::var(key)
        .ok()
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            value == "1" || value == "true" || value == "yes" || value == "on"
        })
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::normalize_host;

    #[test]
    fn normalizes_hosts() {
        assert_eq!(normalize_host("Example.COM:443").as_deref(), Some("example.com"));
        assert_eq!(normalize_host("https://www.example.com/app").as_deref(), Some("www.example.com"));
        assert_eq!(normalize_host("[::1]:5001").as_deref(), Some("::1"));
    }
}
