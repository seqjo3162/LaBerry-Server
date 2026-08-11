use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::{Row, PgPool};
use std::path::PathBuf;
use tokio::{fs, fs::File, io::AsyncWriteExt};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::auth;
use crate::middleware::auth_guard::AuthUser;
use crate::server::AppState;

const MAX_GIF_BYTES: usize = 50 * 1024 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_gifs))
        .route("/clone", post(clone_gif_to_chat))
        .route("/favorites", post(add_favorite))
        .route("/{asset_id}/raw", get(get_gif_raw))
        .route("/favorites/{asset_id}", delete(remove_favorite))
}

#[derive(Deserialize, Default)]
struct DlQuery {
    dl: Option<String>,
}

#[derive(Deserialize, Default)]
struct AddFavoriteBody {
    file_id: Option<i64>,
    asset_id: Option<i64>,
}

#[derive(Deserialize)]
struct CloneGifBody {
    asset_id: i64,
    chat_id: i64,
}

#[derive(Serialize, Clone)]
struct GifAssetView {
    id: i64,
    scope: String,
    original_name: String,
    file_size: i64,
    mime_type: String,
    raw_url: String,
    is_favorite: bool,
    created_at: String,
}

#[derive(Serialize)]
struct GifListResponse {
    global: Vec<GifAssetView>,
    favorites: Vec<GifAssetView>,
}

#[derive(Serialize)]
struct GifMutationResponse {
    ok: bool,
    id: i64,
}

#[derive(Serialize)]
struct GifCloneResponse {
    ok: bool,
    file_id: i64,
    original_name: String,
    file_size: i64,
    mime_type: String,
}

#[derive(Clone)]
struct GifAssetRow {
    id: i64,
    scope: String,
    owner_id: Option<i64>,
    source_file_id: Option<i64>,
    filename: String,
    original_name: String,
    file_size: i64,
    mime_type: String,
    storage_path: String,
    created_at: String,
}

#[derive(Clone)]
struct SourceFileRow {
    id: i64,
    filename: String,
    original_name: String,
    file_size: i64,
    mime_type: String,
    storage_path: String,
    chat_id: i64,
}

fn is_gif_magic(bytes: &[u8]) -> bool {
    bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")
}

fn is_gif_meta(name: &str, mime: &str) -> bool {
    mime.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("image/gif")
        || name.trim().to_ascii_lowercase().ends_with(".gif")
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
    let mut out = String::new();
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            out.push(ch);
        } else if ch.is_whitespace() {
            out.push('_');
        }
    }
    if out.is_empty() { "animation.gif".to_string() } else { out }
}

async fn can_access_chat_by_user_id(st: &AppState, user_id: i64, chat_id: i64) -> bool {
    let row = sqlx::query("SELECT server_id, is_private, COALESCE(kind,'text') AS kind FROM chats WHERE id = $1 LIMIT 1")
        .bind(chat_id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten();

    let Some(chat) = row else {
        return false;
    };

    let kind: String = chat.get("kind");
    if kind == "voice" && st.hub.voice_get_user_channel(user_id) != Some(chat_id) {
        return false;
    }

    let server_id: Option<i64> = chat.try_get("server_id").ok();
    if let Some(server_id) = server_id.filter(|sid| *sid > 0) {
        return sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM server_members WHERE server_id = $1 AND user_id = $2 LIMIT 1",
        )
        .bind(server_id)
        .bind(user_id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten()
        .is_some();
    }

    let in_participants = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM chat_participants WHERE chat_id = $1 AND user_id = $2 LIMIT 1",
    )
    .bind(chat_id)
    .bind(user_id)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten()
    .is_some();

    if in_participants {
        return true;
    }

    sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM dm_chats WHERE chat_id = $1 AND (user_a = $2 OR user_b = $2) LIMIT 1",
    )
    .bind(chat_id)
    .bind(user_id)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten()
    .is_some()
}

async fn load_asset(db: &PgPool, asset_id: i64) -> anyhow::Result<Option<GifAssetRow>> {
    let row = sqlx::query(
        r#"
        SELECT id, scope, owner_id, source_file_id, filename, original_name,
               file_size, mime_type, storage_path, created_at
        FROM gif_assets
        WHERE id = $1
        LIMIT 1
        "#,
    )
    .bind(asset_id)
    .fetch_optional(db)
    .await?;

    Ok(row.map(|r| GifAssetRow {
        id: r.get("id"),
        scope: r.get("scope"),
        owner_id: r.try_get("owner_id").ok(),
        source_file_id: r.try_get("source_file_id").ok(),
        filename: r.get("filename"),
        original_name: r.get("original_name"),
        file_size: r.get("file_size"),
        mime_type: r.get("mime_type"),
        storage_path: r.get("storage_path"),
        created_at: r.get("created_at"),
    }))
}

fn can_use_asset(asset: &GifAssetRow, user_id: i64) -> bool {
    asset.scope == "global" || (asset.scope == "favorite" && asset.owner_id == Some(user_id))
}

async fn load_source_file(st: &AppState, file_id: i64) -> anyhow::Result<Option<SourceFileRow>> {
    let row = sqlx::query(
        r#"
        SELECT id, filename, original_name, file_size, mime_type, storage_path, chat_id
        FROM files
        WHERE id = $1
          AND deleted_at IS NULL
          AND (expires_at IS NULL OR expires_at > NOW())
        LIMIT 1
        "#,
    )
    .bind(file_id)
    .fetch_optional(&st.db)
    .await?;

    Ok(row.map(|r| SourceFileRow {
        id: r.get("id"),
        filename: r.get("filename"),
        original_name: r.get("original_name"),
        file_size: r.get("file_size"),
        mime_type: r.get("mime_type"),
        storage_path: r.get("storage_path"),
        chat_id: r.get("chat_id"),
    }))
}

async fn token_version_for(db: &PgPool, user_id: i64) -> i64 {
    sqlx::query_scalar("SELECT token_version FROM users WHERE id = $1 LIMIT 1")
        .bind(user_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .unwrap_or(0)
}

fn view_for(asset: &GifAssetRow, user_id: i64, token_version: i64, favorite_storage_paths: &[String]) -> Option<GifAssetView> {
    let token = auth::create_file_download_token(user_id, asset.id, token_version).ok()?.0;
    let raw_url = format!("/api/gifs/{}/raw?dl={}", asset.id, percent_encode_query_component(&token));
    let is_favorite = asset.scope == "favorite"
        || favorite_storage_paths.iter().any(|p| p == &asset.storage_path);

    Some(GifAssetView {
        id: asset.id,
        scope: asset.scope.clone(),
        original_name: asset.original_name.clone(),
        file_size: asset.file_size,
        mime_type: asset.mime_type.clone(),
        raw_url,
        is_favorite,
        created_at: asset.created_at.clone(),
    })
}

async fn list_gifs(State(st): State<AppState>, me: AuthUser) -> impl IntoResponse {
    let rows = sqlx::query(
        r#"
        SELECT id, scope, owner_id, source_file_id, filename, original_name,
               file_size, mime_type, storage_path, created_at
        FROM gif_assets
        WHERE scope = 'global' OR (scope = 'favorite' AND owner_id = $1)
        ORDER BY CASE scope WHEN 'favorite' THEN 0 ELSE 1 END, id DESC
        LIMIT 240
        "#,
    )
    .bind(me.id)
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();

    let assets: Vec<GifAssetRow> = rows
        .into_iter()
        .map(|r| GifAssetRow {
            id: r.get("id"),
            scope: r.get("scope"),
            owner_id: r.try_get("owner_id").ok(),
            source_file_id: r.try_get("source_file_id").ok(),
            filename: r.get("filename"),
            original_name: r.get("original_name"),
            file_size: r.get("file_size"),
            mime_type: r.get("mime_type"),
            storage_path: r.get("storage_path"),
            created_at: r.get("created_at"),
        })
        .collect();

    let favorite_storage_paths: Vec<String> = assets
        .iter()
        .filter(|a| a.scope == "favorite")
        .map(|a| a.storage_path.clone())
        .collect();
    let token_version = token_version_for(&st.db, me.id).await;

    let mut global = Vec::new();
    let mut favorites = Vec::new();
    for asset in assets {
        let Some(view) = view_for(&asset, me.id, token_version, &favorite_storage_paths) else {
            continue;
        };
        if asset.scope == "favorite" {
            favorites.push(view);
        } else {
            global.push(view);
        }
    }

    (StatusCode::OK, Json(GifListResponse { global, favorites })).into_response()
}

async fn add_favorite(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<AddFavoriteBody>,
) -> impl IntoResponse {
    let source = if let Some(asset_id) = body.asset_id.filter(|id| *id > 0) {
        let asset = match load_asset(&st.db, asset_id).await {
            Ok(Some(v)) => v,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
        if !can_use_asset(&asset, me.id) {
            return StatusCode::FORBIDDEN.into_response();
        }
        SourceFileRow {
            id: asset.source_file_id.unwrap_or(0),
            filename: asset.filename,
            original_name: asset.original_name,
            file_size: asset.file_size,
            mime_type: asset.mime_type,
            storage_path: asset.storage_path,
            chat_id: 0,
        }
    } else if let Some(file_id) = body.file_id.filter(|id| *id > 0) {
        let file = match load_source_file(&st, file_id).await {
            Ok(Some(v)) => v,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
        if !can_access_chat_by_user_id(&st, me.id, file.chat_id).await {
            return StatusCode::FORBIDDEN.into_response();
        }
        if !is_gif_meta(&file.original_name, &file.mime_type) {
            return (StatusCode::UNSUPPORTED_MEDIA_TYPE, Json(serde_json::json!({"detail":"gif_required"}))).into_response();
        }
        file
    } else {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"detail":"file_id_or_asset_id_required"}))).into_response();
    };

    if fs::metadata(&source.storage_path).await.map(|m| !m.is_file() || m.len() == 0).unwrap_or(true) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let existing = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM gif_assets WHERE scope = 'favorite' AND owner_id = $1 AND storage_path = $2 LIMIT 1",
    )
    .bind(me.id)
    .bind(&source.storage_path)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten();

    if let Some(id) = existing {
        return (StatusCode::OK, Json(GifMutationResponse { ok: true, id })).into_response();
    }

    let created_at = auth::now_iso();
    let res = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO gif_assets(
            scope, owner_id, source_file_id, filename, original_name,
            file_size, mime_type, storage_path, created_by_admin, created_at
        )
        VALUES('favorite', $1, $2, $3, $4, $5, 'image/gif', $6, FALSE, $7) RETURNING id
        "#,
    )
    .bind(me.id)
    .bind(if source.id > 0 { Some(source.id) } else { None })
    .bind(&source.filename)
    .bind(&source.original_name)
    .bind(source.file_size)
    .bind(&source.storage_path)
    .bind(&created_at)
    .fetch_one(&st.db)
    .await;

    let Ok(id) = res else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    if source.id > 0 {
        let _ = sqlx::query("UPDATE files SET storage_kind = 'gif_asset', expires_at = NULL, deleted_at = NULL WHERE id = $1")
            .bind(source.id)
            .execute(&st.db)
            .await;
    }

    (
        StatusCode::OK,
        Json(GifMutationResponse { ok: true, id }),
    )
        .into_response()
}

async fn remove_favorite(
    State(st): State<AppState>,
    me: AuthUser,
    Path(asset_id): Path<i64>,
) -> impl IntoResponse {
    let res = sqlx::query("DELETE FROM gif_assets WHERE id = $1 AND scope = 'favorite' AND owner_id = $2")
        .bind(asset_id)
        .bind(me.id)
        .execute(&st.db)
        .await;

    match res {
        Ok(_) => {
            crate::routes::files::cleanup_orphan_storage_files(&st).await;
            (StatusCode::OK, Json(serde_json::json!({"ok":true}))).into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn clone_gif_to_chat(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<CloneGifBody>,
) -> impl IntoResponse {
    if body.asset_id <= 0 || body.chat_id <= 0 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if !can_access_chat_by_user_id(&st, me.id, body.chat_id).await {
        return StatusCode::FORBIDDEN.into_response();
    }

    let asset = match load_asset(&st.db, body.asset_id).await {
        Ok(Some(v)) => v,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if !can_use_asset(&asset, me.id) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if fs::metadata(&asset.storage_path).await.map(|m| !m.is_file() || m.len() == 0).unwrap_or(true) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let created_at = auth::now_iso();
    let expires_at = sqlx::query_scalar::<_, String>(r#"SELECT to_char(now() + interval '24 hours', 'YYYY-MM-DD"T"HH24:MI:SS"Z"')"#)
        .fetch_one(&st.db)
        .await
        .unwrap_or_else(|_| created_at.clone());

    let res = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO files(
            filename, original_name, file_size, mime_type, storage_path,
            uploaded_by, chat_id, created_at, storage_kind, expires_at
        )
        VALUES($1, $2, $3, 'image/gif', $4, $5, $6, $7, 'temporary', $8) RETURNING id
        "#,
    )
    .bind(&asset.filename)
    .bind(&asset.original_name)
    .bind(asset.file_size)
    .bind(&asset.storage_path)
    .bind(me.id)
    .bind(body.chat_id)
    .bind(&created_at)
    .bind(&expires_at)
    .fetch_one(&st.db)
    .await;

    let Ok(file_id) = res else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    (
        StatusCode::OK,
        Json(GifCloneResponse {
            ok: true,
            file_id,
            original_name: asset.original_name,
            file_size: asset.file_size,
            mime_type: "image/gif".to_string(),
        }),
    )
        .into_response()
}

async fn resolve_user_id_for_asset_request(
    st: &AppState,
    me: Option<&AuthUser>,
    asset_id: i64,
    dl: Option<&str>,
) -> Result<i64, StatusCode> {
    if let Some(me) = me {
        return Ok(me.id);
    }
    let Some(token) = dl else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    let claims = auth::decode_file_download_claims(token).map_err(|_| StatusCode::UNAUTHORIZED)?;
    if claims.file_id != asset_id {
        return Err(StatusCode::FORBIDDEN);
    }
    let tv = token_version_for(&st.db, claims.uid).await;
    if tv != claims.token_version {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(claims.uid)
}

async fn get_gif_raw(
    State(st): State<AppState>,
    Path(asset_id): Path<i64>,
    Query(q): Query<DlQuery>,
    headers: HeaderMap,
    me: Option<AuthUser>,
) -> impl IntoResponse {
    let uid = match resolve_user_id_for_asset_request(&st, me.as_ref(), asset_id, q.dl.as_deref()).await {
        Ok(v) => v,
        Err(code) => return code.into_response(),
    };
    let asset = match load_asset(&st.db, asset_id).await {
        Ok(Some(v)) => v,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if !can_use_asset(&asset, uid) {
        return StatusCode::FORBIDDEN.into_response();
    }

    let file = match File::open(&asset.storage_path).await {
        Ok(v) => v,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let len_value = asset.file_size.to_string();
    let len = HeaderValue::from_str(&len_value).unwrap_or_else(|_| HeaderValue::from_static("0"));
    let safe_name = sanitize_filename(&asset.original_name);
    let cd = HeaderValue::from_str(&format!("inline; filename=\"{}\"", safe_name))
        .unwrap_or_else(|_| HeaderValue::from_static("inline"));
    let cache_control = if headers.get(header::RANGE).is_some() {
        HeaderValue::from_static("private, max-age=300")
    } else {
        HeaderValue::from_static("private, max-age=900")
    };

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("image/gif")),
            (header::CONTENT_DISPOSITION, cd),
            (header::CONTENT_LENGTH, len),
            (header::CACHE_CONTROL, cache_control),
            (header::HeaderName::from_static("x-content-type-options"), HeaderValue::from_static("nosniff")),
        ],
        axum::body::Body::from_stream(ReaderStream::new(file)),
    )
        .into_response()
}

pub(crate) async fn save_global_gif_asset(db: &PgPool, original_name: &str, bytes: &[u8]) -> anyhow::Result<i64> {
    if bytes.is_empty() || bytes.len() > MAX_GIF_BYTES || !is_gif_magic(bytes) {
        anyhow::bail!("gif_required");
    }

    let storage_dir = PathBuf::from("storage/files");
    fs::create_dir_all(&storage_dir).await?;
    let stored_filename = format!("{}.gif", Uuid::new_v4());
    let storage_path = storage_dir.join(&stored_filename);

    let mut file = fs::File::create(&storage_path).await?;
    file.write_all(bytes).await?;
    file.flush().await?;

    let name = {
        let raw = original_name.trim();
        if raw.is_empty() { "global.gif" } else { raw }
    };
    let created_at = auth::now_iso();
    let res = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO gif_assets(
            scope, owner_id, source_file_id, filename, original_name,
            file_size, mime_type, storage_path, created_by_admin, created_at
        )
        VALUES('global', NULL, NULL, $1, $2, $3, 'image/gif', $4, TRUE, $5) RETURNING id
        "#,
    )
    .bind(&stored_filename)
    .bind(name)
    .bind(bytes.len() as i64)
    .bind(storage_path.to_string_lossy().to_string())
    .bind(&created_at)
    .fetch_one(db)
    .await;

    match res {
        Ok(id) => Ok(id),
        Err(e) => {
            let _ = fs::remove_file(storage_path).await;
            Err(e.into())
        }
    }
}
