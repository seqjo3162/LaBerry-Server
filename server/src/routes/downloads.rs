use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use sqlx::Row;
use tokio::fs::File;
use tokio_util::io::ReaderStream;

use crate::server::AppState;

#[derive(Serialize)]
pub struct AppDownloadView {
    pub platform: String,
    pub title: String,
    pub version: String,
    pub original_name: Option<String>,
    pub file_size: Option<i64>,
    pub uploaded_at: Option<String>,
    pub available: bool,
    pub download_url: Option<String>,
}

fn normalize_platform(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "android" | "mobile" | "apk" => Some("android"),
        "pc" | "windows" | "desktop" => Some("pc"),
        _ => None,
    }
}

fn platform_title(platform: &str) -> &'static str {
    match platform {
        "android" => "Мобильная версия",
        "pc" => "ПК клиент",
        _ => "Клиент",
    }
}

fn ascii_download_filename(original: &str, platform: &str) -> String {
    let mut out = String::with_capacity(original.len());
    for ch in original.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else if ch.is_whitespace() {
            out.push('_');
        }
    }
    if out.is_empty() {
        match platform {
            "android" => "laberry.apk".to_string(),
            "pc" => "laberry-client.bin".to_string(),
            _ => "laberry-download.bin".to_string(),
        }
    } else {
        out
    }
}

fn percent_encode_header_value(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &b in input.as_bytes() {
        let keep = matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~');
        if keep {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0F) as usize] as char);
        }
    }
    out
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_downloads))
        .route("/{platform}/file", get(download_latest))
        .fallback(get(list_downloads))
}

async fn latest_for_platform(db: &sqlx::PgPool, platform: &str) -> Option<sqlx::postgres::PgRow> {
    sqlx::query(
        r#"
        SELECT id, platform, version, original_name, mime_type, file_size, storage_path, uploaded_at
        FROM app_downloads
        WHERE platform = $1 AND is_active = TRUE
        ORDER BY id DESC
        LIMIT 1
        "#,
    )
    .bind(platform)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

async fn list_downloads(State(st): State<AppState>) -> impl IntoResponse {
    let mut out = Vec::new();
    for platform in ["android", "pc"] {
        let row = latest_for_platform(&st.db, platform).await;
        if let Some(r) = row {
            out.push(AppDownloadView {
                platform: platform.to_string(),
                title: platform_title(platform).to_string(),
                version: r.try_get("version").unwrap_or_default(),
                original_name: r.try_get("original_name").ok(),
                file_size: r.try_get("file_size").ok(),
                uploaded_at: r.try_get::<chrono::DateTime<chrono::Utc>, _>("uploaded_at").ok().map(|d| d.to_rfc3339()),
                available: true,
                download_url: Some(format!("/api/downloads/{}/file", platform)),
            });
        } else {
            out.push(AppDownloadView {
                platform: platform.to_string(),
                title: platform_title(platform).to_string(),
                version: String::new(),
                original_name: None,
                file_size: None,
                uploaded_at: None,
                available: false,
                download_url: None,
            });
        }
    }

    (StatusCode::OK, Json(out)).into_response()
}

async fn download_latest(
    State(st): State<AppState>,
    Path(platform_raw): Path<String>,
) -> Response {
    let Some(platform) = normalize_platform(&platform_raw) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let Some(row) = latest_for_platform(&st.db, platform).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let storage_path: String = row.get("storage_path");
    let original_name: String = row.get("original_name");
    let mime_type: String = row.get("mime_type");

    let Ok(file) = File::open(&storage_path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let fallback_name = ascii_download_filename(&original_name, platform);
    let encoded_name = percent_encode_header_value(&original_name);
    let stream = ReaderStream::new(file);
    let mut response = Response::new(Body::from_stream(stream));

    if let Ok(value) = HeaderValue::from_str(&mime_type) {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    if let Ok(value) = HeaderValue::from_str(&format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        fallback_name, encoded_name
    )) {
        response.headers_mut().insert(header::CONTENT_DISPOSITION, value);
    }

    response
}
