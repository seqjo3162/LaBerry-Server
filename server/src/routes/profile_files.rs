use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
    extract::Multipart,
};
use tokio::{fs, io::AsyncWriteExt};
use uuid::Uuid;
use sqlx::Row;

use crate::auth;
use crate::middleware::auth_guard::AuthUser;
use crate::server::AppState;

const MAX_PROFILE_UPLOAD_BYTES: i64 = 12 * 1024 * 1024; // 12MB

#[derive(serde::Serialize)]
pub struct ProfileFileResp {
    pub id: i64,
    pub original_name: String,
    pub file_size: i64,
    pub mime_type: String,
    pub created_at: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(upload))
        .route("/{file_id}/raw", get(get_raw))
}

fn is_allowed_mime(mime: &str) -> bool {
    let m = mime.to_ascii_lowercase();
    m.starts_with("image/") && m != "image/svg+xml"
}

fn transparent_png_response() -> axum::response::Response {
    let transparent_png: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
        0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
        0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
        0x42, 0x60, 0x82
    ];
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (headers, transparent_png.to_vec()).into_response()
}

async fn upload(
    State(st): State<AppState>,
    me: AuthUser,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let db = &st.db;

    let mut file_bytes: Vec<u8> = Vec::new();
    let mut original_name: Option<String> = None;
    let mut mime_type: Option<String> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        if name != "file" {
            continue;
        }

        original_name = field.file_name().map(|s| s.to_string());
        mime_type = field.content_type().map(|s| s.to_string());

        let data = match field.bytes().await {
            Ok(b) => b,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        };

        if (data.len() as i64) > MAX_PROFILE_UPLOAD_BYTES {
            return StatusCode::PAYLOAD_TOO_LARGE.into_response();
        }

        file_bytes = data.to_vec();
        break;
    }

    let original_name = original_name.unwrap_or_else(|| "avatar".to_string());
    let mime_type = mime_type.unwrap_or_else(|| "application/octet-stream".to_string());

    if file_bytes.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    if !is_allowed_mime(&mime_type) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(serde_json::json!({"detail":"Only raster images are allowed"})),
        )
            .into_response();
    }

    let id_name = Uuid::new_v4().to_string();
    let ext = match mime_type.to_ascii_lowercase().as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "bin",
    };

    let stored_filename = format!("{}.{}", id_name, ext);
    let dir = std::path::PathBuf::from("storage/profile");
    if fs::create_dir_all(&dir).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let storage_path = dir.join(&stored_filename);

    match fs::File::create(&storage_path).await {
        Ok(mut f) => {
            if f.write_all(&file_bytes).await.is_err() {
                let _ = fs::remove_file(&storage_path).await;
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }

    let created_at = auth::now_iso();
    let res = sqlx::query_scalar::<_, i64>(
        r#"INSERT INTO profile_files(filename, original_name, file_size, mime_type, storage_path, uploaded_by, created_at)
           VALUES($1, $2, $3, $4, $5, $6, $7) RETURNING id"#,
    )
    .bind(&stored_filename)
    .bind(&original_name)
    .bind(file_bytes.len() as i64)
    .bind(&mime_type)
    .bind(storage_path.to_string_lossy().to_string())
    .bind(me.id)
    .bind(created_at)
    .fetch_one(db)
    .await;

    let Ok(file_id) = res else {
        let _ = fs::remove_file(&storage_path).await;
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    (
        StatusCode::OK,
        Json(ProfileFileResp {
            id: file_id,
            original_name,
            file_size: file_bytes.len() as i64,
            mime_type,
            created_at: created_at.to_rfc3339(),
        }),
    )
        .into_response()
}

pub async fn get_raw(
    State(st): State<AppState>,
    Path(file_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;
    let row = sqlx::query(
        "SELECT original_name, mime_type, storage_path FROM profile_files WHERE id = $1 LIMIT 1",
    )
    .bind(file_id)
    .fetch_optional(db)
    .await;

    let Ok(Some(r)) = row else {
        let _ = sqlx::query("UPDATE users SET avatar_file_id = NULL WHERE avatar_file_id = $1")
            .bind(file_id)
            .execute(db)
            .await;
        return transparent_png_response();
    };

    let original_name: String = r.get("original_name");
    let mime_type: String = r.get("mime_type");
    let storage_path: String = r.get("storage_path");

    let path = std::path::Path::new(&storage_path);
    if !path.exists() {
        let _ = sqlx::query("UPDATE users SET avatar_file_id = NULL WHERE avatar_file_id = $1")
            .bind(file_id)
            .execute(db)
            .await;
        let _ = sqlx::query("DELETE FROM profile_files WHERE id = $1")
            .bind(file_id)
            .execute(db)
            .await;
        return transparent_png_response();
    }

    let Ok(bytes) = fs::read(&storage_path).await else {
        return transparent_png_response();
    };

    let mut headers = HeaderMap::new();
    let ct = mime_type.parse().unwrap_or(HeaderValue::from_static("application/octet-stream"));
    headers.insert(header::CONTENT_TYPE, ct);

    let disp = format!("inline; filename=\"{}\"", original_name.replace('"', ""));
    if let Ok(v) = disp.parse() {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }

    (headers, bytes).into_response()
}