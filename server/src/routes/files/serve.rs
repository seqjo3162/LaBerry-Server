use axum::{
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use std::io::{Write, Seek, Read};

use crate::server::AppState;
use crate::middleware::auth_guard::AuthUser;

use super::{resolve_user_id_for_file_request, load_file_for_serving, load_file_for_access, can_access_chat_by_user_id};

/// Returns a signed download link for the file
pub(crate) async fn get_file_link(
    State(st): State<AppState>,
    axum::extract::Path(file_id): axum::extract::Path<i64>,
    me: AuthUser,
) -> axum::response::Response {
    let file_row = match load_file_for_serving(&st, file_id).await {
        Ok(row) => row,
        Err(code) => return (code, "file not found or expired").into_response(),
    };

    if file_row.is_expired != 0 {
        return (StatusCode::GONE, "file expired").into_response();
    }

    // Get token_version from DB
    let token_version = match sqlx::query_scalar::<_, i64>(
        "SELECT token_version FROM users WHERE id = ? LIMIT 1"
    )
    .bind(me.id)
    .fetch_optional(&st.db)
    .await {
        Ok(Some(tv)) => tv,
        _ => return (StatusCode::INTERNAL_SERVER_ERROR, "db_error").into_response(),
    };

    match crate::auth::create_file_download_token(me.id, file_id, token_version) {
        Ok((token, _ttl)) => {
            let url = format!(
                "/api/files/{}?dl={}",
                file_id,
                urlencoding::encode(&token)
            );
            (StatusCode::OK, axum::Json(serde_json::json!({ "url": url }))).into_response()
        }
        Err(e) => {
            tracing::error!("[FILES] Failed to create download token for file_id={}: {}", file_id, e);
            (StatusCode::INTERNAL_SERVER_ERROR, "token_creation_failed").into_response()
        }
    }
}

/// Returns a preview (thumbnail) for image files
pub(crate) async fn get_preview(
    State(st): State<AppState>,
    axum::extract::Path(file_id): axum::extract::Path<i64>,
    me: AuthUser,
) -> axum::response::Response {
    let user_id = match resolve_user_id_for_file_request(&st, Some(&me), file_id, None).await {
        Ok(uid) => uid,
        Err(code) => return (code, "unauthorized").into_response(),
    };

    let file_row = match load_file_for_access(&st, file_id).await {
        Ok(row) => row,
        Err(code) => return (code, "file not found").into_response(),
    };

    if !can_access_chat_by_user_id(&st, user_id, file_row.chat_id).await {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }

    // Check for existing thumbnail
    let thumbs_dir = std::path::PathBuf::from("storage/files/thumbs");
    let stem = std::path::Path::new(&file_row.filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&file_row.filename);
    
    let extensions = ["png", "webp", "jpg", "jpeg"];
    for ext in &extensions {
        let thumb_path = thumbs_dir.join(format!("{}.{}", stem, ext));
        if let Ok(meta) = tokio::fs::metadata(&thumb_path).await {
            if meta.is_file() && meta.len() > 0 {
                if let Ok(bytes) = tokio::fs::read(&thumb_path).await {
                    let len = bytes.len();
                    let mut resp: Response<axum::body::Body> = Response::new(bytes.into());
                    resp.headers_mut().insert(header::CONTENT_TYPE, "image/png".parse().unwrap());
                    resp.headers_mut().insert(header::CONTENT_LENGTH, len.to_string().parse().unwrap());
                    return resp.into_response();
                }
            }
        }
    }

    (StatusCode::NOT_FOUND, "preview not found").into_response()
}

/// Returns a ZIP archive of the file
pub(crate) async fn get_archive(
    State(st): State<AppState>,
    axum::extract::Path(file_id): axum::extract::Path<i64>,
    me: AuthUser,
) -> axum::response::Response {
    let user_id = match resolve_user_id_for_file_request(&st, Some(&me), file_id, None).await {
        Ok(uid) => uid,
        Err(code) => return (code, "unauthorized").into_response(),
    };

    let file_row = match load_file_for_serving(&st, file_id).await {
        Ok(row) => row,
        Err(code) => return (code, "file not found").into_response(),
    };

    if !can_access_chat_by_user_id(&st, user_id, file_row.chat_id).await {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }

    // Read the file
    match tokio::fs::read(&file_row.storage_path).await {
        Ok(bytes) => {
            // Create a ZIP in memory using a temp File (zip 0.6 requires Seek)
            let temp_path = std::env::temp_dir().join(format!("laberry-zip-{}", file_id));
            let result = (|| -> Result<Vec<u8>, std::io::Error> {
                let mut tf = std::fs::File::create(&temp_path)?;
                {
                    let mut archive = zip::ZipWriter::new(&mut tf);
                    let options = zip::write::FileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored);
                    
                    archive.start_file(&file_row.original_name, options)?;
                    archive.write_all(&bytes)?;
                    archive.finish()?;
                }
                
                tf.seek(std::io::SeekFrom::Start(0))?;
                let mut buf = Vec::new();
                tf.read_to_end(&mut buf)?;
                Ok(buf)
            })();
            
            let _ = std::fs::remove_file(&temp_path);
            
            let disposition = format!(
                "attachment; filename=\"{}\"; filename*=UTF-8''{}",
                urlencoding::encode(&file_row.original_name),
                urlencoding::encode(&file_row.original_name)
            );
            
            match result {
                Ok(buf) => {
                    let content_len = buf.len();
                    let mut resp: Response<axum::body::Body> = Response::new(buf.into());
                    resp.headers_mut().insert(header::CONTENT_TYPE, "application/zip".parse().unwrap());
                    resp.headers_mut().insert(header::CONTENT_DISPOSITION, disposition.parse().unwrap());
                    resp.headers_mut().insert(header::CONTENT_LENGTH, content_len.to_string().parse().unwrap());
                    resp
                }
                Err(_) => {
                    // Fallback: return raw bytes
                    let content_len = bytes.len();
                    let body = bytes.into();
                    let mut resp: Response<axum::body::Body> = Response::new(body);
                    resp.headers_mut().insert(header::CONTENT_TYPE, "application/octet-stream".parse().unwrap());
                    resp.headers_mut().insert(header::CONTENT_LENGTH, content_len.to_string().parse().unwrap());
                    resp
                }
            }
        }
        Err(_) => (StatusCode::NOT_FOUND, "file not found on disk").into_response(),
    }
}

/// Returns the raw file content
pub(crate) async fn get_file_raw(
    State(st): State<AppState>,
    axum::extract::Path(file_id): axum::extract::Path<i64>,
    me: AuthUser,
) -> axum::response::Response {
    let user_id = match resolve_user_id_for_file_request(&st, Some(&me), file_id, None).await {
        Ok(uid) => uid,
        Err(code) => return (code, "unauthorized").into_response(),
    };

    let file_row = match load_file_for_serving(&st, file_id).await {
        Ok(row) => row,
        Err(code) => return (code, "file not found").into_response(),
    };

    if !can_access_chat_by_user_id(&st, user_id, file_row.chat_id).await {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }

    match tokio::fs::read(&file_row.storage_path).await {
        Ok(bytes) => {
            let mime = file_row.mime_type.clone();
            let content_len = bytes.len();
            let mut resp: Response<axum::body::Body> = Response::new(bytes.into());
            resp.headers_mut().insert(header::CONTENT_TYPE, mime.parse().unwrap());
            resp.headers_mut().insert(header::CONTENT_LENGTH, content_len.to_string().parse().unwrap());
            resp
        }
        Err(_) => (StatusCode::NOT_FOUND, "file not found on disk").into_response(),
    }
}

/// Downloads the file with proper content disposition
pub(crate) async fn get_file(
    State(st): State<AppState>,
    axum::extract::Path(file_id): axum::extract::Path<i64>,
    axum::extract::Query(dl): axum::extract::Query<Option<String>>,
    me: AuthUser,
) -> axum::response::Response {
    let user_id = match resolve_user_id_for_file_request(&st, Some(&me), file_id, dl.as_deref()).await {
        Ok(uid) => uid,
        Err(code) => return (code, "unauthorized").into_response(),
    };

    let file_row = match load_file_for_serving(&st, file_id).await {
        Ok(row) => row,
        Err(code) => return (code, "file not found").into_response(),
    };

    if !can_access_chat_by_user_id(&st, user_id, file_row.chat_id).await {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }

    match tokio::fs::read(&file_row.storage_path).await {
        Ok(bytes) => {
            let original_name = file_row.original_name.clone();
            let mime = file_row.mime_type.clone();
            let disposition = format!(
                "attachment; filename=\"{}\"; filename*=UTF-8''{}",
                urlencoding::encode(&original_name),
                urlencoding::encode(&original_name)
            );
            
            let content_len = bytes.len();
            let mut resp: Response<axum::body::Body> = Response::new(bytes.into());
            resp.headers_mut().insert(header::CONTENT_TYPE, mime.parse().unwrap());
            resp.headers_mut().insert(header::CONTENT_DISPOSITION, disposition.parse().unwrap());
            resp.headers_mut().insert(header::CONTENT_LENGTH, content_len.to_string().parse().unwrap());
            resp
        }
        Err(_) => (StatusCode::NOT_FOUND, "file not found on disk").into_response(),
    }
}
