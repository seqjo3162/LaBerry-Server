use axum::{
    extract::Request,
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Response},
    Json,
};
use serde_json::json;
use std::{
    collections::HashSet,
    env,
    fs::{self, OpenOptions},
    io::Write,
    net::IpAddr,
    path::PathBuf,
};

const DEFAULT_BLOCKED_COUNTRIES: &str = "SK";

pub async fn geo_guard(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let headers = req.headers().clone();

    if env_bool("LB_DISABLE_GEO_GUARD", false) {
        return next.run(req).await;
    }

    let country = country_from_headers(&headers);
    let ip = client_ip_from_headers(&headers);
    let vpn_or_proxy = vpn_or_proxy_hint(&headers);
    let blocked_countries = blocked_country_codes();
    let country_blocked = country
        .as_deref()
        .map(|code| blocked_countries.contains(code))
        .unwrap_or(false);
    let ip_blocked = ip
        .as_deref()
        .map(|value| blocked_ip_exists(value))
        .unwrap_or(false);

    if country_blocked || ip_blocked {
        if country_blocked || vpn_or_proxy {
            if let Some(value) = ip.as_deref() {
                auto_ban_public_ip(value, country.as_deref(), vpn_or_proxy);
            }
        }

        return blocked_response(&path);
    }

    next.run(req).await
}

fn blocked_response(path: &str) -> Response {
    let status = StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS;
    let mut response = if path.starts_with("/api/") || path == "/ws" {
        (
            status,
            Json(json!({
                "error": "location_not_supported",
                "message": "Доступ из этой локации сейчас не поддерживается. Если включен VPN или прокси, отключите его или выберите поддерживаемый регион."
            })),
        )
            .into_response()
    } else {
        (
            status,
            Html(
                r#"<!doctype html>
<html lang="ru">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>LaBerry - доступ ограничен</title>
  <style>
    :root { color-scheme: dark; }
    body {
      margin: 0;
      min-height: 100vh;
      display: grid;
      place-items: center;
      font-family: Inter, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      background: radial-gradient(circle at 30% 20%, rgba(186, 73, 255, .2), transparent 36%), #080b12;
      color: #f3f0ff;
    }
    main {
      width: min(560px, calc(100vw - 32px));
      padding: 28px;
      border: 1px solid rgba(255, 255, 255, .12);
      border-radius: 18px;
      background: rgba(18, 22, 33, .92);
      box-shadow: 0 24px 80px rgba(0, 0, 0, .42);
    }
    h1 { margin: 0 0 12px; font-size: 28px; }
    p { margin: 0; color: #c7bed8; line-height: 1.6; }
    .hint {
      margin-top: 18px;
      padding: 14px 16px;
      border: 1px solid rgba(255, 190, 88, .28);
      border-radius: 14px;
      background: rgba(255, 190, 88, .08);
      color: #ffe3b0;
    }
  </style>
</head>
<body>
  <main>
    <h1>Доступ временно ограничен</h1>
    <p>LaBerry сейчас не поддерживает подключение из этой локации.</p>
    <p class="hint">Если включен VPN или прокси, отключите его или выберите поддерживаемый регион. Повторные подключения с неподдерживаемой локации могут быть автоматически ограничены.</p>
  </main>
</body>
</html>"#,
            ),
        )
            .into_response()
    };

    response.headers_mut().insert(
        "x-laberry-access-guard",
        HeaderValue::from_static("geo-location"),
    );
    response
}

fn blocked_country_codes() -> HashSet<String> {
    let raw = env::var("LB_BLOCKED_COUNTRIES")
        .unwrap_or_else(|_| DEFAULT_BLOCKED_COUNTRIES.to_string());

    raw.split(',')
        .filter_map(normalize_country_code)
        .collect::<HashSet<_>>()
}

fn country_from_headers(headers: &HeaderMap) -> Option<String> {
    [
        "cf-ipcountry",
        "x-geo-country",
        "x-country-code",
        "cloudfront-viewer-country",
        "x-vercel-ip-country",
    ]
    .iter()
    .find_map(|name| {
        headers
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .and_then(normalize_country_code)
    })
}

fn normalize_country_code(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let upper = value.to_ascii_uppercase();
    match upper.as_str() {
        "SLOVAKIA" | "SLOVAK REPUBLIC" => Some("SK".to_string()),
        _ if upper.len() == 2 && upper.chars().all(|ch| ch.is_ascii_alphabetic()) => Some(upper),
        _ => None,
    }
}

fn client_ip_from_headers(headers: &HeaderMap) -> Option<String> {
    [
        "cf-connecting-ip",
        "x-real-ip",
        "x-forwarded-for",
        "x-client-ip",
        "fastly-client-ip",
    ]
    .iter()
    .find_map(|name| {
        headers
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .and_then(clean_ip)
    })
}

fn clean_ip(value: &str) -> Option<String> {
    let mut value = value.split(',').next()?.trim();
    if value.is_empty() {
        return None;
    }

    if let Some(stripped) = value.strip_prefix('[').and_then(|s| s.split(']').next()) {
        value = stripped;
    } else if value.matches(':').count() == 1 && value.contains('.') {
        value = value.split(':').next().unwrap_or(value).trim();
    }

    value.parse::<IpAddr>().ok().map(|_| value.to_string())
}

fn vpn_or_proxy_hint(headers: &HeaderMap) -> bool {
    [
        "x-vpn-detected",
        "x-proxy-detected",
        "x-ip-vpn",
        "x-ip-proxy",
        "x-risk-vpn",
    ]
    .iter()
    .any(|name| header_truthy(headers, name))
        || numeric_header_at_least(headers, "cf-threat-score", 10)
        || numeric_header_at_least(headers, "x-ip-risk", 75)
}

fn header_truthy(headers: &HeaderMap, name: &str) -> bool {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on" | "vpn" | "proxy"
            )
        })
        .unwrap_or(false)
}

fn numeric_header_at_least(headers: &HeaderMap, name: &str, limit: i64) -> bool {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<i64>().ok())
        .map(|value| value >= limit)
        .unwrap_or(false)
}

fn blocked_ip_exists(ip: &str) -> bool {
    let Ok(contents) = fs::read_to_string(blocked_ips_path()) else {
        return false;
    };

    contents.lines().any(|line| {
        let line = line.trim();
        !line.starts_with('#')
            && line
                .split_whitespace()
                .next()
                .map(|candidate| candidate == ip)
                .unwrap_or(false)
    })
}

fn auto_ban_public_ip(ip: &str, country: Option<&str>, vpn_or_proxy: bool) {
    if !is_public_bannable_ip(ip) || blocked_ip_exists(ip) {
        return;
    }

    let path = blocked_ips_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };

    let country = country.unwrap_or("unknown");
    let _ = writeln!(
        file,
        "{} # auto=geo_guard country={} vpn_or_proxy={} at={}",
        ip,
        country,
        vpn_or_proxy,
        crate::auth::now_iso()
    );
}

fn blocked_ips_path() -> PathBuf {
    env::var("LB_BLOCKED_IPS_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data").join("blocked_ips.txt"))
}

fn is_public_bannable_ip(ip: &str) -> bool {
    let Ok(ip) = ip.parse::<IpAddr>() else {
        return false;
    };

    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || o[0] == 0
                || o[0] >= 224
                || (o[0] == 100 && (64..=127).contains(&o[1]))
                || (o[0] == 192 && o[1] == 0 && o[2] == 2)
                || (o[0] == 198 && o[1] == 51 && o[2] == 100)
                || (o[0] == 203 && o[1] == 0 && o[2] == 113))
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] == 0x2001 && s[1] == 0x0db8))
        }
    }
}

fn env_bool(key: &str, default: bool) -> bool {
    env::var(key)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_slovakia_names() {
        assert_eq!(normalize_country_code("sk").as_deref(), Some("SK"));
        assert_eq!(normalize_country_code("Slovakia").as_deref(), Some("SK"));
    }

    #[test]
    fn extracts_first_forwarded_ip() {
        assert_eq!(
            clean_ip("8.8.8.8, 10.0.0.1").as_deref(),
            Some("8.8.8.8")
        );
        assert_eq!(clean_ip("8.8.8.8:443").as_deref(), Some("8.8.8.8"));
    }

    #[test]
    fn avoids_banning_local_networks() {
        assert!(!is_public_bannable_ip("127.0.0.1"));
        assert!(!is_public_bannable_ip("192.168.1.5"));
        assert!(is_public_bannable_ip("8.8.8.8"));
    }
}
