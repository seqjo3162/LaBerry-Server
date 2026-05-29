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
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::Path,
    str::FromStr,
    sync::Arc,
};

use crate::server::AppState;

#[derive(Clone)]
pub struct GeoGuardState {
    pub blocked_networks: Arc<Vec<IpNet>>,
    pub allowed_domains: Arc<Vec<String>>,
}

impl GeoGuardState {
    pub fn from_ripe_file<P: AsRef<Path>>(ripe_file_path: P) -> anyhow::Result<Self> {
        let networks = load_sk_networks_from_ripe(ripe_file_path)?;
        Ok(Self {
            blocked_networks: Arc::new(networks),
            allowed_domains: Arc::new(vec!["laberry.ru".to_string()]),
        })
    }

    /// Загружает только список CIDR из текстового файла (по одному CIDR на строку)
    pub fn from_custom_file<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let path_str = path.as_ref().to_str().ok_or_else(|| anyhow::anyhow!("Non-UTF-8 path: {:?}", path.as_ref()))?;
        let networks = load_custom_networks(path_str)?;
        Ok(Self {
            blocked_networks: Arc::new(networks),
            allowed_domains: Arc::new(vec!["laberry.ru".to_string()]),
        })
    }
}

fn load_sk_networks_from_ripe<P: AsRef<Path>>(path: P) -> anyhow::Result<Vec<IpNet>> {
    let data = fs::read_to_string(path)?;
    let mut nets = Vec::new();

    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("2|") {
            continue;
        }

        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() < 7 {
            continue;
        }

        if parts[1] != "SK" {
            continue;
        }

        match parts[2] {
            "ipv4" => {
                let addr = Ipv4Addr::from_str(parts[3])?;
                let count: u32 = parts[4].parse()?;
                let mask = 32u8
                    .checked_sub((count as f64).log2() as u8)
                    .ok_or_else(|| anyhow::anyhow!("Invalid IPv4 count: {}", count))?;
                let net = ipnet::Ipv4Net::new(addr, mask)?;
                nets.push(IpNet::V4(net));
            }
            "ipv6" => {
                let addr = Ipv6Addr::from_str(parts[3])?;
                let prefix_len: u8 = parts[4].parse()?;
                let net = ipnet::Ipv6Net::new(addr, prefix_len)?;
                nets.push(IpNet::V6(net));
            }
            _ => {}
        }
    }

    Ok(nets)
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

fn get_real_ip_from_headers(headers: &HeaderMap, trusted_proxies: &[IpAddr]) -> IpAddr {
    // Пытаемся получить IP из X-Forwarded-For
    if let Some(xff) = headers.get("X-Forwarded-For") {
        if let Ok(xff_str) = xff.to_str() {
            if let Some(first) = xff_str.split(',').next() {
                if let Ok(real_ip) = first.trim().parse::<IpAddr>() {
                    let is_trusted = trusted_proxies.contains(&real_ip)
                        || real_ip.is_loopback()
                        || match &real_ip {
                            IpAddr::V4(addr) => addr.is_private() || addr.is_link_local(),
                            IpAddr::V6(addr) => addr.is_unique_local() || addr.is_unicast_link_local(),
                        };
                    if is_trusted {
                        return real_ip;
                    }
                }
            }
        }
    }
    // Пытаемся получить IP из X-Real-IP
    if let Some(xri) = headers.get("X-Real-IP") {
        if let Ok(xri_str) = xri.to_str() {
            if let Ok(real_ip) = xri_str.trim().parse::<IpAddr>() {
                let is_trusted = trusted_proxies.contains(&real_ip)
                    || real_ip.is_loopback()
                    || match &real_ip {
                        IpAddr::V4(addr) => addr.is_private() || addr.is_link_local(),
                        IpAddr::V6(addr) => addr.is_unique_local() || addr.is_unicast_link_local(),
                    };
                if is_trusted {
                    return real_ip;
                }
            }
        }
    }
    // Возвращаем loopback по умолчанию
    IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))
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