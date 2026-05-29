use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::collections::VecDeque;
use sqlx::SqlitePool;

use crate::auth;

static BUCKETS: Lazy<DashMap<String, VecDeque<i64>>> = Lazy::new(DashMap::new);

/// Simple in-memory sliding-window rate limiter (with optional DB persistence).
/// Returns true if allowed.
pub fn allow(key: &str, max: usize, window_secs: i64) -> bool {
    let now = auth::now_unix();
    let mut entry = BUCKETS.entry(key.to_string()).or_insert_with(VecDeque::new);

    // prune
    while let Some(&t) = entry.front() {
        if now.saturating_sub(t) > window_secs {
            entry.pop_front();
        } else {
            break;
        }
    }

    if entry.len() >= max {
        return false;
    }

    entry.push_back(now);
    true
}

/// Persistent rate limiting with database (prevents bypass on server restart)
pub async fn allow_with_db(
    db: &SqlitePool,
    key: &str,
    max: usize,
    window_secs: i64,
) -> Result<bool, sqlx::Error> {
    let now = auth::now_unix();
    let window_start = now - window_secs;

    // 🔴 SECURITY FIX: DB-backed rate limiting to prevent bypass on restart
    // Clean expired entries for this key
    sqlx::query(
        r#"DELETE FROM rate_limit_logs WHERE key = ? AND timestamp < ?"#
    )
    .bind(key)
    .bind(window_start)
    .execute(db)
    .await?;

    // Count current requests in window
    let count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM rate_limit_logs WHERE key = ? AND timestamp > ?"#
    )
    .bind(key)
    .bind(window_start)
    .fetch_one(db)
    .await?;

    if count >= max as i64 {
        return Ok(false);
    }

    // Record this request
    sqlx::query(
        r#"INSERT INTO rate_limit_logs(key, timestamp) VALUES(?, ?)"#
    )
    .bind(key)
    .bind(now)
    .execute(db)
    .await?;

    Ok(true)
}

/// Background cleanup task for rate_limit_logs (call this periodically)
pub async fn cleanup_expired_logs(db: &SqlitePool) -> Result<u64, sqlx::Error> {
    let cutoff_time = auth::now_unix() - 86400; // 24 hours
    
    let result = sqlx::query(
        r#"DELETE FROM rate_limit_logs WHERE timestamp < ?"#
    )
    .bind(cutoff_time)
    .execute(db)
    .await?;

    Ok(result.rows_affected())
}

/// In-memory cleanup for BUCKETS to prevent unbounded memory growth
pub fn cleanup_expired_buckets() {
    let now = auth::now_unix();
    BUCKETS.retain(|_, bucket| {
        if let Some(&oldest_ts) = bucket.front() {
            // Keep if within 24h, remove if empty
            now - oldest_ts < 86400
        } else {
            false // remove empty buckets
        }
    });
}

use std::net::IpAddr;

/// Extract client IP from headers, but only trust forwarded headers when the
/// immediate peer (the connector) is a trusted proxy or local address.
///
/// - `headers` — request headers
/// - `peer_ip` — actual TCP peer IP (if available)
/// - `trusted_proxies` — list of IPs considered trusted (e.g. Caddy frontends)
pub fn extract_ip(
    headers: &axum::http::HeaderMap,
    peer_ip: Option<IpAddr>,
    trusted_proxies: &[IpAddr],
) -> Option<String> {
    // Determine if the immediate peer is trusted (loopback/private or in configured list)
    let mut peer_is_trusted = false;
    if let Some(peer) = peer_ip {
        peer_is_trusted = peer.is_loopback()
            || match &peer {
                IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
                IpAddr::V6(v6) => v6.is_unique_local() || v6.is_unicast_link_local(),
            }
            || trusted_proxies.iter().any(|p| *p == peer);
    }

    // If peer is trusted, prefer X-Forwarded-For / X-Real-IP (first value)
    if peer_is_trusted {
        if let Some(v) = headers.get("x-forwarded-for").and_then(|h| h.to_str().ok()) {
            if let Some(first) = v.split(',').next() {
                let ip = first.trim();
                if !ip.is_empty() {
                    return Some(ip.to_string());
                }
            }
        }

        if let Some(v) = headers.get("x-real-ip").and_then(|h| h.to_str().ok()) {
            let ip = v.trim();
            if !ip.is_empty() {
                return Some(ip.to_string());
            }
        }
    }

    // Fallback to peer IP if available
    if let Some(peer) = peer_ip {
        return Some(peer.to_string());
    }

    None
}
