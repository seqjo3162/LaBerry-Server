use axum::{
    body::Body,
    extract::{State, ConnectInfo},
    http::{HeaderMap, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

use ipnet::IpNet;
use serde_json::json;
use std::{
    fs,
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::Arc,
};

use crate::server::AppState;

#[derive(Clone)]
pub struct GeoGuardState {
    pub blocked_networks: Arc<Vec<IpNet>>,
    pub allowed_domains: Arc<Vec<String>>,
}

impl GeoGuardState {
    /// Загружает список заблокированных подсетей из кастомного файла (по одной CIDR на строку).
    /// Если файл не существует, блокировка не применяется.
    pub fn from_custom_file<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let p = path.as_ref();
        let networks = if p.exists() {
            let path_str = p
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("Non-UTF-8 path: {:?}", p))?;
            load_custom_networks(path_str)?
        } else {
            tracing::warn!(
                "[GEO] Blocked-CIDR file not found at {:?}. \
                 Starting without geo-blocking. Create the file to enable it.",
                p
            );
            Vec::new()
        };
        Ok(Self {
            blocked_networks: Arc::new(networks),
            allowed_domains: Arc::new(vec!["laberry.ru".to_string()]),
        })
    }
}

fn load_custom_networks(path: &str) -> anyhow::Result<Vec<IpNet>> {
    let data = fs::read_to_string(path)?;
    let nets: Vec<IpNet> = data
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            line.parse().ok()
        })
        .collect();
    Ok(nets)
}

fn get_real_ip(addr: SocketAddr, headers: &HeaderMap, trusted_proxies: &[IpAddr]) -> IpAddr {
    let ip = addr.ip();
    // Разрешаем X-Forwarded-For для loopback, частных и link-local сетей
    let is_trusted = trusted_proxies.contains(&ip)
        || ip.is_loopback()
        || match &ip {
            IpAddr::V4(addr) => addr.is_private() || addr.is_link_local(),
            IpAddr::V6(addr) => addr.is_unique_local() || addr.is_unicast_link_local(),
        };
    if !is_trusted {
        return ip;
    }
    if let Some(xff) = headers.get("X-Forwarded-For") {
        if let Ok(xff_str) = xff.to_str() {
            if let Some(first) = xff_str.split(',').next() {
                if let Ok(real_ip) = first.trim().parse::<IpAddr>() {
                    return real_ip;
                }
            }
        }
    }
    ip
}

pub async fn geo_guard(
    State(app): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let ip = get_real_ip(peer, request.headers(), &app.trusted_proxies);

    if ip.is_loopback() {
        return next.run(request).await;
    }

    let blocked = app
        .geo_guard
        .blocked_networks
        .iter()
        .any(|net| net.contains(&ip));

    if blocked {
        tracing::warn!("Blocked IP: {}", ip);
        return deny(
            "region_blocked",
            "LaBerry services are unavailable in your region.",
        );
    }

    next.run(request).await
}

fn deny(code: &'static str, message: &'static str) -> Response {
    (
        StatusCode::FORBIDDEN,
        axum::Json(json!({
            "success": false,
            "error": code,
            "message": message,
        })),
    )
        .into_response()
}