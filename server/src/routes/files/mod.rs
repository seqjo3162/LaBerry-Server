use axum::{
    http::StatusCode,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use sqlx::Row;

use crate::auth;
use crate::middleware::auth_guard::AuthUser;
use crate::server::AppState;

pub mod upload;
pub mod serve;
pub(crate) use self::upload::{
    cleanup_expired_files,
    cleanup_orphan_storage_files,
    cleanup_file_artifacts_if_unreferenced,
};
pub(crate) use self::upload::{
    heal_message_file_if_referenced,
    thumb_only_file_can_be_shown,
    mark_file_expired,
    upload_file,
};
pub(crate) use self::serve::{get_file_link, get_preview, get_archive, get_file_raw, get_file};

const MAX_UPLOAD_BYTES: i64 = 50 * 1024 * 1024;
const MAX_IMAGE_DIM: u32 = 12000;
const MAX_IMAGE_PIXELS: u64 = 80_000_000;
const THUMB_MAX_W: u32 = 800;
const THUMB_MAX_H: u32 = 800;
const TEMP_FILE_EXPIRES_SQL_MODIFIER: &str = "+24 hours";
const ORPHAN_CLEANUP_GRACE_SECS: u64 = 60 * 60;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/{file_id}/link", get(get_file_link))
        .route("/{file_id}/preview", get(get_preview))
        .route("/{file_id}/archive", get(get_archive))
        .route("/{file_id}/raw", get(get_file_raw))
        .route("/", post(upload_file))
        .route("/{file_id}", get(get_file))
}

#[derive(Deserialize, Default)]
pub(crate) struct DlQuery {
    /// short-lived signed token for downloads/previews without Authorization header
    dl: Option<String>,
}

#[derive(sqlx::FromRow)]
pub(super) struct FileServeRow {
    pub(super) filename: String,
    pub(super) original_name: String,
    pub(super) mime_type: String,
    pub(super) storage_path: String,
    pub(super) chat_id: i64,
    pub(super) is_expired: i64,
}

#[derive(sqlx::FromRow)]
pub(super) struct FileAccessRow {
    pub(super) filename: String,
    pub(super) storage_path: String,
    pub(super) chat_id: i64,
    pub(super) is_expired: i64,
}

#[derive(sqlx::FromRow)]
pub(super) struct ExpiredFileRow {
    pub(super) id: i64,
    pub(super) filename: String,
    pub(super) storage_path: String,
    pub(super) chat_id: i64,
}

pub(super) async fn resolve_user_id_for_file_request(
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

    let file_row = sqlx::query("SELECT chat_id FROM files WHERE id = ? LIMIT 1")
        .bind(file_id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten()
        .ok_or(StatusCode::NOT_FOUND)?;

    let chat_id: i64 = file_row.get("chat_id");

    if !can_access_chat_by_user_id(st, claims.uid, chat_id).await {
        tracing::error!("[SECURITY] Unauthorized file download attempt: user_id={}, file_id={}, chat_id={}", claims.uid, file_id, chat_id);
        return Err(StatusCode::FORBIDDEN);
    }

    Ok(claims.uid)
}

pub(super) async fn can_access_chat_by_user_id(st: &AppState, user_id: i64, chat_id: i64) -> bool {
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
            .fetch_optional(&st.db)
            .await
            .ok()
            .flatten()
            .is_some()
        }
    }
}

pub(super) async fn load_file_for_serving(st: &AppState, file_id: i64) -> Result<FileServeRow, StatusCode> {
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

pub(super) async fn load_file_for_access(st: &AppState, file_id: i64) -> Result<FileAccessRow, StatusCode> {
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
