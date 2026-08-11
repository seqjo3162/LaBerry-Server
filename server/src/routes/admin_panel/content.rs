pub(super) use crate::auth;
pub(super) use crate::server::{AdminSession, AppState};

use axum::{
    extract::{Form, Path, Query, State, ConnectInfo},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    extract::Multipart,
};

use std::net::SocketAddr;
use sqlx::{Row, PgPool};
use std::path::PathBuf;
use tokio::fs;
use tokio::fs::File;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use super::{
    ActionForm, MsgQuery, admin_format_bytes, admin_redirect_with_msg, admin_sanitize_filename,
    embedded_page, escape_html, fmt_admin_dt, page, require_admin_panel_enabled, require_allow_ip,
    require_auth, safe_admin_return_to,
};

// =============================
// File serving (raw access)
// =============================

pub(super) async fn admin_file_raw(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(file_id): Path<i64>,
) -> impl IntoResponse {
    if let Err((code, msg)) = require_admin_panel_enabled() { return (code, msg).into_response(); }
    if let Err((code, msg)) = require_allow_ip(&st, &headers, Some(peer)) { return (code, msg).into_response(); }
    if let Err(redir) = require_auth(&st, &headers) { return redir.into_response(); }

    let row = sqlx::query(
        r#"
        SELECT original_name, mime_type, storage_path, file_size
        FROM files
        WHERE id = $1
        LIMIT 1
        "#,
    )
    .bind(file_id)
    .fetch_optional(&st.db)
    .await;

    let row = match row {
        Ok(Some(v)) => v,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("db_error: {e}")).into_response(),
    };

    let original_name: String = row.get("original_name");
    let mime_type: String = row.get("mime_type");
    let storage_path: String = row.get("storage_path");
    let file_size: i64 = row.get("file_size");

    let path = PathBuf::from(storage_path);
    let meta = match fs::metadata(&path).await {
        Ok(m) if m.is_file() && m.len() > 0 => m,
        _ => return (StatusCode::NOT_FOUND, "Файл отсутствует на диске").into_response(),
    };

    let file = match File::open(&path).await {
        Ok(f) => f,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("open_failed: {e}")).into_response(),
    };

    let safe_name = admin_sanitize_filename(&original_name);
    let ct = HeaderValue::from_str(mime_type.split(';').next().unwrap_or("application/octet-stream").trim())
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    let cd = HeaderValue::from_str(&format!("attachment; filename=\"{}\"", safe_name))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment"));
    let len_value = std::cmp::max(file_size, meta.len() as i64).to_string();
    let len = HeaderValue::from_str(&len_value).unwrap_or_else(|_| HeaderValue::from_static("0"));
    let body = axum::body::Body::from_stream(ReaderStream::new(file));

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, ct),
            (header::CONTENT_DISPOSITION, cd),
            (header::CONTENT_LENGTH, len),
            (header::HeaderName::from_static("x-content-type-options"), HeaderValue::from_static("nosniff")),
        ],
        body,
    ).into_response()
}

pub(super) async fn admin_profile_file_raw(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(file_id): Path<i64>,
) -> impl IntoResponse {
    if let Err((code, msg)) = require_admin_panel_enabled() { return (code, msg).into_response(); }
    if let Err((code, msg)) = require_allow_ip(&st, &headers, Some(peer)) { return (code, msg).into_response(); }
    if let Err(redir) = require_auth(&st, &headers) { return redir.into_response(); }

    let row = sqlx::query(
        r#"
        SELECT original_name, mime_type, storage_path, file_size
        FROM profile_files
        WHERE id = $1
        LIMIT 1
        "#,
    )
    .bind(file_id)
    .fetch_optional(&st.db)
    .await;

    let row = match row {
        Ok(Some(v)) => v,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("db_error: {e}")).into_response(),
    };

    let original_name: String = row.get("original_name");
    let mime_type: String = row.get("mime_type");
    let storage_path: String = row.get("storage_path");
    let file_size: i64 = row.get("file_size");

    let path = PathBuf::from(storage_path);
    let meta = match fs::metadata(&path).await {
        Ok(m) if m.is_file() && m.len() > 0 => m,
        _ => return (StatusCode::NOT_FOUND, "Файл отсутствует на диске").into_response(),
    };

    let file = match File::open(&path).await {
        Ok(f) => f,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("open_failed: {e}")).into_response(),
    };

    let safe_name = admin_sanitize_filename(&original_name);
    let ct = HeaderValue::from_str(mime_type.split(';').next().unwrap_or("application/octet-stream").trim())
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    let cd = HeaderValue::from_str(&format!("inline; filename=\"{}\"", safe_name))
        .unwrap_or_else(|_| HeaderValue::from_static("inline"));
    let len_value = std::cmp::max(file_size, meta.len() as i64).to_string();
    let len = HeaderValue::from_str(&len_value).unwrap_or_else(|_| HeaderValue::from_static("0"));
    let body = axum::body::Body::from_stream(ReaderStream::new(file));

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, ct),
            (header::CONTENT_DISPOSITION, cd),
            (header::CONTENT_LENGTH, len),
            (header::HeaderName::from_static("x-content-type-options"), HeaderValue::from_static("nosniff")),
        ],
        body,
    ).into_response()
}

pub(super) async fn admin_gif_raw(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(asset_id): Path<i64>,
) -> impl IntoResponse {
    if let Err((code, msg)) = require_admin_panel_enabled() { return (code, msg).into_response(); }
    if let Err((code, msg)) = require_allow_ip(&st, &headers, Some(peer)) { return (code, msg).into_response(); }
    if let Err(redir) = require_auth(&st, &headers) { return redir.into_response(); }

    let row = sqlx::query(
        r#"
        SELECT original_name, storage_path, file_size
        FROM gif_assets
        WHERE id = $1 AND scope = 'global'
        LIMIT 1
        "#,
    )
    .bind(asset_id)
    .fetch_optional(&st.db)
    .await;

    let row = match row {
        Ok(Some(v)) => v,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("db_error: {e}")).into_response(),
    };

    let original_name: String = row.get("original_name");
    let storage_path: String = row.get("storage_path");
    let file_size: i64 = row.get("file_size");
    let path = PathBuf::from(storage_path);
    let meta = match fs::metadata(&path).await {
        Ok(m) if m.is_file() && m.len() > 0 => m,
        _ => return (StatusCode::NOT_FOUND, "GIF отсутствует на диске").into_response(),
    };
    let file = match File::open(&path).await {
        Ok(f) => f,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("open_failed: {e}")).into_response(),
    };

    let safe_name = admin_sanitize_filename(&original_name);
    let cd = HeaderValue::from_str(&format!("inline; filename=\"{}\"", safe_name))
        .unwrap_or_else(|_| HeaderValue::from_static("inline"));
    let len_value = std::cmp::max(file_size, meta.len() as i64).to_string();
    let len = HeaderValue::from_str(&len_value).unwrap_or_else(|_| HeaderValue::from_static("0"));
    
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/gif")
        .header(header::CONTENT_DISPOSITION, cd)
        .header(header::CONTENT_LENGTH, len)
        .header(header::CACHE_CONTROL, "private, max-age=900")
        .header("x-content-type-options", "nosniff")
        .body(axum::body::Body::from_stream(ReaderStream::new(file)))
        .unwrap()
}

// =============================
// GIF management
// =============================

pub(super) async fn gifs_page(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<MsgQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let embedded = q.embed == Some(1);
    let return_to = if embedded { "/admin/gifs?embed=1" } else { "/admin/gifs" };

    let rows = fetch_admin_global_gifs(&st.db).await;

    let body = render_admin_gifs_panel_body(&sess, &rows, return_to);

    if embedded {
        embedded_page("Админка • GIF", &body, q.msg.as_deref()).into_response()
    } else {
        page("Админка • GIF", &body, q.msg.as_deref()).into_response()
    }
}

pub(super) struct AdminGifRow {
    id: i64,
    original_name: String,
    file_size: i64,
    created_at: String,
}

pub(super) async fn fetch_admin_global_gifs(db: &PgPool) -> Vec<AdminGifRow> {
    sqlx::query(
        r#"
        SELECT id, original_name, file_size, created_at
        FROM gif_assets
        WHERE scope = 'global'
        ORDER BY id DESC
        LIMIT 240
        "#,
    )
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|r| AdminGifRow {
        id: r.get("id"),
        original_name: r.get("original_name"),
        file_size: r.get("file_size"),
        created_at: r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
    })
    .collect()
}

pub(super) fn render_admin_gifs_panel_body(sess: &AdminSession, rows: &[AdminGifRow], return_to: &str) -> String {
    let cards = if rows.is_empty() {
        "<div class='empty-state'>Глобальных GIF пока нет. Загрузи первую анимацию слева.</div>".to_string()
    } else {
        rows.iter()
            .map(|r| {
                format!(
                    r#"<div class='admin-gif-card'>
  <div class='admin-gif-thumb'><img src='/admin/gifs/{id}/raw' alt='{name}' loading='lazy'></div>
  <div class='admin-gif-body'>
    <div class='admin-gif-name' title='{name}'>{name}</div>
    <div class='small'>{size} • {created_at}</div>
    <div class='admin-gif-actions'>
      <form method='post' action='/admin/gifs/{id}/delete'>
        <input type='hidden' name='csrf' value='{csrf}'>
        <input type='hidden' name='return_to' value='{return_to}'>
        <button type='submit' class='btn-danger'>Удалить</button>
      </form>
    </div>
  </div>
</div>"#,
                    id = r.id,
                    name = escape_html(&r.original_name),
                    size = escape_html(&admin_format_bytes(r.file_size)),
                    created_at = escape_html(&fmt_admin_dt(&r.created_at)),
                    csrf = escape_html(&sess.csrf),
                    return_to = escape_html(return_to),
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };

    format!(
        r#"<div class='admin-gif-shell'>
  <aside class='admin-gif-upload'>
    <h2>Глобальные GIF</h2>
    <p class='small'>Эти GIF видят все пользователи в пикере. Личные избранные пользователи добавляют сами из чата.</p>
    <form method='post' action='/admin/gifs/upload' enctype='multipart/form-data'>
      <input type='hidden' name='csrf' value='{csrf}'>
      <input type='hidden' name='return_to' value='{return_to}'>
      <input type='file' name='file' accept='image/gif,.gif' required>
      <button type='submit'>Добавить GIF</button>
    </form>
  </aside>
  <section>
    <div class='admin-gif-grid'>{cards}</div>
  </section>
</div>"#,
        csrf = escape_html(&sess.csrf),
        return_to = escape_html(return_to),
        cards = cards,
    )
}

pub(super) async fn admin_gif_upload(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let mut csrf = String::new();
    let mut return_to = "/admin/gifs".to_string();
    let mut original_name = "global.gif".to_string();
    let mut bytes: Option<Vec<u8>> = None;

    loop {
        let next = match multipart.next_field().await {
            Ok(v) => v,
            Err(_) => return admin_redirect_with_msg("/admin/gifs", "Некорректная форма загрузки").into_response(),
        };
        let Some(field) = next else { break; };
        let name = field.name().unwrap_or("").to_string();
        if name == "csrf" {
            csrf = field.text().await.unwrap_or_default();
        } else if name == "return_to" {
            return_to = safe_admin_return_to(&field.text().await.unwrap_or_default(), "/admin/gifs");
        } else if name == "file" {
            original_name = field.file_name().unwrap_or("global.gif").to_string();
            let data = match field.bytes().await {
                Ok(v) => v,
                Err(_) => return admin_redirect_with_msg("/admin/gifs", "Не удалось прочитать GIF").into_response(),
            };
            bytes = Some(data.to_vec());
        }
    }

    if csrf != sess.csrf {
        return admin_redirect_with_msg(&return_to, "CSRF-токен не совпадает").into_response();
    }
    let Some(data) = bytes else {
        return admin_redirect_with_msg(&return_to, "GIF не выбран").into_response();
    };

    match crate::routes::gifs::save_global_gif_asset(&st.db, &original_name, &data).await {
        Ok(_) => admin_redirect_with_msg(&return_to, "GIF добавлен в глобальный список").into_response(),
        Err(e) => {
            let msg = if e.to_string().contains("gif_required") {
                "Нужен корректный GIF до 50 МБ".to_string()
            } else {
                format!("Ошибка: {e}")
            };
            admin_redirect_with_msg(&return_to, &msg).into_response()
        }
    }
}

pub(super) async fn admin_gif_delete(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let return_to = safe_admin_return_to(&f.return_to, "/admin/gifs");
    if f.csrf != sess.csrf {
        return admin_redirect_with_msg(&return_to, "CSRF-токен не совпадает").into_response();
    }

    let res = sqlx::query("DELETE FROM gif_assets WHERE id = $1 AND scope = 'global'")
        .bind(id)
        .execute(&st.db)
        .await;
    match res {
        Ok(done) if done.rows_affected() > 0 => {
            let _ = cleanup_file_storage_orphans_db(&st.db).await;
            admin_redirect_with_msg(&return_to, "GIF удалён из глобального списка").into_response()
        }
        Ok(_) => admin_redirect_with_msg(&return_to, "GIF не найден").into_response(),
        Err(e) => admin_redirect_with_msg(&return_to, &format!("Ошибка: {e}")).into_response(),
    }
}

// =============================
// Downloads management
// =============================

#[derive(Clone)]
pub(super) struct AdminDownloadRow {
    id: i64,
    platform: String,
    version: String,
    original_name: String,
    file_size: i64,
    uploaded_at: String,
    is_active: bool,
}

pub(super) fn admin_download_platform(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "android" | "mobile" | "apk" => Some("android"),
        "pc" | "windows" | "desktop" => Some("pc"),
        _ => None,
    }
}

pub(super) fn admin_download_platform_label(platform: &str) -> &'static str {
    match platform {
        "android" => "Android APK",
        "pc" => "ПК клиент",
        _ => "Клиент",
    }
}

pub(super) fn admin_download_ext(original_name: &str, platform: &str) -> Option<String> {
    let sanitized = admin_sanitize_filename(original_name);
    let ext = std::path::Path::new(&sanitized)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    match platform {
        "android" if ext == "apk" => Some(ext),
        "pc" if matches!(ext.as_str(), "exe" | "msi" | "zip" | "7z" | "rar") => Some(ext),
        _ => None,
    }
}

pub(super) fn admin_download_mime(ext: &str) -> &'static str {
    match ext {
        "apk" => "application/vnd.android.package-archive",
        "exe" => "application/vnd.microsoft.portable-executable",
        "msi" => "application/x-msi",
        "zip" => "application/zip",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        _ => "application/octet-stream",
    }
}

pub(super) async fn fetch_admin_downloads(db: &PgPool) -> Vec<AdminDownloadRow> {
    sqlx::query(
        r#"
        SELECT id, platform, version, original_name, file_size, uploaded_at, is_active
        FROM app_downloads
        ORDER BY platform ASC, is_active DESC, id DESC
        LIMIT 80
        "#,
    )
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|r| AdminDownloadRow {
        id: r.get("id"),
        platform: r.get("platform"),
        version: r.try_get("version").unwrap_or_default(),
        original_name: r.try_get("original_name").unwrap_or_default(),
        file_size: r.try_get("file_size").unwrap_or(0),
        uploaded_at: r.try_get::<chrono::DateTime<chrono::Utc>, _>("uploaded_at").ok().map(|d| d.to_rfc3339()).unwrap_or_default(),
        is_active: r.try_get::<bool, _>("is_active").unwrap_or(false),
    })
    .collect()
}

pub(super) fn render_admin_downloads_panel_body(sess: &AdminSession, rows: &[AdminDownloadRow], return_to: &str) -> String {
    let cards = if rows.is_empty() {
        "<div class='empty-state'>Загрузок пока нет. Загрузите APK или ПК клиент слева.</div>".to_string()
    } else {
        rows.iter()
            .map(|r| {
                let platform = admin_download_platform_label(&r.platform);
                let active = if r.is_active { "Активна" } else { "Скрыта" };
                let active_cls = if r.is_active { "status-active" } else { "" };
                let download_link = if r.is_active {
                    format!(
                        "<a href='/api/downloads/{}/file' target='_blank' rel='noopener'>Скачать</a>",
                        escape_html(&r.platform)
                    )
                } else {
                    String::new()
                };
                let version_text = if r.version.trim().is_empty() { "без версии" } else { r.version.as_str() };
                format!(
                    r#"<div class='admin-download-card'>
  <div class='admin-download-top'>
    <div>
      <div class='admin-download-title'>{platform}</div>
      <div class='admin-download-meta'>{name}</div>
    </div>
    <span class='status-badge {active_cls}'>{active}</span>
  </div>
  <div class='admin-download-meta'>Версия: {version}</div>
  <div class='admin-download-meta'>{size} • {uploaded_at}</div>
  <div class='admin-download-actions'>
    {download_link}
    <form method='post' action='/admin/downloads/{id}/delete'>
      <input type='hidden' name='csrf' value='{csrf}'>
      <input type='hidden' name='return_to' value='{return_to}'>
      <button type='submit' class='btn-danger'>Удалить</button>
    </form>
  </div>
</div>"#,
                    id = r.id,
                    platform = escape_html(platform),
                    name = escape_html(&r.original_name),
                    version = escape_html(version_text),
                    size = escape_html(&admin_format_bytes(r.file_size)),
                    uploaded_at = escape_html(&fmt_admin_dt(&r.uploaded_at)),
                    active = active,
                    active_cls = active_cls,
                    download_link = download_link,
                    csrf = escape_html(&sess.csrf),
                    return_to = escape_html(return_to),
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };

    format!(
        r#"<div class='admin-download-shell'>
  <aside class='admin-download-upload'>
    <h2>Загрузки приложения</h2>
    <p class='small'>Файлы отсюда раздаются сервером на странице скачивания LaBerry. Новая загрузка заменяет активную версию выбранной платформы.</p>
    <form method='post' action='/admin/downloads/upload' enctype='multipart/form-data'>
      <input type='hidden' name='csrf' value='{csrf}'>
      <input type='hidden' name='return_to' value='{return_to}'>
      <label class='small'>Платформа</label>
      <select name='platform' required>
        <option value='android'>Android APK</option>
        <option value='pc'>ПК клиент</option>
      </select>
      <label class='small'>Версия</label>
      <input type='text' name='version' maxlength='64' placeholder='Например: 0.9.3'>
      <input type='file' name='file' accept='.apk,.exe,.msi,.zip,.7z,.rar' required>
      <button type='submit'>Загрузить</button>
    </form>
  </aside>
  <section>
    <div class='admin-download-grid'>{cards}</div>
  </section>
</div>"#,
        csrf = escape_html(&sess.csrf),
        return_to = escape_html(return_to),
        cards = cards,
    )
}

pub(super) async fn downloads_page(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<MsgQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let embedded = q.embed == Some(1);
    let return_to = if embedded { "/admin/downloads?embed=1" } else { "/admin/downloads" };
    let rows = fetch_admin_downloads(&st.db).await;
    let body = render_admin_downloads_panel_body(&sess, &rows, return_to);

    if embedded {
        embedded_page("Админка • Загрузки", &body, q.msg.as_deref()).into_response()
    } else {
        page("Админка • Загрузки", &body, q.msg.as_deref()).into_response()
    }
}

pub(super) async fn admin_download_upload(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let mut csrf = String::new();
    let mut return_to = "/admin/downloads".to_string();
    let mut platform_raw = "android".to_string();
    let mut version = String::new();
    let mut original_name = "laberry.apk".to_string();
    let mut bytes: Option<Vec<u8>> = None;

    loop {
        let next = match multipart.next_field().await {
            Ok(v) => v,
            Err(_) => return admin_redirect_with_msg("/admin/downloads", "Некорректная форма загрузки").into_response(),
        };
        let Some(field) = next else { break; };
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "csrf" => csrf = field.text().await.unwrap_or_default(),
            "return_to" => {
                return_to = safe_admin_return_to(&field.text().await.unwrap_or_default(), "/admin/downloads");
            }
            "platform" => platform_raw = field.text().await.unwrap_or_else(|_| "android".to_string()),
            "version" => version = field.text().await.unwrap_or_default().trim().chars().take(64).collect(),
            "file" => {
                original_name = field.file_name().unwrap_or("laberry.bin").to_string();
                let data = match field.bytes().await {
                    Ok(v) => v,
                    Err(_) => return admin_redirect_with_msg("/admin/downloads", "Не удалось прочитать файл").into_response(),
                };
                bytes = Some(data.to_vec());
            }
            _ => {}
        }
    }

    if csrf != sess.csrf {
        return admin_redirect_with_msg(&return_to, "CSRF-токен не совпадает").into_response();
    }

    let Some(platform) = admin_download_platform(&platform_raw) else {
        return admin_redirect_with_msg(&return_to, "Неизвестная платформа").into_response();
    };
    let Some(data) = bytes else {
        return admin_redirect_with_msg(&return_to, "Файл не выбран").into_response();
    };
    if data.is_empty() {
        return admin_redirect_with_msg(&return_to, "Файл пустой").into_response();
    }
    if data.len() > 512 * 1024 * 1024 {
        return admin_redirect_with_msg(&return_to, "Файл слишком большой (максимум 512 МБ)").into_response();
    }
    let Some(ext) = admin_download_ext(&original_name, platform) else {
        return admin_redirect_with_msg(&return_to, "Для Android нужен .apk, для ПК: .exe, .msi, .zip, .7z или .rar").into_response();
    };

    let storage_dir = PathBuf::from("storage/app_downloads");
    if let Err(e) = fs::create_dir_all(&storage_dir).await {
        return admin_redirect_with_msg(&return_to, &format!("Не удалось создать каталог: {e}")).into_response();
    }
    let stored_filename = format!("{}.{}", Uuid::new_v4(), ext);
    let storage_path = storage_dir.join(stored_filename);
    if let Err(e) = fs::write(&storage_path, &data).await {
        return admin_redirect_with_msg(&return_to, &format!("Не удалось сохранить файл: {e}")).into_response();
    }

    let now = auth::now_iso();
    let mime = admin_download_mime(&ext);
    let storage_path_str = storage_path.to_string_lossy().to_string();
    let mut tx = match st.db.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            let _ = fs::remove_file(&storage_path).await;
            return admin_redirect_with_msg(&return_to, &format!("Ошибка БД: {e}")).into_response();
        }
    };

    let res = async {
        sqlx::query("UPDATE app_downloads SET is_active = FALSE WHERE platform = $1")
            .bind(platform)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            r#"
            INSERT INTO app_downloads(platform, version, original_name, mime_type, file_size, storage_path, uploaded_at, is_active)
            VALUES($1, $2, $3, $4, $5, $6, $7, TRUE)
            "#,
        )
        .bind(platform)
        .bind(&version)
        .bind(&original_name)
        .bind(mime)
        .bind(data.len() as i64)
        .bind(&storage_path_str)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }
    .await;

    match res {
        Ok(_) => admin_redirect_with_msg(&return_to, "Файл приложения загружен").into_response(),
        Err(e) => {
            let _ = fs::remove_file(&storage_path).await;
            admin_redirect_with_msg(&return_to, &format!("Ошибка: {e}")).into_response()
        }
    }
}

pub(super) async fn admin_download_delete(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let return_to = safe_admin_return_to(&f.return_to, "/admin/downloads");
    if f.csrf != sess.csrf {
        return admin_redirect_with_msg(&return_to, "CSRF-токен не совпадает").into_response();
    }

    let row = sqlx::query("SELECT storage_path FROM app_downloads WHERE id = $1 LIMIT 1")
        .bind(id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten();
    let Some(row) = row else {
        return admin_redirect_with_msg(&return_to, "Загрузка не найдена").into_response();
    };
    let storage_path: String = row.try_get("storage_path").unwrap_or_default();

    let res = sqlx::query("DELETE FROM app_downloads WHERE id = $1")
        .bind(id)
        .execute(&st.db)
        .await;

    match res {
        Ok(done) if done.rows_affected() > 0 => {
            if !storage_path.trim().is_empty() {
                let _ = fs::remove_file(PathBuf::from(storage_path)).await;
            }
            admin_redirect_with_msg(&return_to, "Загрузка удалена").into_response()
        }
        Ok(_) => admin_redirect_with_msg(&return_to, "Загрузка не найдена").into_response(),
        Err(e) => admin_redirect_with_msg(&return_to, &format!("Ошибка: {e}")).into_response(),
    }
}

// =============================
// DB Tools panel
// =============================

pub(super) fn render_db_panel_body(sess: &AdminSession, return_to: &str) -> String {
    let rt = escape_html(return_to);
    format!(
        r#"
<div class='card'>
  <div class='hstack'>
    <h2 style='margin:0;'>База данных</h2>
    <span class='pill'>Прямое выполнение</span>
  </div>
  <div class='small' style='margin-top:10px;'>Операции на этой странице запускаются сразу по нажатию. Интерфейс использует единый UTC-формат времени.</div>
</div>

<div class='db-list'>
  <div class='db-card'>
    <h3>Очистить сообщения и вложения</h3>
    <div class='small'>Удаляет сообщения, реакции, закрепы, chat_reads, записи files и сами файлы. Пользователи, серверы и каналы остаются.</div>
    <div style='height:12px'></div>
    <form method='post' action='/admin/db/wipe_messages'>
      <input type='hidden' name='csrf' value='{csrf}' />
      <input type='hidden' name='return_to' value='{rt}' />
      <button type='submit' class='btn-soft'>Очистить сообщения</button>
    </form>
  </div>

  <div class='db-card'>
    <h3>Очистить серверы и каналы</h3>
    <div class='small danger-note'>Удаляет все серверы, каналы, сообщения и файлы внутри них. Личные сообщения не трогаются.</div>
    <div style='height:12px'></div>
    <form method='post' action='/admin/db/wipe_servers'>
      <input type='hidden' name='csrf' value='{csrf}' />
      <input type='hidden' name='return_to' value='{rt}' />
      <button type='submit' class='btn-danger'>Очистить серверы</button>
    </form>
  </div>

  <div class='db-card'>
    <h3>Сбросить всё, кроме пользователей</h3>
    <div class='small danger-note'>Профили, настройки, сессии, друзья, серверы, ЛС, сообщения и файлы будут удалены. Глобальный сервер создастся заново автоматически.</div>
    <div style='height:12px'></div>
    <form method='post' action='/admin/db/reset_keep_users'>
      <input type='hidden' name='csrf' value='{csrf}' />
      <input type='hidden' name='return_to' value='{rt}' />
      <button type='submit' class='btn-danger'>Сбросить, оставить пользователей</button>
    </form>
  </div>

  <div class='db-card'>
    <h3>Очистить просроченные файлы</h3>
    <div class='small'>Удаляет истёкшие временные вложения и мусорные файлы/thumbs без записей в БД.</div>
    <div style='height:12px'></div>
    <form method='post' action='/admin/db/cleanup_expired_files'>
      <input type='hidden' name='csrf' value='{csrf}' />
      <input type='hidden' name='return_to' value='{rt}' />
      <button type='submit' class='btn-soft'>Очистить файлы</button>
    </form>
  </div>

  <div class='db-card'>
    <h3>Выполнить VACUUM</h3>
    <div class='small'>Пересобирает файл базы данных. Во время работы может потребоваться до ~2× свободного места на диске.</div>
    <div style='height:12px'></div>
    <form method='post' action='/admin/db/vacuum'>
      <input type='hidden' name='csrf' value='{csrf}' />
      <input type='hidden' name='return_to' value='{rt}' />
      <button type='submit' class='btn-soft'>Запустить VACUUM</button>
    </form>
  </div>
</div>
"#,
        csrf = escape_html(&sess.csrf),
        rt = rt,
    )
}

pub(super) async fn db_tools_page(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<MsgQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let body = render_db_panel_body(
        &sess,
        if q.embed == Some(1) { "/admin/center?view=db" } else { "/admin/db" },
    );

    if q.embed == Some(1) {
        embedded_page("Админка • База данных", &body, q.msg.as_deref()).into_response()
    } else {
        page("Админка • База данных", &body, q.msg.as_deref()).into_response()
    }
}

pub(super) async fn db_wipe_messages_post(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    db_action_common(st, Some(peer), headers, f, DbAction::WipeMessages).await
}

pub(super) async fn db_wipe_servers_post(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    db_action_common(st, Some(peer), headers, f, DbAction::WipeServers).await
}

pub(super) async fn db_reset_keep_users_post(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    db_action_common(st, Some(peer), headers, f, DbAction::ResetKeepUsers).await
}

pub(super) async fn db_vacuum_post(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    db_action_common(st, Some(peer), headers, f, DbAction::Vacuum).await
}

pub(super) async fn db_cleanup_expired_files_post(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let return_to = safe_admin_return_to(&f.return_to, "/admin/db");
    if f.csrf != sess.csrf {
        return admin_redirect_with_msg(&return_to, "CSRF-токен не совпадает").into_response();
    }

    crate::routes::files::upload::cleanup_expired_files(&st).await;
    admin_redirect_with_msg(&return_to, "Готово. Просроченные файлы и мусорные thumbs очищены.").into_response()
}

pub(super) async fn db_list_expired_files_get(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) {
        return e.into_response();
    }
    let (_sid, _sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let rows = sqlx::query(
        r#"
        SELECT id, filename, storage_path, storage_kind, expires_at, deleted_at, uploaded_by, chat_id
        FROM files
        WHERE expires_at IS NOT NULL
          AND expires_at <= NOW()
        ORDER BY expires_at ASC
        LIMIT 1000
        "#,
    )
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();

    let out: Vec<serde_json::Value> = rows.into_iter().map(|r| {
        serde_json::json!({
            "id": r.get::<i64, _>("id"),
            "filename": r.get::<String, _>("filename"),
            "storage_path": r.get::<String, _>("storage_path"),
            "storage_kind": r.get::<String, _>("storage_kind"),
            "expires_at": r.try_get::<chrono::DateTime<chrono::Utc>, _>("expires_at").ok().map(|d| d.to_rfc3339()),
            "deleted_at": r.try_get::<chrono::DateTime<chrono::Utc>, _>("deleted_at").ok().map(|d| d.to_rfc3339()),
            "uploaded_by": r.try_get::<i64, _>("uploaded_by").ok(),
            "chat_id": r.try_get::<i64, _>("chat_id").ok(),
        })
    }).collect();

    (StatusCode::OK, axum::Json(out)).into_response()
}

pub(super) enum DbAction {
    WipeMessages,
    WipeServers,
    ResetKeepUsers,
    Vacuum,
}

pub(super) async fn db_action_common(
    st: AppState,
    peer: Option<SocketAddr>,
    headers: HeaderMap,
    f: ActionForm,
    act: DbAction,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, peer) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    if f.csrf != sess.csrf {
        return admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/db"), "CSRF-токен не совпадает").into_response();
    }
    let res = match act {
        DbAction::WipeMessages => wipe_all_messages_exec(&st.db).await,
        DbAction::WipeServers => wipe_all_servers_exec(&st.db).await,
        DbAction::ResetKeepUsers => reset_db_keep_users_exec(&st.db).await,
        DbAction::Vacuum => vacuum_exec(&st.db).await,
    };

    match res {
        Ok(_) => admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/db"), "Готово").into_response(),
        Err(e) => admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/db"), &format!("Ошибка: {}", e)).into_response(),
    }
}

// =============================
// Storage / internal ops
// =============================

fn admin_thumb_path_for(stored_filename: &str) -> std::path::PathBuf {
    let stem = std::path::Path::new(stored_filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(stored_filename);
    std::path::PathBuf::from("storage/files/thumbs").join(format!("{}.png", stem))
}

pub(super) async fn cleanup_file_storage_orphans_db(db: &PgPool) -> anyhow::Result<()> {
    use std::collections::HashSet;
    use std::path::PathBuf;

    let rows = sqlx::query(
        r#"
        SELECT filename, storage_path
        FROM files
        WHERE deleted_at IS NULL
          AND (expires_at IS NULL OR expires_at > NOW())
        "#,
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let mut active_paths: HashSet<String> = HashSet::new();
    let mut active_thumbs: HashSet<String> = HashSet::new();

    for r in rows {
        let storage_path: String = r.get("storage_path");
        let filename: String = r.get("filename");
        active_paths.insert(PathBuf::from(storage_path).to_string_lossy().to_string());
        active_thumbs.insert(admin_thumb_path_for(&filename).to_string_lossy().to_string());
    }

    let gif_rows = sqlx::query("SELECT filename, storage_path FROM gif_assets")
        .fetch_all(db)
        .await
        .unwrap_or_default();

    for r in gif_rows {
        let storage_path: String = r.get("storage_path");
        let filename: String = r.get("filename");
        active_paths.insert(PathBuf::from(storage_path).to_string_lossy().to_string());
        active_thumbs.insert(admin_thumb_path_for(&filename).to_string_lossy().to_string());
    }

    let storage_dir = PathBuf::from("storage/files");
    if let Ok(entries) = std::fs::read_dir(&storage_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                continue;
            }
            if path.extension().and_then(|s| s.to_str()) == Some("uploading") {
                continue;
            }
            let key = path.to_string_lossy().to_string();
            if !active_paths.contains(&key) {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    let thumbs_dir = PathBuf::from("storage/files/thumbs");
    if let Ok(entries) = std::fs::read_dir(&thumbs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                continue;
            }
            let key = path.to_string_lossy().to_string();
            if !active_thumbs.contains(&key) {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    Ok(())
}

// =============================
// DB Tools (global wipe/reset)
// =============================

pub(super) async fn wipe_all_messages_exec(db: &PgPool) -> anyhow::Result<()> {
    use std::path::{Path, PathBuf};

    let mut tx = db.begin().await?;

    let file_rows = sqlx::query("SELECT storage_path, filename FROM files")
        .fetch_all(&mut *tx)
        .await?;

    let mut file_paths: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    for fr in file_rows {
        let p: String = fr.get("storage_path");
        let stored_filename: String = fr.get("filename");

        let main = PathBuf::from(p);
        let stem = Path::new(&stored_filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&stored_filename);
        let thumb = PathBuf::from("storage/files/thumbs").join(format!("{}.png", stem));

        file_paths.push((main, Some(thumb)));
    }

    sqlx::query("DELETE FROM user_reports")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM message_reactions")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM pinned_messages")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM chat_reads")
        .execute(&mut *tx)
        .await?;

    sqlx::query("UPDATE gif_assets SET source_file_id = NULL")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM files")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE user_reports SET message_id = NULL")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM messages")
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    let _ = file_paths;
    let _ = cleanup_file_storage_orphans_db(db).await;

    Ok(())
}

pub(super) async fn wipe_all_servers_exec(db: &PgPool) -> anyhow::Result<()> {
    let server_ids = sqlx::query_scalar::<_, i64>("SELECT id FROM servers ORDER BY id")
        .fetch_all(db)
        .await?;

    for sid in server_ids {
        let _ = super::purge_server_exec(db, sid).await;
    }

    crate::db::bootstrap::ensure_global_server(db).await?;
    Ok(())
}

pub(super) async fn reset_db_keep_users_exec(db: &PgPool) -> anyhow::Result<()> {
    use std::path::{Path, PathBuf};

    let mut tx = db.begin().await?;

    let file_rows = sqlx::query("SELECT storage_path, filename FROM files")
        .fetch_all(&mut *tx)
        .await?;

    let mut file_paths: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    for fr in file_rows {
        let p: String = fr.get("storage_path");
        let stored_filename: String = fr.get("filename");

        let main = PathBuf::from(p);
        let stem = Path::new(&stored_filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&stored_filename);
        let thumb = PathBuf::from("storage/files/thumbs").join(format!("{}.png", stem));

        file_paths.push((main, Some(thumb)));
    }

    let profile_rows = sqlx::query("SELECT storage_path FROM profile_files")
        .fetch_all(&mut *tx)
        .await?;
    let mut profile_paths: Vec<PathBuf> = Vec::new();
    for pr in profile_rows {
        let p: String = pr.get("storage_path");
        profile_paths.push(PathBuf::from(p));
    }

    sqlx::query("DELETE FROM user_reports")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM user_suggestions")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM message_reactions")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM pinned_messages")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM chat_reads")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM dm_chats")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM gif_assets")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM files")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE user_reports SET message_id = NULL")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM messages")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM chat_participants")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM chats")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM server_members")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM servers")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM friendships")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM friend_requests")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM user_blocks")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM user_presence")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM user_settings")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM user_sessions")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM refresh_sessions")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM email_codes")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM user_profile")
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM profile_files")
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    let _ = file_paths;
    let _ = cleanup_file_storage_orphans_db(db).await;
    for p in profile_paths {
        let _ = std::fs::remove_file(&p);
    }

    crate::db::bootstrap::ensure_global_server(db).await?;

    Ok(())
}

pub(super) async fn vacuum_exec(db: &PgPool) -> anyhow::Result<()> {
    sqlx::query("VACUUM;").execute(db).await?;
    Ok(())
}
