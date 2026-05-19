use axum::{
    extract::{Multipart, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use std::{collections::HashSet, io::Cursor, io::SeekFrom, path::{Path as FsPath, PathBuf}};
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
const TEMP_FILE_EXPIRES_SQL_MODIFIER: &str = "+24 hours";
const ORPHAN_CLEANUP_GRACE_SECS: u64 = 60 * 60;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/:file_id/link", get(get_file_link))
.route("/:file_id/preview", get(get_preview))
        .route("/:file_id/archive", get(get_archive))
        .route("/:file_id/raw", get(get_file_raw))
        .route("/", post(upload_file))
        .route("/:file_id", get(get_file))
}

fn upload_json_error(status: StatusCode, detail: &'static str) -> axum::response::Response {
    (status, Json(serde_json::json!({ "detail": detail }))).into_response()
}

fn upload_json_error_with_message(status: StatusCode, detail: &'static str, message: String) -> axum::response::Response {
    (status, Json(serde_json::json!({ "detail": detail, "message": message }))).into_response()
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

    if let Some(server_id) = chat.server_id.filter(|sid| *sid > 0) {
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
        // Support both current private chats and legacy DM rows with server_id NULL/0.
        let in_participants = sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM chat_participants WHERE chat_id = ? AND user_id = ? LIMIT 1",
        )
        .bind(chat_id)
        .bind(user_id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten()
        .is_some();

        if in_participants {
            true
        } else {
            sqlx::query_scalar::<_, i64>(
                "SELECT 1 FROM dm_chats WHERE chat_id = ? AND (user_a = ? OR user_b = ?) LIMIT 1",
            )
            .bind(chat_id)
            .bind(user_id)
            .bind(user_id)
            .fetch_optional(&st.db)
            .await
            .ok()
            .flatten()
            .is_some()
        }
    }
}


#[derive(sqlx::FromRow)]
struct FileServeRow {
    filename: String,
    original_name: String,
    mime_type: String,
    storage_path: String,
    chat_id: i64,
    is_expired: i64,
}

#[derive(sqlx::FromRow)]
struct FileAccessRow {
    filename: String,
    storage_path: String,
    chat_id: i64,
    is_expired: i64,
}

#[derive(sqlx::FromRow)]
struct ExpiredFileRow {
    id: i64,
    filename: String,
    storage_path: String,
    chat_id: i64,
}

async fn temporary_file_expires_at(st: &AppState) -> Result<String, StatusCode> {
    sqlx::query_scalar::<_, String>(
        "SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?)",
    )
    .bind(TEMP_FILE_EXPIRES_SQL_MODIFIER)
    .fetch_one(&st.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn sqlite_now_iso(st: &AppState) -> Result<String, StatusCode> {
    sqlx::query_scalar::<_, String>("SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now')")
        .fetch_one(&st.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(crate) fn thumb_path_for(stored_filename: &str) -> PathBuf {
    let thumbs_dir = PathBuf::from("storage/files/thumbs");
    let stem = std::path::Path::new(stored_filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(stored_filename);
    thumbs_dir.join(format!("{}.png", stem))
}

fn thumb_candidate_paths(stored_filename: &str, storage_path: Option<&str>) -> Vec<PathBuf> {
    let thumbs_dir = PathBuf::from("storage/files/thumbs");
    let mut stems: Vec<String> = Vec::new();

    let mut push_stem = |value: &str| {
        let stem = std::path::Path::new(value)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(value)
            .trim()
            .to_string();

        if !stem.is_empty() && !stems.iter().any(|x| x == &stem) {
            stems.push(stem);
        }
    };

    push_stem(stored_filename);

    if let Some(path) = storage_path {
        if let Some(name) = std::path::Path::new(path).file_name().and_then(|s| s.to_str()) {
            push_stem(name);
        }
    }

    let mut out = Vec::new();
    for stem in stems {
        for ext in ["png", "webp", "jpg", "jpeg"] {
            out.push(thumbs_dir.join(format!("{}.{}", stem, ext)));
        }
    }

    out
}

async fn existing_thumb_path_for(stored_filename: &str, storage_path: Option<&str>) -> Option<PathBuf> {
    for path in thumb_candidate_paths(stored_filename, storage_path) {
        if fs::metadata(&path).await.map(|m| m.is_file() && m.len() > 0).unwrap_or(false) {
            return Some(path);
        }
    }

    None
}

async fn active_ref_count_by_storage_path(st: &AppState, storage_path: &str) -> i64 {
    let file_refs = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(1)
        FROM files
        WHERE storage_path = ?
          AND deleted_at IS NULL
          AND (expires_at IS NULL OR expires_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
        "#,
    )
    .bind(storage_path)
    .fetch_one(&st.db)
    .await
    .unwrap_or(0);

    let gif_refs = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(1) FROM gif_assets WHERE storage_path = ?",
    )
    .bind(storage_path)
    .fetch_one(&st.db)
    .await
    .unwrap_or(0);

    file_refs + gif_refs
}

async fn active_ref_count_by_filename(st: &AppState, filename: &str) -> i64 {
    let file_refs = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(1)
        FROM files
        WHERE filename = ?
          AND deleted_at IS NULL
          AND (expires_at IS NULL OR expires_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
        "#,
    )
    .bind(filename)
    .fetch_one(&st.db)
    .await
    .unwrap_or(0);

    let gif_refs = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(1) FROM gif_assets WHERE filename = ?",
    )
    .bind(filename)
    .fetch_one(&st.db)
    .await
    .unwrap_or(0);

    file_refs + gif_refs
}

pub(crate) async fn cleanup_file_artifacts_if_unreferenced(
    st: &AppState,
    storage_path: &str,
    filename: &str,
) {
    if !storage_path.trim().is_empty() && active_ref_count_by_storage_path(st, storage_path).await == 0 {
        let _ = fs::remove_file(PathBuf::from(storage_path)).await;
    }

    if !filename.trim().is_empty() && active_ref_count_by_filename(st, filename).await == 0 {
        let _ = fs::remove_file(thumb_path_for(filename)).await;
    }
}

async fn mark_file_expired(st: &AppState, file_id: i64, storage_path: &str, filename: &str) {
    let deleted_at = sqlite_now_iso(st)
        .await
        .unwrap_or_else(|_| auth::now_iso());

    let _ = sqlx::query(
        r#"
        UPDATE files
        SET deleted_at = COALESCE(deleted_at, ?)
        WHERE id = ?
        "#,
    )
    .bind(deleted_at)
    .bind(file_id)
    .execute(&st.db)
    .await;

    cleanup_file_artifacts_if_unreferenced(st, storage_path, filename).await;
}

async fn old_enough_to_cleanup(path: &FsPath) -> bool {
    let Ok(meta) = fs::metadata(path).await else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return true;
    };
    match modified.elapsed() {
        Ok(elapsed) => elapsed.as_secs() >= ORPHAN_CLEANUP_GRACE_SECS,
        Err(_) => false,
    }
}

pub(crate) async fn cleanup_orphan_storage_files(st: &AppState) {
    #[derive(sqlx::FromRow)]
    struct ActivePathRow {
        id: i64,
        filename: String,
        storage_path: String,
        chat_id: i64,
        is_active: i64,
    }

    let rows: Vec<ActivePathRow> = sqlx::query_as(
        r#"
        SELECT
            id,
            filename,
            storage_path,
            chat_id,
            CASE
                WHEN deleted_at IS NULL
                 AND (expires_at IS NULL OR expires_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                THEN 1
                ELSE 0
            END AS is_active
        FROM files
        "#,
    )
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();

    let mut active_paths: HashSet<String> = HashSet::new();
    let mut active_thumbs: HashSet<String> = HashSet::new();

    for row in rows {
        if row.is_active != 0 {
            active_paths.insert(PathBuf::from(&row.storage_path).to_string_lossy().to_string());
            for thumb in thumb_candidate_paths(&row.filename, Some(&row.storage_path)) {
                active_thumbs.insert(thumb.to_string_lossy().to_string());
            }
            continue;
        }

        // Variant 2: old broken uploads may have lost the original but still have a useful thumbnail
        // referenced by a live message. Keep such thumbnails until the message is deleted.
        if message_id_referencing_file(st, row.id, row.chat_id).await.is_some() {
            for thumb in thumb_candidate_paths(&row.filename, Some(&row.storage_path)) {
                active_thumbs.insert(thumb.to_string_lossy().to_string());
            }
        }
    }

    let gif_rows = sqlx::query("SELECT filename, storage_path FROM gif_assets")
        .fetch_all(&st.db)
        .await
        .unwrap_or_default();
    for row in gif_rows {
        let filename: String = row.get("filename");
        let storage_path: String = row.get("storage_path");
        active_paths.insert(PathBuf::from(&storage_path).to_string_lossy().to_string());
        for thumb in thumb_candidate_paths(&filename, Some(&storage_path)) {
            active_thumbs.insert(thumb.to_string_lossy().to_string());
        }
    }

    let storage_dir = PathBuf::from("storage/files");
    if let Ok(mut dir) = fs::read_dir(&storage_dir).await {
        while let Ok(Some(entry)) = dir.next_entry().await {
            let path = entry.path();
            if path.is_dir() {
                continue;
            }
            if path.extension().and_then(|s| s.to_str()) == Some("uploading") {
                continue;
            }
            let key = path.to_string_lossy().to_string();
            if !active_paths.contains(&key) && old_enough_to_cleanup(&path).await {
                let _ = fs::remove_file(path).await;
            }
        }
    }

    let thumbs_dir = PathBuf::from("storage/files/thumbs");
    if let Ok(mut dir) = fs::read_dir(&thumbs_dir).await {
        while let Ok(Some(entry)) = dir.next_entry().await {
            let path = entry.path();
            if path.is_dir() {
                continue;
            }
            let key = path.to_string_lossy().to_string();
            if !active_thumbs.contains(&key) && old_enough_to_cleanup(&path).await {
                let _ = fs::remove_file(path).await;
            }
        }
    }
}

pub(crate) async fn cleanup_expired_files(st: &AppState) {
    let rows: Vec<ExpiredFileRow> = sqlx::query_as(
        r#"
        SELECT id, filename, storage_path, chat_id
        FROM files
        WHERE storage_kind = 'temporary'
          AND deleted_at IS NULL
          AND expires_at IS NOT NULL
          AND expires_at <= strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
        LIMIT 250
        "#,
    )
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();

    for row in rows {
        if heal_message_file_if_referenced(st, row.id, row.chat_id, &row.storage_path).await {
            continue;
        }
        if thumb_only_file_can_be_shown(st, row.id, row.chat_id, &row.filename, &row.storage_path).await {
            continue;
        }
        mark_file_expired(st, row.id, &row.storage_path, &row.filename).await;
    }

    cleanup_orphan_storage_files(st).await;
}

async fn message_id_referencing_file(st: &AppState, file_id: i64, chat_id: i64) -> Option<i64> {
    let pat_pipe = format!("%[[file:{}|%", file_id);
    let pat_eq = format!("%[[file={}|%", file_id);
    let pat_broken = format!("%[[file:{}]]%", file_id);

    sqlx::query_scalar(
        r#"
        SELECT id
        FROM messages
        WHERE chat_id = ?
          AND (content LIKE ? OR content LIKE ? OR content LIKE ?)
        ORDER BY id DESC
        LIMIT 1
        "#,
    )
    .bind(chat_id)
    .bind(&pat_pipe)
    .bind(&pat_eq)
    .bind(&pat_broken)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten()
}

async fn thumb_available_for(filename: &str, storage_path: Option<&str>) -> bool {
    existing_thumb_path_for(filename, storage_path).await.is_some()
}

async fn original_available_for(storage_path: &str) -> bool {
    fs::metadata(storage_path).await.map(|m| m.is_file() && m.len() > 0).unwrap_or(false)
}

async fn thumb_only_file_can_be_shown(st: &AppState, file_id: i64, chat_id: i64, filename: &str, storage_path: &str) -> bool {
    thumb_available_for(filename, Some(storage_path)).await && message_id_referencing_file(st, file_id, chat_id).await.is_some()
}

async fn heal_message_file_if_referenced(st: &AppState, file_id: i64, chat_id: i64, storage_path: &str) -> bool {
    // Старые загрузки могли остаться temporary и протухнуть, хотя маркер файла уже есть в сообщении.
    // Если физический файл ещё на месте и сообщение с этим маркером существует в том же чате —
    // переводим файл обратно в нормальное состояние message без удаления/перезаливки.
    if !original_available_for(storage_path).await {
        return false;
    }

    let Some(message_id) = message_id_referencing_file(st, file_id, chat_id).await else {
        return false;
    };

    sqlx::query(
        r#"
        UPDATE files
        SET message_id = ?,
            storage_kind = 'message',
            expires_at = NULL,
            deleted_at = NULL
        WHERE id = ?
          AND chat_id = ?
        "#,
    )
    .bind(message_id)
    .bind(file_id)
    .bind(chat_id)
    .execute(&st.db)
    .await
    .is_ok()
}

async fn load_file_for_serving(st: &AppState, file_id: i64) -> Result<FileServeRow, StatusCode> {
    let row: Option<FileServeRow> = sqlx::query_as(
        r#"
        SELECT
            filename,
            original_name,
            mime_type,
            storage_path,
            chat_id,
            CASE
                WHEN deleted_at IS NOT NULL THEN 1
                WHEN expires_at IS NOT NULL AND expires_at <= strftime('%Y-%m-%dT%H:%M:%SZ', 'now') THEN 1
                ELSE 0
            END AS is_expired
        FROM files
        WHERE id = ?
        "#,
    )
    .bind(file_id)
    .fetch_optional(&st.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let Some(row) = row else {
        return Err(StatusCode::NOT_FOUND);
    };

    if row.is_expired != 0 {
        if heal_message_file_if_referenced(st, file_id, row.chat_id, &row.storage_path).await {
            return Ok(row);
        }
        if thumb_only_file_can_be_shown(st, file_id, row.chat_id, &row.filename, &row.storage_path).await {
            return Ok(row);
        }
        mark_file_expired(st, file_id, &row.storage_path, &row.filename).await;
        return Err(StatusCode::GONE);
    }

    Ok(row)
}

async fn load_file_for_access(st: &AppState, file_id: i64) -> Result<FileAccessRow, StatusCode> {
    let row: Option<FileAccessRow> = sqlx::query_as(
        r#"
        SELECT
            filename,
            storage_path,
            chat_id,
            CASE
                WHEN deleted_at IS NOT NULL THEN 1
                WHEN expires_at IS NOT NULL AND expires_at <= strftime('%Y-%m-%dT%H:%M:%SZ', 'now') THEN 1
                ELSE 0
            END AS is_expired
        FROM files
        WHERE id = ?
        "#,
    )
    .bind(file_id)
    .fetch_optional(&st.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let Some(row) = row else {
        return Err(StatusCode::NOT_FOUND);
    };

    if row.is_expired != 0 {
        if heal_message_file_if_referenced(st, file_id, row.chat_id, &row.storage_path).await {
            return Ok(row);
        }
        if thumb_only_file_can_be_shown(st, file_id, row.chat_id, &row.filename, &row.storage_path).await {
            return Ok(row);
        }
        mark_file_expired(st, file_id, &row.storage_path, &row.filename).await;
        return Err(StatusCode::GONE);
    }

    Ok(row)
}

fn is_raster_image(mime: &str) -> bool {
    let m = mime.to_ascii_lowercase();
    m.starts_with("image/") && m != "image/svg+xml"
}

fn is_normalizable_raster_image(mime: &str) -> bool {
    matches!(
        mime.to_ascii_lowercase().as_str(),
        "image/jpeg" | "image/png" | "image/webp" | "image/bmp" | "image/tiff"
    )
}

async fn compute_normalized_image_hash(path: &FsPath, mime_type: &str) -> Option<String> {
    if !is_normalizable_raster_image(mime_type) {
        return None;
    }

    let img_path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Option<String> {
        let (w, h) = image::image_dimensions(&img_path).ok()?;
        let pixels = (w as u64).saturating_mul(h as u64);
        if w > MAX_IMAGE_DIM || h > MAX_IMAGE_DIM || pixels > MAX_IMAGE_PIXELS {
            return None;
        }

        let img = image::open(&img_path).ok()?;
        let rgba = img.to_rgba8();

        let mut hasher = Sha256::new();
        hasher.update(b"laberry-normalized-image-v1\\0");
        hasher.update(&rgba.width().to_be_bytes());
        hasher.update(&rgba.height().to_be_bytes());
        hasher.update(rgba.as_raw());
        Some(hasher.finalize_hex())
    })
    .await
    .ok()
    .flatten()
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

fn percent_encode_query_component(input: &str) -> String {
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

#[derive(Clone, Copy)]
struct UploadType {
    mime: &'static str,
    ext: &'static str,
}

fn safe_ext_from_name(name: &str) -> Option<String> {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|s| s.to_str())?
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();

    if ext.is_empty() || ext.len() > 16 {
        return None;
    }

    if !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }

    Some(ext)
}

fn known_type_from_ext(ext: &str) -> Option<UploadType> {
    match ext {
        "jpg" | "jpeg" => Some(UploadType { mime: "image/jpeg", ext: "jpg" }),
        "png" => Some(UploadType { mime: "image/png", ext: "png" }),
        "gif" => Some(UploadType { mime: "image/gif", ext: "gif" }),
        "webp" => Some(UploadType { mime: "image/webp", ext: "webp" }),
        "bmp" => Some(UploadType { mime: "image/bmp", ext: "bmp" }),
        "tif" | "tiff" => Some(UploadType { mime: "image/tiff", ext: "tiff" }),
        "svg" => Some(UploadType { mime: "image/svg+xml", ext: "svg" }),

        "mp4" | "m4v" => Some(UploadType { mime: "video/mp4", ext: "mp4" }),
        "mov" => Some(UploadType { mime: "video/quicktime", ext: "mov" }),
        "webm" => Some(UploadType { mime: "video/webm", ext: "webm" }),
        "avi" => Some(UploadType { mime: "video/x-msvideo", ext: "avi" }),
        "mkv" => Some(UploadType { mime: "video/x-matroska", ext: "mkv" }),

        "mp3" => Some(UploadType { mime: "audio/mpeg", ext: "mp3" }),
        "wav" => Some(UploadType { mime: "audio/wav", ext: "wav" }),
        "ogg" => Some(UploadType { mime: "audio/ogg", ext: "ogg" }),
        "flac" => Some(UploadType { mime: "audio/flac", ext: "flac" }),
        "m4a" => Some(UploadType { mime: "audio/mp4", ext: "m4a" }),

        "pdf" => Some(UploadType { mime: "application/pdf", ext: "pdf" }),
        "zip" => Some(UploadType { mime: "application/zip", ext: "zip" }),
        "rar" => Some(UploadType { mime: "application/vnd.rar", ext: "rar" }),
        "7z" => Some(UploadType { mime: "application/x-7z-compressed", ext: "7z" }),
        "apk" => Some(UploadType { mime: "application/vnd.android.package-archive", ext: "apk" }),
        "exe" => Some(UploadType { mime: "application/vnd.microsoft.portable-executable", ext: "exe" }),
        "dll" => Some(UploadType { mime: "application/vnd.microsoft.portable-executable", ext: "dll" }),
        "msi" => Some(UploadType { mime: "application/x-msi", ext: "msi" }),

        "txt" => Some(UploadType { mime: "text/plain", ext: "txt" }),
        "md" => Some(UploadType { mime: "text/markdown", ext: "md" }),
        "csv" => Some(UploadType { mime: "text/csv", ext: "csv" }),
        "json" => Some(UploadType { mime: "application/json", ext: "json" }),
        "log" => Some(UploadType { mime: "text/plain", ext: "log" }),
        "rs" => Some(UploadType { mime: "text/plain", ext: "rs" }),
        "js" => Some(UploadType { mime: "text/plain", ext: "js" }),
        "ts" => Some(UploadType { mime: "text/plain", ext: "ts" }),
        "html" | "htm" => Some(UploadType { mime: "text/plain", ext: "html" }),
        "css" => Some(UploadType { mime: "text/plain", ext: "css" }),
        "sql" => Some(UploadType { mime: "text/plain", ext: "sql" }),
        "ps1" => Some(UploadType { mime: "text/plain", ext: "ps1" }),
        "sh" => Some(UploadType { mime: "text/plain", ext: "sh" }),

        "doc" => Some(UploadType { mime: "application/msword", ext: "doc" }),
        "docx" => Some(UploadType { mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document", ext: "docx" }),
        "xls" => Some(UploadType { mime: "application/vnd.ms-excel", ext: "xls" }),
        "xlsx" => Some(UploadType { mime: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", ext: "xlsx" }),
        "ppt" => Some(UploadType { mime: "application/vnd.ms-powerpoint", ext: "ppt" }),
        "pptx" => Some(UploadType { mime: "application/vnd.openxmlformats-officedocument.presentationml.presentation", ext: "pptx" }),

        _ => None,
    }
}


fn is_generic_mime(mime: &str) -> bool {
    let m = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    m.is_empty()
        || m == "application/octet-stream"
        || m == "binary/octet-stream"
        || m == "application/download"
        || m == "application/force-download"
        || m == "unknown/unknown"
}

fn effective_mime_type(stored_mime: &str, original_name: &str, stored_filename: &str) -> String {
    let clean = stored_mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    // APK and Office-like formats are ZIP containers. Legacy rows may have
    // been stored as application/zip, so prefer the original extension there.
    if matches!(clean.as_str(), "application/zip" | "application/x-zip-compressed") {
        for name in [original_name, stored_filename] {
            if let Some(ext) = safe_ext_from_name(name) {
                if is_zip_container_ext(&ext) {
                    if let Some(t) = known_type_from_ext(&ext) {
                        return t.mime.to_string();
                    }
                }
            }
        }
    }

    // Normalize known stored MIME values first.
    if !is_generic_mime(&clean) {
        if let Some(t) = known_type_from_mime(&clean) {
            return t.mime.to_string();
        }
        return clean;
    }

    // Legacy rows may have application/octet-stream even for jpg/png/mp4.
    // Use the original filename first, then stored filename as a fallback.
    for name in [original_name, stored_filename] {
        if let Some(ext) = safe_ext_from_name(name) {
            if let Some(t) = known_type_from_ext(&ext) {
                return t.mime.to_string();
            }
        }
    }

    "application/octet-stream".to_string()
}

fn known_type_from_mime(mime: &str) -> Option<UploadType> {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    match mime.as_str() {
        "image/jpeg" | "image/jpg" => Some(UploadType { mime: "image/jpeg", ext: "jpg" }),
        "image/png" => Some(UploadType { mime: "image/png", ext: "png" }),
        "image/gif" => Some(UploadType { mime: "image/gif", ext: "gif" }),
        "image/webp" => Some(UploadType { mime: "image/webp", ext: "webp" }),
        "image/bmp" => Some(UploadType { mime: "image/bmp", ext: "bmp" }),
        "image/tiff" => Some(UploadType { mime: "image/tiff", ext: "tiff" }),
        "image/svg+xml" => Some(UploadType { mime: "image/svg+xml", ext: "svg" }),

        "video/mp4" => Some(UploadType { mime: "video/mp4", ext: "mp4" }),
        "video/quicktime" => Some(UploadType { mime: "video/quicktime", ext: "mov" }),
        "video/webm" => Some(UploadType { mime: "video/webm", ext: "webm" }),
        "video/x-msvideo" => Some(UploadType { mime: "video/x-msvideo", ext: "avi" }),
        "video/x-matroska" => Some(UploadType { mime: "video/x-matroska", ext: "mkv" }),

        "audio/mpeg" | "audio/mp3" => Some(UploadType { mime: "audio/mpeg", ext: "mp3" }),
        "audio/wav" | "audio/wave" | "audio/x-wav" => Some(UploadType { mime: "audio/wav", ext: "wav" }),
        "audio/ogg" => Some(UploadType { mime: "audio/ogg", ext: "ogg" }),
        "audio/flac" => Some(UploadType { mime: "audio/flac", ext: "flac" }),
        "audio/mp4" => Some(UploadType { mime: "audio/mp4", ext: "m4a" }),

        "application/pdf" => Some(UploadType { mime: "application/pdf", ext: "pdf" }),
        "application/zip" | "application/x-zip-compressed" => Some(UploadType { mime: "application/zip", ext: "zip" }),
        "application/vnd.android.package-archive" => Some(UploadType { mime: "application/vnd.android.package-archive", ext: "apk" }),
        "application/vnd.rar" | "application/x-rar-compressed" => Some(UploadType { mime: "application/vnd.rar", ext: "rar" }),
        "application/x-7z-compressed" => Some(UploadType { mime: "application/x-7z-compressed", ext: "7z" }),
        "application/vnd.microsoft.portable-executable" | "application/x-msdownload" => Some(UploadType { mime: "application/vnd.microsoft.portable-executable", ext: "exe" }),
        "application/x-msi" => Some(UploadType { mime: "application/x-msi", ext: "msi" }),

        "text/plain" => Some(UploadType { mime: "text/plain", ext: "txt" }),
        "text/markdown" => Some(UploadType { mime: "text/markdown", ext: "md" }),
        "text/csv" => Some(UploadType { mime: "text/csv", ext: "csv" }),
        "application/json" => Some(UploadType { mime: "application/json", ext: "json" }),

        "application/msword" => Some(UploadType { mime: "application/msword", ext: "doc" }),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => Some(UploadType { mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document", ext: "docx" }),
        "application/vnd.ms-excel" => Some(UploadType { mime: "application/vnd.ms-excel", ext: "xls" }),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => Some(UploadType { mime: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", ext: "xlsx" }),
        "application/vnd.ms-powerpoint" => Some(UploadType { mime: "application/vnd.ms-powerpoint", ext: "ppt" }),
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => Some(UploadType { mime: "application/vnd.openxmlformats-officedocument.presentationml.presentation", ext: "pptx" }),

        _ => None,
    }
}

fn is_zip_container_ext(ext: &str) -> bool {
    matches!(
        ext,
        "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp" | "jar" | "apk" | "epub"
    )
}

fn sniff_magic_type(head: &[u8], original_ext: Option<&str>) -> Option<UploadType> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(UploadType { mime: "image/png", ext: "png" });
    }

    if head.len() >= 3 && head[0] == 0xff && head[1] == 0xd8 && head[2] == 0xff {
        return Some(UploadType { mime: "image/jpeg", ext: "jpg" });
    }

    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Some(UploadType { mime: "image/gif", ext: "gif" });
    }

    if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        return Some(UploadType { mime: "image/webp", ext: "webp" });
    }

    if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WAVE" {
        return Some(UploadType { mime: "audio/wav", ext: "wav" });
    }

    if head.starts_with(b"%PDF-") {
        return Some(UploadType { mime: "application/pdf", ext: "pdf" });
    }

    if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") || head.starts_with(b"PK\x07\x08") {
        if let Some(ext) = original_ext {
            if is_zip_container_ext(ext) {
                if let Some(t) = known_type_from_ext(ext) {
                    return Some(t);
                }
            }
        }
        return Some(UploadType { mime: "application/zip", ext: "zip" });
    }

    if head.starts_with(b"Rar!\x1a\x07\x00") || head.starts_with(b"Rar!\x1a\x07\x01\x00") {
        return Some(UploadType { mime: "application/vnd.rar", ext: "rar" });
    }

    if head.starts_with(b"7z\xbc\xaf\x27\x1c") {
        return Some(UploadType { mime: "application/x-7z-compressed", ext: "7z" });
    }

    if head.starts_with(b"ID3") || (head.len() >= 2 && head[0] == 0xff && (head[1] & 0xe0) == 0xe0) {
        return Some(UploadType { mime: "audio/mpeg", ext: "mp3" });
    }

    if head.starts_with(b"OggS") {
        return Some(UploadType { mime: "audio/ogg", ext: "ogg" });
    }

    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        let brand = &head[8..12];
        if brand == b"qt  " {
            return Some(UploadType { mime: "video/quicktime", ext: "mov" });
        }
        return Some(UploadType { mime: "video/mp4", ext: "mp4" });
    }

    if head.starts_with(b"\x1a\x45\xdf\xa3") {
        return Some(UploadType { mime: "video/webm", ext: "webm" });
    }

    if head.starts_with(b"MZ") {
        return Some(UploadType { mime: "application/vnd.microsoft.portable-executable", ext: "exe" });
    }

    None
}

fn looks_like_plain_text(head: &[u8]) -> bool {
    if head.is_empty() {
        return false;
    }

    if std::str::from_utf8(head).is_err() {
        return false;
    }

    // Разрешаем обычные пробельные символы, режем бинарный мусор.
    !head.iter().any(|b| {
        (*b < 0x20 && !matches!(*b, b'\n' | b'\r' | b'\t')) || *b == 0x7f
    })
}

fn detect_upload_type(head: &[u8], original_name: &str, provided_mime: &str) -> (String, String) {
    let original_ext = safe_ext_from_name(original_name);

    if let Some(t) = sniff_magic_type(head, original_ext.as_deref()) {
        return (t.mime.to_string(), t.ext.to_string());
    }

    // For text-ish files the extension is more trustworthy than a browser-provided
    // fallback MIME like text/plain or application/octet-stream. This keeps
    // message.md stored as Markdown instead of silently becoming .txt/.bin.
    if let Some(ext) = original_ext.as_deref() {
        if matches!(ext, "md" | "markdown" | "txt" | "json" | "csv" | "log" | "rs" | "js" | "ts" | "html" | "css" | "sql" | "ps1" | "sh") {
            if ext == "markdown" {
                return ("text/markdown".to_string(), "md".to_string());
            }
            if let Some(t) = known_type_from_ext(ext) {
                return (t.mime.to_string(), t.ext.to_string());
            }
            return ("text/plain".to_string(), ext.to_string());
        }
    }

    if let Some(t) = known_type_from_mime(provided_mime) {
        return (t.mime.to_string(), t.ext.to_string());
    }

    if let Some(ext) = original_ext.as_deref() {
        if let Some(t) = known_type_from_ext(ext) {
            return (t.mime.to_string(), t.ext.to_string());
        }
    }

    if looks_like_plain_text(head) {
        return ("text/plain".to_string(), "txt".to_string());
    }

    ("application/octet-stream".to_string(), "bin".to_string())
}

struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffer_len: usize,
    len_bytes: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            state: [
                0x6a09e667,
                0xbb67ae85,
                0x3c6ef372,
                0xa54ff53a,
                0x510e527f,
                0x9b05688c,
                0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: [0u8; 64],
            buffer_len: 0,
            len_bytes: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.len_bytes = self.len_bytes.wrapping_add(data.len() as u64);

        while !data.is_empty() {
            let free = 64 - self.buffer_len;
            let take = free.min(data.len());
            self.buffer[self.buffer_len..self.buffer_len + take]
                .copy_from_slice(&data[..take]);
            self.buffer_len += take;
            data = &data[take..];

            if self.buffer_len == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffer_len = 0;
            }
        }
    }

    fn finalize(mut self) -> [u8; 32] {
        let bit_len = self.len_bytes.wrapping_mul(8);
        self.update(&[0x80]);

        let zeros = [0u8; 64];
        while self.buffer_len != 56 {
            if self.buffer_len > 56 {
                let need = 64 - self.buffer_len;
                self.update(&zeros[..need]);
            } else {
                let need = 56 - self.buffer_len;
                self.update(&zeros[..need]);
            }
        }

        self.update(&bit_len.to_be_bytes());

        let mut out = [0u8; 32];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn finalize_hex(self) -> String {
        let digest = self.finalize();
        let mut s = String::with_capacity(64);
        for b in digest {
            let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{:02x}", b));
        }
        s
    }

    fn compress(&mut self, block: &[u8; 64]) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5,
            0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
            0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
            0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
            0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
            0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
            0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
            0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
            0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
            0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
            0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
            0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5,
            0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
            0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
        ];

        let mut w = [0u32; 64];
        for i in 0..16 {
            let j = i * 4;
            w[i] = u32::from_be_bytes([block[j], block[j + 1], block[j + 2], block[j + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = self.state[0];
        let mut b = self.state[1];
        let mut c = self.state[2];
        let mut d = self.state[3];
        let mut e = self.state[4];
        let mut f = self.state[5];
        let mut g = self.state[6];
        let mut h = self.state[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

async fn upload_file(
    State(st): State<AppState>,
    me: AuthUser,
    mut multipart: Multipart,
) -> impl IntoResponse {
    cleanup_expired_files(&st).await;

    let storage_dir = PathBuf::from("storage/files");
    if let Err(e) = fs::create_dir_all(&storage_dir).await {
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string());
        let msg = format!("{}; cwd={}; path={}", e, cwd, storage_dir.to_string_lossy());
        eprintln!("[FILES] storage_create_failed: {}", msg);
        return upload_json_error_with_message(StatusCode::INTERNAL_SERVER_ERROR, "storage_create_failed", msg);
    }

    let mut chat_id: Option<i64> = None;
    let mut original_name = "file.bin".to_string();
    let mut provided_mime = "application/octet-stream".to_string();

    let mut temp_path: Option<PathBuf> = None;
    let mut file_size: i64 = 0;
    let mut head: Vec<u8> = Vec::with_capacity(512);
    let mut hasher = Sha256::new();
    let mut got_file = false;

    loop {
        let next_field = match multipart.next_field().await {
            Ok(v) => v,
            Err(_) => {
                if let Some(p) = temp_path.as_ref() {
                    let _ = fs::remove_file(p).await;
                }
                return StatusCode::BAD_REQUEST.into_response();
            }
        };

        let Some(mut field) = next_field else {
            break;
        };

        match field.name().unwrap_or("") {
            "chat_id" => {
                if let Ok(txt) = field.text().await {
                    chat_id = txt.parse::<i64>().ok();
                }
            }

            "file" => {
                if got_file {
                    if let Some(p) = temp_path.as_ref() {
                        let _ = fs::remove_file(p).await;
                    }
                    return StatusCode::BAD_REQUEST.into_response();
                }

                got_file = true;
                original_name = field.file_name().unwrap_or("file").to_string();
                provided_mime = field
                    .content_type()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "application/octet-stream".to_string());

                let temp_filename = format!("{}.uploading", Uuid::new_v4());
                let path = storage_dir.join(&temp_filename);
                temp_path = Some(path.clone());

                let mut file = match fs::File::create(&path).await {
                    Ok(f) => f,
                    Err(e) => return upload_json_error_with_message(StatusCode::INTERNAL_SERVER_ERROR, "temp_file_create_failed", e.to_string()),
                };

                loop {
                    let chunk = match field.chunk().await {
                        Ok(Some(chunk)) => chunk,
                        Ok(None) => break,
                        Err(_) => {
                            let _ = fs::remove_file(&path).await;
                            return StatusCode::BAD_REQUEST.into_response();
                        }
                    };

                    file_size += chunk.len() as i64;
                    hasher.update(&chunk);
                    if file_size > MAX_UPLOAD_BYTES {
                        let _ = fs::remove_file(&path).await;
                        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
                    }

                    if head.len() < 512 {
                        let need = 512usize.saturating_sub(head.len());
                        let take = std::cmp::min(need, chunk.len());
                        head.extend_from_slice(&chunk[..take]);
                    }

                    if file.write_all(&chunk).await.is_err() {
                        let _ = fs::remove_file(&path).await;
                        return upload_json_error(StatusCode::INTERNAL_SERVER_ERROR, "temp_file_write_failed");
                    }
                }

                if file.flush().await.is_err() {
                    let _ = fs::remove_file(&path).await;
                    return upload_json_error(StatusCode::INTERNAL_SERVER_ERROR, "temp_file_flush_failed");
                }
            }
            _ => {}
        }
    }

    let chat_id = match chat_id {
        Some(v) => v,
        None => {
            if let Some(p) = temp_path.as_ref() {
                let _ = fs::remove_file(p).await;
            }
            return StatusCode::BAD_REQUEST.into_response();
        }
    };

    let upload_temp_path = match temp_path.take() {
        Some(p) => p,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };

    if file_size <= 0 {
        let _ = fs::remove_file(&upload_temp_path).await;
        return StatusCode::BAD_REQUEST.into_response();
    }

    // Проверка доступа: приватный чат -> chat_participants; серверный канал -> server_members
    // + voice chat: only while user is in this voice channel
    if !can_access_chat_by_user_id(&st, me.id, chat_id).await {
        let _ = fs::remove_file(&upload_temp_path).await;
        return StatusCode::FORBIDDEN.into_response();
    }

    // MIME/extension берём не только от клиента.
    // Это чинит "белые файлы" без расширения и снижает риск неверного Content-Type.
    let (mime_type, ext) = detect_upload_type(&head, &original_name, &provided_mime);
    let content_hash = hasher.finalize_hex();
    let normalized_hash = compute_normalized_image_hash(&upload_temp_path, &mime_type).await;

    #[derive(Clone, sqlx::FromRow)]
    struct DuplicateFileRow {
        id: i64,
        filename: String,
        storage_path: String,
    }

    let mut duplicate_by: Option<&'static str> = None;
    let mut raw_dedupe_allowed = false;

    let mut duplicate: Option<DuplicateFileRow> = sqlx::query_as(
        r#"
        SELECT id, filename, storage_path
        FROM files
        WHERE content_hash = ?
          AND file_size = ?
          AND deleted_at IS NULL
          AND (expires_at IS NULL OR expires_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
        ORDER BY id ASC
        LIMIT 1
        "#,
    )
    .bind(&content_hash)
    .bind(file_size)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten();

    if let Some(d) = duplicate.as_ref() {
        if fs::metadata(&d.storage_path).await.is_ok() {
            duplicate_by = Some("content_hash");
            raw_dedupe_allowed = true;
        } else {
            mark_file_expired(&st, d.id, &d.storage_path, &d.filename).await;
            duplicate = None;
        }
    }

    if duplicate.is_none() {
        if let Some(nh) = normalized_hash.as_ref() {
            duplicate = sqlx::query_as(
                r#"
                SELECT id, filename, storage_path
                FROM files
                WHERE normalized_hash = ?
                  AND mime_type = ?
                  AND deleted_at IS NULL
                  AND (expires_at IS NULL OR expires_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                ORDER BY id ASC
                LIMIT 1
                "#,
            )
            .bind(nh)
            .bind(&mime_type)
            .fetch_optional(&st.db)
            .await
            .ok()
            .flatten();

            if let Some(d) = duplicate.as_ref() {
                if fs::metadata(&d.storage_path).await.is_ok() {
                    duplicate_by = Some("normalized_hash");
                    // Важно: raw-файл по normalized_hash не hard-link'аем.
                    // Иначе можно случайно выдать чужие EXIF/метаданные из первой копии.
                    raw_dedupe_allowed = false;
                } else {
                    mark_file_expired(&st, d.id, &d.storage_path, &d.filename).await;
                    duplicate = None;
                }
            }
        }
    }

    let duplicate_of = duplicate.as_ref().map(|d| d.id);

    // Для точного совпадения по content_hash реально объединяем кеш:
    // новая запись в БД указывает на уже существующий физический файл.
    // В storage/files не появляется второй .exe/.bin/.png и т.п.
    let (final_filename, final_path, raw_shared) = if raw_dedupe_allowed {
        if let Some(d) = duplicate.as_ref() {
            let _ = fs::remove_file(&upload_temp_path).await;
            (d.filename.clone(), PathBuf::from(&d.storage_path), true)
        } else {
            let name = format!("{}.{}", Uuid::new_v4(), ext);
            let path = storage_dir.join(&name);
            if fs::rename(&upload_temp_path, &path).await.is_err() {
                let _ = fs::remove_file(&upload_temp_path).await;
                return upload_json_error(StatusCode::INTERNAL_SERVER_ERROR, "storage_rename_failed");
            }
            (name, path, false)
        }
    } else {
        let name = format!("{}.{}", Uuid::new_v4(), ext);
        let path = storage_dir.join(&name);
        if fs::rename(&upload_temp_path, &path).await.is_err() {
            let _ = fs::remove_file(&upload_temp_path).await;
            return upload_json_error(StatusCode::INTERNAL_SERVER_ERROR, "storage_rename_failed");
        }
        (name, path, false)
    };

    if is_raster_image(&mime_type) {
        let final_thumb = thumb_path_for(&final_filename);
        let mut thumb_reused = raw_shared;

        if !thumb_reused {
            if let Some(d) = duplicate.as_ref() {
                let duplicate_thumb = thumb_path_for(&d.filename);
                if fs::metadata(&duplicate_thumb).await.is_ok() {
                    if let Some(parent) = final_thumb.parent() {
                        let _ = fs::create_dir_all(parent).await;
                    }
                    thumb_reused = std::fs::hard_link(&duplicate_thumb, &final_thumb).is_ok();
                    if !thumb_reused {
                        thumb_reused = fs::copy(&duplicate_thumb, &final_thumb).await.is_ok();
                    }
                }
            }
        }

        if !thumb_reused {
            // Защита от DoS картинками: ограничиваем мегапиксели + делаем превью.
            if let Err(code) = ensure_thumbnail(&final_path, &final_filename, &mime_type).await {
                let _ = fs::remove_file(&final_path).await;
                let _ = fs::remove_file(&final_thumb).await;
                return code.into_response();
            }
        }
    }

    let path = final_path;
    let stored_filename = final_filename;

    let created_at = auth::now_iso();
    let expires_at = match temporary_file_expires_at(&st).await {
        Ok(v) => v,
        Err(code) => {
            let _ = fs::remove_file(&path).await;
            let _ = fs::remove_file(thumb_path_for(&stored_filename)).await;
            return upload_json_error(code, "expires_at_failed");
        }
    };
    let r = sqlx::query(
        r#"
        INSERT INTO files (
            filename, original_name, file_size, mime_type,
            storage_path, uploaded_by, chat_id, created_at,
            content_hash, normalized_hash, content_hash_algo, storage_kind, expires_at
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'sha256', 'temporary', ?)
        "#,
    )
    .bind(&stored_filename)
    .bind(&original_name)
    .bind(file_size)
    .bind(&mime_type)
    .bind(path.to_string_lossy().to_string())
    .bind(me.id)
    .bind(chat_id)
    .bind(&created_at)
    .bind(&content_hash)
    .bind(normalized_hash.as_deref())
    .bind(&expires_at)
    .execute(&st.db)
    .await;

    let r = match r {
        Ok(v) => v,
        Err(e) => {
            let err_text = e.to_string();
            eprintln!("[FILES] upload db_insert_failed: {}", err_text);
            let _ = fs::remove_file(&path).await;
            let _ = fs::remove_file(thumb_path_for(&stored_filename)).await;
            return upload_json_error_with_message(StatusCode::INTERNAL_SERVER_ERROR, "db_insert_failed", err_text);
        }
    };

    let file_id = r.last_insert_rowid();

    (
        StatusCode::OK,
        axum::Json(serde_json::json!({
            "id": file_id,
            "expires_at": expires_at,
            "duplicate_of": duplicate_of,
            "duplicate_by": duplicate_by,
            "normalized_hash": normalized_hash.is_some(),
        })),
    )
        .into_response()
}

async fn get_file_link(
    State(st): State<AppState>,
    me: AuthUser,
    Path(file_id): Path<i64>,
) -> impl IntoResponse {
    let f = match load_file_for_access(&st, file_id).await {
        Ok(v) => v,
        Err(code) => return code.into_response(),
    };

    if !can_access_chat_by_user_id(&st, me.id, f.chat_id).await {
        return StatusCode::FORBIDDEN.into_response();
    }

    let original_available = original_available_for(&f.storage_path).await;
    let thumb_available = thumb_available_for(&f.filename, Some(&f.storage_path)).await;

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

    let dl_q = percent_encode_query_component(&dl);
    let download_url = if original_available {
        Some(format!("/api/files/{}?dl={}", file_id, dl_q))
    } else {
        None
    };
    let raw_url = if original_available {
        Some(format!("/api/files/{}/raw?dl={}", file_id, dl_q))
    } else {
        None
    };

    let resp = serde_json::json!({
        "download_url": download_url,
        "raw_url": raw_url,
        "preview_url": format!("/api/files/{}/preview?dl={}", file_id, dl_q),
        "expires_in_sec": ttl,
        "original_available": original_available,
        "thumb_available": thumb_available,
        "download_available": original_available,
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
    let f = match load_file_for_serving(&st, file_id).await {
        Ok(v) => v,
        Err(code) => return code.into_response(),
    };

    let uid = match resolve_user_id_for_file_request(&st, me.as_ref(), file_id, q.dl.as_deref()).await {
        Ok(v) => v,
        Err(code) => return code.into_response(),
    };

    if !can_access_chat_by_user_id(&st, uid, f.chat_id).await {
        return StatusCode::FORBIDDEN.into_response();
    }

    let effective_mime = effective_mime_type(&f.mime_type, &f.original_name, &f.filename);
    let storage_path = PathBuf::from(f.storage_path);
    serve_file_with_range(storage_path, &effective_mime, &f.original_name, false, headers).await
}


pub(crate) async fn get_file_raw(
    State(st): State<AppState>,
    me: Option<AuthUser>,
    Path(file_id): Path<i64>,
    Query(q): Query<DlQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let f = match load_file_for_serving(&st, file_id).await {
        Ok(v) => v,
        Err(code) => return code.into_response(),
    };

    let uid = match resolve_user_id_for_file_request(&st, me.as_ref(), file_id, q.dl.as_deref()).await {
        Ok(v) => v,
        Err(code) => return code.into_response(),
    };

    if !can_access_chat_by_user_id(&st, uid, f.chat_id).await {
        return StatusCode::FORBIDDEN.into_response();
    }

    let effective_mime = effective_mime_type(&f.mime_type, &f.original_name, &f.filename);
    let storage_path = PathBuf::from(f.storage_path);
    serve_file_with_range(storage_path, &effective_mime, &f.original_name, true, headers).await
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
    let encoded_name = percent_encode_query_component(&safe_name);

    let inline = inline && inline_safe(mime_type);
    let cd_value = if inline {
        format!("inline; filename=\"{}\"; filename*=UTF-8''{}", safe_name, encoded_name)
    } else {
        format!("attachment; filename=\"{}\"; filename*=UTF-8''{}", safe_name, encoded_name)
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
    let f = match load_file_for_serving(&st, file_id).await {
        Ok(v) => v,
        Err(code) => return code.into_response(),
    };

    if !can_access_chat_by_user_id(&st, me.id, f.chat_id).await {
        return StatusCode::FORBIDDEN.into_response();
    }

    let name_l = f.original_name.to_ascii_lowercase();
    let effective_mime = effective_mime_type(&f.mime_type, &f.original_name, &f.filename);
    let mime_l = effective_mime.to_ascii_lowercase();
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
    let f = match load_file_for_serving(&st, file_id).await {
        Ok(v) => v,
        Err(code) => return code.into_response(),
    };

    let uid = match resolve_user_id_for_file_request(&st, me.as_ref(), file_id, q.dl.as_deref()).await {
        Ok(v) => v,
        Err(code) => return code.into_response(),
    };

    if !can_access_chat_by_user_id(&st, uid, f.chat_id).await {
        return StatusCode::FORBIDDEN.into_response();
    };

    let effective_mime = effective_mime_type(&f.mime_type, &f.original_name, &f.filename);
    if !is_raster_image(&effective_mime) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let original_available = original_available_for(&f.storage_path).await;
    if existing_thumb_path_for(&f.filename, Some(&f.storage_path)).await.is_none() && original_available {
        // Legacy / missing thumb -> best-effort generate on demand.
        // Use effective MIME so old application/octet-stream JPG/PNG rows still preview.
        let orig = PathBuf::from(&f.storage_path);
        let _ = ensure_thumbnail(&orig, &f.filename, &effective_mime).await;
    }

    let thumb_path = match existing_thumb_path_for(&f.filename, Some(&f.storage_path)).await {
        Some(path) => path,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    let file = match File::open(&thumb_path).await {
        Ok(f) => f,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let original_header = if original_available {
        HeaderValue::from_static("1")
    } else {
        HeaderValue::from_static("0")
    };

    let body = axum::body::Body::from_stream(ReaderStream::new(file));
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("image/png")),
            (header::HeaderName::from_static("x-content-type-options"), HeaderValue::from_static("nosniff")),
            (header::HeaderName::from_static("x-laberry-original-available"), original_header),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=31536000, immutable"),
            ),
        ],
        body,
    )
        .into_response()
}
