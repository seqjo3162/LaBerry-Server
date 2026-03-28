use axum::{
    extract::{Multipart, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use std::{io::Cursor, io::SeekFrom, path::{Path as FsPath, PathBuf}};
use tokio::{fs, fs::File, io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt}};
use tokio_util::io::ReaderStream;
use uuid::Uuid;
use serde::{Serialize, Deserialize};
use sqlx::Row;

use crate::auth;
use crate::middleware::auth_guard::AuthUser;
use crate::server::AppState;

const MAX_UPLOAD_BYTES: i64 = 50 * 1024 * 1024;
const MAX_IMAGE_DIM: u32 = 12000;
const MAX_IMAGE_PIXELS: u64 = 80_000_000;
const THUMB_MAX_W: u32 = 800;
const THUMB_MAX_H: u32 = 800;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/:file_id/link", get(get_file_link))
.route("/:file_id/preview", get(get_preview))
        .route("/:file_id/archive", get(get_archive))
        .route("/:file_id/raw", get(get_file_raw))
        .route("/", post(upload_file))
        .route("/:file_id", get(get_file))
}



#[derive(Deserialize, Default)]
pub(crate) struct DlQuery {
    /// short-lived signed token for downloads/previews without Authorization header
    dl: Option<String>,
}

fn inline_safe(mime: &str) -> bool {
    let m = mime.to_ascii_lowercase();
    if m.starts_with("image/") && m != "image/svg+xml" {
        return true;
    }
    if m.starts_with("video/") || m.starts_with("audio/") {
        return true;
    }
    // allow plain text previews (not html)
    if m == "text/plain" {
        return true;
    }
    false
}

async fn resolve_user_id_for_file_request(
    st: &AppState,
    me: Option<&AuthUser>,
    file_id: i64,
    dl: Option<&str>,
) -> Result<i64, StatusCode> {
    if let Some(me) = me {
        return Ok(me.id);
    }

    let Some(token) = dl else {
        return Err(StatusCode::UNAUTHORIZED);
    };

    let claims = auth::decode_file_download_claims(token)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    if claims.file_id != file_id {
        return Err(StatusCode::FORBIDDEN);
    }

    let row = sqlx::query("SELECT token_version, is_banned FROM users WHERE id = ? LIMIT 1")
        .bind(claims.uid)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten()
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let banned: i64 = row.get("is_banned");
    if banned != 0 {
        return Err(StatusCode::FORBIDDEN);
    }

    let tv: i64 = row.get("token_version");
    if tv != claims.token_version {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(claims.uid)
}

async fn can_access_chat_by_user_id(st: &AppState, user_id: i64, chat_id: i64) -> bool {
    #[derive(sqlx::FromRow)]
    struct ChatInfo {
        server_id: Option<i64>,
        is_private: i64,
        kind: Option<String>,
    }

    let chat: Option<ChatInfo> = sqlx::query_as("SELECT server_id, is_private, kind FROM chats WHERE id = ?")
        .bind(chat_id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten();

    let Some(chat) = chat else { return false; };

    let kind = chat.kind.unwrap_or_else(|| "text".to_string());

    // For voice chats: allow only while user is actually in this voice channel
    if kind == "voice" {
        if st.hub.voice_get_user_channel(user_id) != Some(chat_id) {
            return false;
        }
    }

    if chat.is_private != 0 {
        sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM chat_participants WHERE chat_id = ? AND user_id = ? LIMIT 1",
        )
        .bind(chat_id)
        .bind(user_id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten()
        .is_some()
    } else if let Some(server_id) = chat.server_id {
        sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM server_members WHERE server_id = ? AND user_id = ? LIMIT 1",
        )
        .bind(server_id)
        .bind(user_id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten()
        .is_some()
    } else {
        false
    }
}


fn is_raster_image(mime: &str) -> bool {
    let m = mime.to_ascii_lowercase();
    m.starts_with("image/") && m != "image/svg+xml"
}

fn thumb_path_for(stored_filename: &str) -> PathBuf {
    let thumbs_dir = PathBuf::from("storage/files/thumbs");
    let stem = std::path::Path::new(stored_filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(stored_filename);
    thumbs_dir.join(format!("{}.png", stem))
}

async fn ensure_thumbnail(original_path: &FsPath, stored_filename: &str, mime_type: &str) -> Result<(), StatusCode> {
    if !is_raster_image(mime_type) {
        return Ok(());
    }

    let thumb_path = thumb_path_for(stored_filename);
    if fs::metadata(&thumb_path).await.is_ok() {
        return Ok(());
    }

    let thumbs_dir = thumb_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("storage/files/thumbs"));
    if fs::create_dir_all(&thumbs_dir).await.is_err() {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    // CPU-heavy decode/resize/encode -> blocking
    let orig = original_path.to_path_buf();
    let out_bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, StatusCode> {
        // Quick header check: dimensions + megapixels limit
        let (w, h) = image::image_dimensions(&orig)
            .map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
        let pixels = (w as u64).saturating_mul(h as u64);
        if w > MAX_IMAGE_DIM || h > MAX_IMAGE_DIM || pixels > MAX_IMAGE_PIXELS {
            return Err(StatusCode::UNPROCESSABLE_ENTITY);
        }

        let img = image::open(&orig)
            .map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
        let thumb = img.thumbnail(THUMB_MAX_W, THUMB_MAX_H);

        let mut out = Vec::new();
        thumb
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Ok(out)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)??;

    if fs::write(&thumb_path, out_bytes).await.is_err() {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    Ok(())
}

fn sanitize_filename(input: &str) -> String {
    // Минимальная безопасная санация для Content-Disposition.
    // Оставляем ASCII буквы/цифры и ._- ; всё остальное заменяем на '_'.
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        let ok = ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-';
        if ok {
            out.push(ch);
        } else if ch.is_whitespace() {
            out.push('_');
        } else {
            out.push('_');
        }
    }

    if out.is_empty() {
        "file".to_string()
    } else {
        out
    }
}

async fn upload_file(
    State(st): State<AppState>,
    me: AuthUser,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let storage_dir = PathBuf::from("storage/files");
    if fs::create_dir_all(&storage_dir).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let mut chat_id: Option<i64> = None;
    let mut original_name = "file.bin".to_string();
    let mut mime_type = "application/octet-stream".to_string();
    let mut saved_path: Option<PathBuf> = None;
    let mut stored_filename: Option<String> = None;
    let mut file_size: i64 = 0;

    while let Ok(Some(mut field)) = multipart.next_field().await {
        match field.name().unwrap_or("") {
            "chat_id" => {
                if let Ok(txt) = field.text().await {
                    chat_id = txt.parse::<i64>().ok();
                }
            }

            "file" => {
                original_name = field.file_name().unwrap_or("file").to_string();
                mime_type = field
                    .content_type()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "application/octet-stream".to_string());

                let ext = std::path::Path::new(&original_name)
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");

                let filename = if ext.is_empty() {
                    Uuid::new_v4().to_string()
                } else {
                    format!("{}.{}", Uuid::new_v4(), ext)
                };

                let path = storage_dir.join(&filename);
                stored_filename = Some(filename);
                let mut file = match fs::File::create(&path).await {
                    Ok(f) => f,
                    Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                };

                while let Some(chunk) = field.chunk().await.unwrap_or(None) {
                    file_size += chunk.len() as i64;
                    if file_size > MAX_UPLOAD_BYTES {
                        let _ = fs::remove_file(&path).await;
                        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
                    }
                    if file.write_all(&chunk).await.is_err() {
                        let _ = fs::remove_file(&path).await;
                        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                    }
                }

                saved_path = Some(path);
            }
            _ => {}
        }
    }

    let chat_id = match chat_id {
        Some(v) => v,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };

    let path = match saved_path {
        Some(p) => p,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };

    let stored_filename = match stored_filename {
        Some(v) => v,
        None => {
            let _ = fs::remove_file(&path).await;
            return StatusCode::BAD_REQUEST.into_response();
        }
    };
    // Проверка доступа: приватный чат -> chat_participants; серверный канал -> server_members
    // + voice chat: only while user is in this voice channel
    if !can_access_chat_by_user_id(&st, me.id, chat_id).await {
        let _ = fs::remove_file(&path).await;
        return StatusCode::FORBIDDEN.into_response();
    }

    // Защита от DoS картинками: ограничиваем мегапиксели + делаем превью.
    if let Err(code) = ensure_thumbnail(&path, &stored_filename, &mime_type).await {
        let _ = fs::remove_file(&path).await;
        // если уже успели сделать thumb (не должно) - удалим тоже
        let _ = fs::remove_file(thumb_path_for(&stored_filename)).await;
        return code.into_response();
    }

    let created_at = auth::now_iso();

    let r = sqlx::query(
        r#"
        INSERT INTO files (
            filename, original_name, file_size, mime_type,
            storage_path, uploaded_by, chat_id, created_at
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(&stored_filename)
    .bind(&original_name)
    .bind(file_size)
    .bind(&mime_type)
    .bind(path.to_string_lossy().to_string())
    .bind(me.id)  // Исправлено: было me.user.id, теперь me.id
    .bind(chat_id)
    .bind(&created_at)
    .execute(&st.db)
    .await;

    let Ok(r) = r else {
        let _ = fs::remove_file(&path).await;
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    let file_id = r.last_insert_rowid();

    (
        StatusCode::OK,
        axum::Json(serde_json::json!({ "id": file_id })),
    )
        .into_response()
}


async fn get_file_link(
    State(st): State<AppState>,
    me: AuthUser,
    Path(file_id): Path<i64>,
) -> impl IntoResponse {
    #[derive(sqlx::FromRow)]
    struct FileRow {
        chat_id: i64,
    }

    let row: Option<FileRow> = sqlx::query_as("SELECT chat_id FROM files WHERE id = ?")
        .bind(file_id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten();

    let Some(f) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if !can_access_chat_by_user_id(&st, me.id, f.chat_id).await {
        return StatusCode::FORBIDDEN.into_response();
    }

    // include current token_version to invalidate on logout/password change
    let tv: i64 = sqlx::query_scalar("SELECT token_version FROM users WHERE id = ? LIMIT 1")
        .bind(me.id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten()
        .unwrap_or(0);

    let (dl, ttl) = match auth::create_file_download_token(me.id, file_id, tv) {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let resp = serde_json::json!({
        "download_url": format!("/api/files/{}?dl={}", file_id, dl),
        "raw_url": format!("/api/files/{}/raw?dl={}", file_id, dl),
        "preview_url": format!("/api/files/{}/preview?dl={}", file_id, dl),
        "expires_in_sec": ttl
    });

    (StatusCode::OK, Json(resp)).into_response()
}

pub(crate) async fn get_file(
    State(st): State<AppState>,
    me: Option<AuthUser>,
    Path(file_id): Path<i64>,
    Query(q): Query<DlQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    #[derive(sqlx::FromRow)]
    struct FileData {
        original_name: String,
        mime_type: String,
        storage_path: String,
        chat_id: i64,
    }
    
    let row: Option<FileData> = sqlx::query_as(
        "SELECT original_name, mime_type, storage_path, chat_id FROM files WHERE id = ?",
    )
    .bind(file_id)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten();

    let Some(f) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };
    
    

let uid = match resolve_user_id_for_file_request(&st, me.as_ref(), file_id, q.dl.as_deref()).await {
    Ok(v) => v,
    Err(code) => return code.into_response(),
};

if !can_access_chat_by_user_id(&st, uid, f.chat_id).await {
    return StatusCode::FORBIDDEN.into_response();
}

let storage_path = PathBuf::from(f.storage_path);
    serve_file_with_range(storage_path, &f.mime_type, &f.original_name, false, headers).await
}


pub(crate) async fn get_file_raw(
    State(st): State<AppState>,
    me: Option<AuthUser>,
    Path(file_id): Path<i64>,
    Query(q): Query<DlQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    #[derive(sqlx::FromRow)]
    struct FileData {
        original_name: String,
        mime_type: String,
        storage_path: String,
        chat_id: i64,
    }

    let row: Option<FileData> = sqlx::query_as(
        "SELECT original_name, mime_type, storage_path, chat_id FROM files WHERE id = ?",
    )
    .bind(file_id)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten();

    let Some(f) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    

let uid = match resolve_user_id_for_file_request(&st, me.as_ref(), file_id, q.dl.as_deref()).await {
    Ok(v) => v,
    Err(code) => return code.into_response(),
};

if !can_access_chat_by_user_id(&st, uid, f.chat_id).await {
    return StatusCode::FORBIDDEN.into_response();
}

let storage_path = PathBuf::from(f.storage_path);
    serve_file_with_range(storage_path, &f.mime_type, &f.original_name, true, headers).await
}

async fn serve_file_with_range(
    storage_path: PathBuf,
    mime_type: &str,
    original_name: &str,
    inline: bool,
    headers: HeaderMap,
) -> axum::response::Response {
    let meta = match fs::metadata(&storage_path).await {
        Ok(m) => m,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let total = meta.len();
    if total == 0 {
        return StatusCode::NOT_FOUND.into_response();
    }

    let ct = HeaderValue::from_str(mime_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));

    let safe_name = sanitize_filename(original_name);

    let inline = inline && inline_safe(mime_type);
    let cd_value = if inline {
        format!("inline; filename=\"{}\"", safe_name)
    } else {
        format!("attachment; filename=\"{}\"", safe_name)
    };
    let cd = HeaderValue::from_str(&cd_value).unwrap_or_else(|_| {
        if inline {
            HeaderValue::from_static("inline")
        } else {
            HeaderValue::from_static("attachment")
        }
    });

    let accept_ranges = (header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    let nosniff = (header::HeaderName::from_static("x-content-type-options"), HeaderValue::from_static("nosniff"));

    // Range: bytes=start-end
    let mut start: u64 = 0;
    let mut end: u64 = total.saturating_sub(1);
    let mut partial = false;

    if let Some(rh) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        if let Some(rest) = rh.strip_prefix("bytes=") {
            if let Some((a, b)) = rest.split_once('-') {
                let a = a.trim();
                let b = b.trim();

                if !a.is_empty() {
                    if let Ok(v) = a.parse::<u64>() {
                        start = v;
                        partial = true;
                    }
                } else if !b.is_empty() {
                    // suffix range: bytes=-N
                    if let Ok(v) = b.parse::<u64>() {
                        if v > 0 {
                            let v = v.min(total);
                            start = total.saturating_sub(v);
                            partial = true;
                        }
                    }
                }

                if !b.is_empty() && !a.is_empty() {
                    if let Ok(v) = b.parse::<u64>() {
                        end = v;
                        partial = true;
                    }
                }

                if end >= total {
                    end = total.saturating_sub(1);
                }
            }
        }
    }

    if start >= total {
        let cr_value = format!("bytes */{}", total);
        let cr = HeaderValue::from_str(&cr_value).unwrap_or_else(|_| HeaderValue::from_static("bytes */0"));
        return (
            StatusCode::RANGE_NOT_SATISFIABLE,
            [
                (header::CONTENT_RANGE, cr),
                (header::CONTENT_TYPE, ct),
                (header::CONTENT_DISPOSITION, cd),
                nosniff,
                accept_ranges,
            ],
        )
            .into_response();
    }

    if start > end {
        start = 0;
        end = total.saturating_sub(1);
        partial = false;
    }

    let len = end - start + 1;

    let mut file = match File::open(&storage_path).await {
        Ok(f) => f,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    if partial {
        if file.seek(SeekFrom::Start(start)).await.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    let limited = file.take(len);
    let body = axum::body::Body::from_stream(ReaderStream::new(limited));

    if partial {
        let cr_value = format!("bytes {}-{}/{}", start, end, total);
        let cr = HeaderValue::from_str(&cr_value).unwrap_or_else(|_| HeaderValue::from_static("bytes 0-0/0"));
        let cl = HeaderValue::from_str(&len.to_string()).unwrap_or_else(|_| HeaderValue::from_static("0"));

        (
            StatusCode::PARTIAL_CONTENT,
            [
                (header::CONTENT_TYPE, ct),
                (header::CONTENT_DISPOSITION, cd),
                (header::CONTENT_RANGE, cr),
                (header::CONTENT_LENGTH, cl),
                nosniff,
                accept_ranges,
            ],
            body,
        )
            .into_response()
    } else {
        let cl = HeaderValue::from_str(&total.to_string()).unwrap_or_else(|_| HeaderValue::from_static("0"));
        (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, ct),
                (header::CONTENT_DISPOSITION, cd),
                (header::CONTENT_LENGTH, cl),
                nosniff,
                accept_ranges,
            ],
            body,
        )
            .into_response()
    }
}


#[derive(Serialize)]
struct ArchiveEntry {
    path: String,
    size: u64,
    is_dir: bool,
}

async fn get_archive(
    State(st): State<AppState>,
    me: AuthUser,
    Path(file_id): Path<i64>,
) -> impl IntoResponse {
    #[derive(sqlx::FromRow)]
    struct FileData {
        original_name: String,
        mime_type: String,
        storage_path: String,
        chat_id: i64,
    }

    let row: Option<FileData> = sqlx::query_as(
        "SELECT original_name, mime_type, storage_path, chat_id FROM files WHERE id = ?",
    )
    .bind(file_id)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten();

    let Some(f) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    // Access check (DM -> chat_participants; server chat -> server_members)
    #[derive(sqlx::FromRow)]
    struct ChatInfo {
        server_id: Option<i64>,
        is_private: i64,
    }

    let chat: Option<ChatInfo> = sqlx::query_as(
        "SELECT server_id, is_private FROM chats WHERE id = ?",
    )
    .bind(f.chat_id)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten();

    let Some(chat) = chat else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let member = if chat.is_private != 0 {
        sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM chat_participants WHERE chat_id = ? AND user_id = ? LIMIT 1",
        )
        .bind(f.chat_id)
        .bind(me.id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten()
        .is_some()
    } else if let Some(server_id) = chat.server_id {
        sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM server_members WHERE server_id = ? AND user_id = ? LIMIT 1",
        )
        .bind(server_id)
        .bind(me.id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten()
        .is_some()
    } else {
        false
    };

    if !member {
        return StatusCode::FORBIDDEN.into_response();
    }

    let name_l = f.original_name.to_ascii_lowercase();
    let mime_l = f.mime_type.to_ascii_lowercase();
    let is_zip = mime_l == "application/zip"
        || mime_l == "application/x-zip-compressed"
        || name_l.ends_with(".zip");

    if !is_zip {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }

    let storage_path = std::path::PathBuf::from(f.storage_path);

    let out = tokio::task::spawn_blocking(move || -> Result<Vec<ArchiveEntry>, StatusCode> {
        let file = std::fs::File::open(&storage_path).map_err(|_| StatusCode::NOT_FOUND)?;
        let mut zip = zip::ZipArchive::new(file).map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;

        let mut entries: Vec<ArchiveEntry> = Vec::new();
        let limit = 5000usize;
        let len = zip.len();
        let take = std::cmp::min(len, limit);
        let bs = std::path::MAIN_SEPARATOR;

        for i in 0..take {
            let zf = zip.by_index(i).map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
            let mut p = zf.name().to_string();
            if p.is_empty() {
                continue;
            }

            // normalize separators for the UI tree
            p = p.replace(bs, "/");

            // prevent pathological names
            if p.len() > 512 {
                p.truncate(512);
            }

            let is_dir = zf.is_dir() || p.ends_with('/');
            let size = if is_dir { 0 } else { zf.size() };

            entries.push(ArchiveEntry {
                path: p,
                size,
                is_dir,
            });
        }

        Ok(entries)
    })
    .await;

    match out {
        Ok(Ok(entries)) => (StatusCode::OK, axum::Json(entries)).into_response(),
        Ok(Err(code)) => code.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub(crate) async fn get_preview(
    State(st): State<AppState>,
    me: Option<AuthUser>,
    Path(file_id): Path<i64>,
    Query(q): Query<DlQuery>,
) -> impl IntoResponse {
    #[derive(sqlx::FromRow)]
    struct FileData {
        filename: String,
        mime_type: String,
        storage_path: String,
        chat_id: i64,
    }

    let row: Option<FileData> = sqlx::query_as(
        "SELECT filename, mime_type, storage_path, chat_id FROM files WHERE id = ?",
    )
    .bind(file_id)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten();

    let Some(f) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let uid = match resolve_user_id_for_file_request(&st, me.as_ref(), file_id, q.dl.as_deref()).await {
        Ok(v) => v,
        Err(code) => return code.into_response(),
    };

    if !can_access_chat_by_user_id(&st, uid, f.chat_id).await {
        return StatusCode::FORBIDDEN.into_response();
    };

    if !is_raster_image(&f.mime_type) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let thumb_path = thumb_path_for(&f.filename);
    if fs::metadata(&thumb_path).await.is_err() {
        // legacy / missing thumb -> best-effort generate on demand
        let orig = PathBuf::from(&f.storage_path);
        let _ = ensure_thumbnail(&orig, &f.filename, &f.mime_type).await;
    }

    let file = match File::open(&thumb_path).await {
        Ok(f) => f,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let body = axum::body::Body::from_stream(ReaderStream::new(file));
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("image/png")),
            (header::HeaderName::from_static("x-content-type-options"), HeaderValue::from_static("nosniff")),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=31536000, immutable"),
            ),
        ],
        body,
    )
        .into_response()
}