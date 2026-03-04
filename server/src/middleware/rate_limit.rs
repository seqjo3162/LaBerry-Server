use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::collections::VecDeque;

use crate::auth;

static BUCKETS: Lazy<DashMap<String, VecDeque<i64>>> = Lazy::new(DashMap::new);

pub fn allow(key: &str, max: usize, window_secs: i64) -> bool {
    let now = auth::now_unix();
    let mut entry = BUCKETS.entry(key.to_string()).or_insert_with(VecDeque::new);
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

pub fn extract_ip(headers: &axum::http::HeaderMap) -> Option<String> {
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

    None
}
