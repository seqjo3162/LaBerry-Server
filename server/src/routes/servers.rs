use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post, delete},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{auth, server::AppState};
use crate::middleware::auth_guard::AuthUser;

#[derive(Deserialize)]
pub struct CreateServerBody {
    pub name: String,
}


#[derive(Deserialize)]
pub struct CreateChannelBody {
    pub name: String,
    pub kind: String, // text|voice
}

#[derive(Serialize)]
pub struct ServerRow {
    pub id: i64,
    pub name: String,
    pub owner_id: i64,
    pub created_at: String,
}

#[derive(Serialize)]
pub struct ChatRow {
    pub id: i64,
    pub name: Option<String>,
    pub kind: String,
    pub server_id: Option<i64>,
    pub is_private: i64,
    pub created_at: String,

    // UI helpers
    pub unread_count: i64,
    pub last_message_id: Option<i64>,
    pub last_message_sender: Option<String>,
    pub last_message_preview: Option<String>,
}


async fn cleanup_duplicate_channels(db: &sqlx::SqlitePool, server_id: i64) {
    // Cleanup duplicate channels (same name + kind) safely.
    // If a duplicate has no real data (messages/files/pins), we can merge
    // read-state/participants into the kept channel and delete the duplicate.

    let rows = sqlx::query(
        r#"
        SELECT id, COALESCE(name,'') AS name, COALESCE(kind,'text') AS kind
        FROM chats
        WHERE server_id = ? AND is_private = 0
        "#,
    )
    .bind(server_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    use std::collections::HashMap;
    let mut groups: HashMap<(String, String), Vec<i64>> = HashMap::new();

    for r in rows {
        let id: i64 = r.get("id");
        let name: String = r.get::<String, _>("name");
        let kind: String = r.get::<String, _>("kind");
        let key = (name.trim().to_lowercase(), kind.trim().to_lowercase());
        groups.entry(key).or_default().push(id);
    }

    for (_key, ids) in groups {
        if ids.len() <= 1 {
            continue;
        }

        // Pick keep_id: most messages, then smallest id
        let mut keep_id: i64 = ids[0];
        let mut keep_msgs: i64 = -1;

        let mut stats: Vec<(i64, i64, i64, i64)> = Vec::with_capacity(ids.len()); // (id, msgs, files, pins)
        for cid in &ids {
            let msgs = sqlx::query_scalar::<_, i64>("SELECT COUNT(1) FROM messages WHERE chat_id = ?")
                .bind(*cid)
                .fetch_one(db)
                .await
                .unwrap_or(0);

            let files = sqlx::query_scalar::<_, i64>("SELECT COUNT(1) FROM files WHERE chat_id = ?")
                .bind(*cid)
                .fetch_one(db)
                .await
                .unwrap_or(0);

            let pins = sqlx::query_scalar::<_, i64>("SELECT COUNT(1) FROM pinned_messages WHERE chat_id = ?")
                .bind(*cid)
                .fetch_one(db)
                .await
                .unwrap_or(0);

            if msgs > keep_msgs || (msgs == keep_msgs && *cid < keep_id) {
                keep_msgs = msgs;
                keep_id = *cid;
            }

            stats.push((*cid, msgs, files, pins));
        }

        let now = auth::now_iso();

        for (cid, msgs, files, pins) in stats {
            if cid == keep_id {
                continue;
            }

            // If there is real data in the duplicate, do NOT touch it.
            if msgs != 0 || files != 0 || pins != 0 {
                continue;
            }

            // Merge chat_reads into keep_id (max last_read_message_id)
            let read_rows = sqlx::query(
                "SELECT user_id, last_read_message_id FROM chat_reads WHERE chat_id = ?",
            )
            .bind(cid)
            .fetch_all(db)
            .await
            .unwrap_or_default();

            for rr in read_rows {
                let user_id: i64 = rr.get("user_id");
                let last_read: i64 = rr.get("last_read_message_id");

                let _ = sqlx::query(
                    r#"
                    INSERT INTO chat_reads (chat_id, user_id, last_read_message_id, updated_at)
                    VALUES (?, ?, ?, ?)
                    ON CONFLICT(chat_id, user_id) DO UPDATE SET
                        last_read_message_id = CASE
                            WHEN excluded.last_read_message_id > chat_reads.last_read_message_id
                            THEN excluded.last_read_message_id
                            ELSE chat_reads.last_read_message_id
                        END,
                        updated_at = excluded.updated_at
                    "#,
                )
                .bind(keep_id)
                .bind(user_id)
                .bind(last_read)
                .bind(&now)
                .execute(db)
                .await;
            }

            let _ = sqlx::query("DELETE FROM chat_reads WHERE chat_id = ?")
                .bind(cid)
                .execute(db)
                .await;

            // Merge chat_participants into keep_id
            let _ = sqlx::query(
                r#"
                INSERT INTO chat_participants (chat_id, user_id)
                SELECT ?, user_id
                FROM chat_participants
                WHERE chat_id = ?
                ON CONFLICT(chat_id, user_id) DO NOTHING
                "#,
            )
            .bind(keep_id)
            .bind(cid)
            .execute(db)
            .await;

            let _ = sqlx::query("DELETE FROM chat_participants WHERE chat_id = ?")
                .bind(cid)
                .execute(db)
                .await;

            // Safety: remove any other empty references
            let _ = sqlx::query("DELETE FROM pinned_messages WHERE chat_id = ?")
                .bind(cid)
                .execute(db)
                .await;
            let _ = sqlx::query("DELETE FROM files WHERE chat_id = ?")
                .bind(cid)
                .execute(db)
                .await;

            let _ = sqlx::query("DELETE FROM chats WHERE id = ?")
                .bind(cid)
                .execute(db)
                .await;
        }
    }
}

async fn ensure_default_channels(db: &sqlx::SqlitePool, server_id: i64) {
    // Требование: у сервера по дефолту должен быть 1 текстовый и 1 голосовой канал.
    // Плюс это чинит старые сервера, созданные до добавления voice.

    let has_text = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM chats WHERE server_id = ? AND LOWER(TRIM(COALESCE(kind,'text'))) = 'text' LIMIT 1",
    )
    .bind(server_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some();

    let has_voice = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM chats WHERE server_id = ? AND LOWER(TRIM(COALESCE(kind,'text'))) = 'voice' LIMIT 1",
    )
    .bind(server_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some();

    let now = auth::now_iso();

    if !has_text {
        let _ = sqlx::query(
            "INSERT INTO chats(name, server_id, kind, is_private, created_at) VALUES(?, ?, 'text', 0, ?)",
        )
        .bind("general")
        .bind(server_id)
        .bind(&now)
        .execute(db)
        .await;
    }

    if !has_voice {
        let _ = sqlx::query(
            "INSERT INTO chats(name, server_id, kind, is_private, created_at) VALUES(?, ?, 'voice', 0, ?)",
        )
        .bind("Voice")
        .bind(server_id)
        .bind(&now)
        .execute(db)
        .await;
    }

    cleanup_duplicate_channels(db, server_id).await;
}


async fn can_manage_channels(db: &sqlx::SqlitePool, server_id: i64, user_id: i64) -> bool {
    // Server owner OR role=admin
    let owner = sqlx::query_scalar::<_, i64>("SELECT owner_id FROM servers WHERE id = ? LIMIT 1")
        .bind(server_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    if owner == Some(user_id) {
        return true;
    }

    let role = sqlx::query_scalar::<_, String>(
        "SELECT COALESCE(role,'member') FROM server_members WHERE server_id = ? AND user_id = ? LIMIT 1",
    )
    .bind(server_id)
    .bind(user_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| "member".to_string());

    role == "admin"
}

async fn create_chat(
    State(st): State<AppState>,
    me: AuthUser,
    Path(server_id): Path<i64>,
    Json(body): Json<CreateChannelBody>,
) -> impl IntoResponse {
    let db = &st.db;

    // membership check
    let member = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM server_members WHERE server_id = ? AND user_id = ? LIMIT 1",
    )
    .bind(server_id)
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some();

    if !member {
        return StatusCode::FORBIDDEN.into_response();
    }

    if !can_manage_channels(db, server_id, me.id).await {
        return StatusCode::FORBIDDEN.into_response();
    }

    let name = body.name.trim();
    if name.is_empty() || name.len() > 64 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"detail":"bad_name"})),
        )
            .into_response();
    }

    let kind = body.kind.trim().to_lowercase();
    let kind = if kind == "voice" { "voice" } else { "text" };

    // prevent exact duplicates (name+kind) to avoid UI confusion
    if let Some(existing_id) = sqlx::query_scalar::<_, i64>(
        r#"SELECT id FROM chats
           WHERE server_id = ? AND is_private = 0
             AND LOWER(TRIM(COALESCE(name,''))) = LOWER(TRIM(?))
             AND LOWER(TRIM(COALESCE(kind,'text'))) = ?
           LIMIT 1"#,
    )
    .bind(server_id)
    .bind(name)
    .bind(kind)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    {
        return (
            StatusCode::OK,
            Json(serde_json::json!({"id": existing_id, "existed": true})),
        )
            .into_response();
    }

    let created_at = auth::now_iso();
    let res = sqlx::query(
        "INSERT INTO chats(name, server_id, kind, is_private, created_at) VALUES(?, ?, ?, 0, ?)",
    )
    .bind(name)
    .bind(server_id)
    .bind(kind)
    .bind(&created_at)
    .execute(db)
    .await;

    let Ok(r) = res else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    let chat_id = r.last_insert_rowid();

    // keep server invariant: at least one text + one voice
    ensure_default_channels(db, server_id).await;

    (StatusCode::OK, Json(serde_json::json!({"id": chat_id}))).into_response()
}

async fn delete_chat(
    State(st): State<AppState>,
    me: AuthUser,
    Path((server_id, chat_id)): Path<(i64, i64)>,
) -> impl IntoResponse {
    let db = &st.db;

    // membership check
    let member = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM server_members WHERE server_id = ? AND user_id = ? LIMIT 1",
    )
    .bind(server_id)
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some();

    if !member {
        return StatusCode::FORBIDDEN.into_response();
    }

    if !can_manage_channels(db, server_id, me.id).await {
        return StatusCode::FORBIDDEN.into_response();
    }

    let row = sqlx::query(
        "SELECT COALESCE(kind,'text') AS kind FROM chats WHERE id = ? AND server_id = ? AND is_private = 0 LIMIT 1",
    )
    .bind(chat_id)
    .bind(server_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(r) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let kind: String = r.get("kind");
    let kind_l = kind.trim().to_lowercase();
    let kind_l = if kind_l == "voice" { "voice" } else { "text" };

    // forbid deleting the last channel of this kind
    let cnt = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(1) FROM chats WHERE server_id = ? AND is_private = 0 AND LOWER(TRIM(COALESCE(kind,'text'))) = ?",
    )
    .bind(server_id)
    .bind(kind_l)
    .fetch_one(db)
    .await
    .unwrap_or(0);

    if cnt <= 1 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"detail":"cannot_delete_last_of_kind"})),
        )
            .into_response();
    }

    // delete reactions first (no cascade)
    let _ = sqlx::query(
        r#"DELETE FROM message_reactions WHERE message_id IN (SELECT id FROM messages WHERE chat_id = ?)"#,
    )
    .bind(chat_id)
    .execute(db)
    .await;

    let _ = sqlx::query("DELETE FROM pinned_messages WHERE chat_id = ?")
        .bind(chat_id)
        .execute(db)
        .await;

    let _ = sqlx::query("DELETE FROM files WHERE chat_id = ?")
        .bind(chat_id)
        .execute(db)
        .await;

    let _ = sqlx::query("DELETE FROM messages WHERE chat_id = ?")
        .bind(chat_id)
        .execute(db)
        .await;

    let _ = sqlx::query("DELETE FROM chat_reads WHERE chat_id = ?")
        .bind(chat_id)
        .execute(db)
        .await;

    let _ = sqlx::query("DELETE FROM chat_participants WHERE chat_id = ?")
        .bind(chat_id)
        .execute(db)
        .await;

    let _ = sqlx::query("DELETE FROM chats WHERE id = ?")
        .bind(chat_id)
        .execute(db)
        .await;

    // keep invariant
    ensure_default_channels(db, server_id).await;

    (StatusCode::OK, Json(serde_json::json!({"status":"ok"}))).into_response()
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create).get(list))
        .route("/:server_id/join", post(join))
        .route("/:server_id", delete(delete_server))
        .route("/:server_id/chats", get(list_chats).post(create_chat))
        .route("/:server_id/chats/:chat_id", delete(delete_chat))
        .route("/:server_id/members", get(list_members))
        .route("/:server_id/chats/:chat_id/messages", get(crate::routes::messages::list).post(crate::routes::messages::send))
        .route("/:server_id/chats/:chat_id/messages/", get(crate::routes::messages::list).post(crate::routes::messages::send))
}

#[derive(Serialize)]
pub struct MemberView {
    pub id: i64,
    pub username: String,
    pub avatar_file_id: Option<i64>,
    pub role: String,
    pub is_online: bool,
    pub status: String,
}

async fn create(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<CreateServerBody>,
) -> impl IntoResponse {
    let db = &st.db;
    let created_at = auth::now_iso();

    let res = sqlx::query(
        "INSERT INTO servers(name, owner_id, created_at) VALUES(?, ?, ?)",
    )
    .bind(&body.name)
    .bind(me.id)
    .bind(&created_at)
    .execute(db)
    .await;

    let Ok(r) = res else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    let server_id = r.last_insert_rowid();

    let _ = sqlx::query(
        "INSERT OR IGNORE INTO server_members(server_id, user_id, role) VALUES(?, ?, 'admin')",
    )
    .bind(server_id)
    .bind(me.id)
    .execute(db)
    .await;

    // default channels
	ensure_default_channels(db, server_id).await;

    (StatusCode::OK, Json(serde_json::json!({ "id": server_id }))).into_response()
}

async fn join(
    State(st): State<AppState>,
    me: AuthUser,
    Path(server_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    let exists = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM servers WHERE id = ? LIMIT 1",
    )
    .bind(server_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some();

    if !exists {
        return StatusCode::NOT_FOUND.into_response();
    }

    if sqlx::query(
        "INSERT OR IGNORE INTO server_members(server_id, user_id) VALUES(?, ?)",
    )
    .bind(server_id)
    .bind(me.id)
    .execute(db)
    .await
    .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
}

async fn list(
    State(st): State<AppState>,
    me: AuthUser,
) -> impl IntoResponse {
    let db = &st.db;

    let rows = sqlx::query(
        r#"
        SELECT DISTINCT s.id, s.name, s.owner_id, s.created_at
        FROM servers s
        JOIN server_members m ON m.server_id = s.id
        WHERE m.user_id = ?
        ORDER BY s.id DESC
        "#,
    )
    .bind(me.id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let servers = rows
        .into_iter()
        .map(|r| ServerRow {
            id: r.get("id"),
            name: r.get("name"),
            owner_id: r.get("owner_id"),
            created_at: r.get("created_at"),
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(servers)).into_response()
}

async fn list_chats(
    State(st): State<AppState>,
    me: AuthUser,
    Path(server_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    let member = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM server_members WHERE server_id = ? AND user_id = ? LIMIT 1",
    )
    .bind(server_id)
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some();

    if !member {
        return StatusCode::FORBIDDEN.into_response();
    }

	// гарантируем дефолтные каналы (и для серверов, созданных до voice)
	ensure_default_channels(db, server_id).await;

    let rows = sqlx::query(
        r#"
        SELECT
            c.id,
            c.name,
            COALESCE(c.kind, 'text') as kind,
            c.server_id,
            c.is_private,
            c.created_at,
            (
                SELECT m.id
                FROM messages m
                WHERE m.chat_id = c.id
                ORDER BY m.id DESC
                LIMIT 1
            ) AS last_message_id,
            (
                SELECT u.username
                FROM messages m
                JOIN users u ON u.id = m.sender_id
                WHERE m.chat_id = c.id
                ORDER BY m.id DESC
                LIMIT 1
            ) AS last_message_sender,
            (
                SELECT substr(m.content, 1, 120)
                FROM messages m
                WHERE m.chat_id = c.id
                ORDER BY m.id DESC
                LIMIT 1
            ) AS last_message_preview,
            (
                SELECT COUNT(*)
                FROM messages m
                WHERE m.chat_id = c.id
                  AND m.id > COALESCE((
                    SELECT r.last_read_message_id
                    FROM chat_reads r
                    WHERE r.chat_id = c.id AND r.user_id = ?
                    LIMIT 1
                  ), 0)
            ) AS unread_count
        FROM chats c
        WHERE c.server_id = ?
        ORDER BY CASE COALESCE(c.kind,'text') WHEN 'voice' THEN 0 ELSE 1 END, COALESCE(last_message_id, c.id) DESC
        "#,
    )
    .bind(me.id)
    .bind(server_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let chats = rows
        .into_iter()
        .map(|r| ChatRow {
            id: r.get("id"),
            name: r.get("name"),
            kind: r.get("kind"),
            server_id: r.get("server_id"),
            is_private: r.get("is_private"),
            created_at: r.get("created_at"),
            unread_count: r.get::<i64, _>("unread_count"),
            last_message_id: r.try_get("last_message_id").ok(),
            last_message_sender: r.try_get("last_message_sender").ok(),
            last_message_preview: r.try_get("last_message_preview").ok(),
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(chats)).into_response()
}

async fn list_members(
    State(st): State<AppState>,
    me: AuthUser,
    Path(server_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    // membership check
    let member = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM server_members WHERE server_id = ? AND user_id = ? LIMIT 1",
    )
    .bind(server_id)
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some();

    if !member {
        return StatusCode::FORBIDDEN.into_response();
    }

    let rows = sqlx::query(
        r#"
        SELECT
            u.id as id,
            u.username as username,
            up.avatar_file_id as avatar_file_id,
            COALESCE(m.role, 'member') as role,
            CASE
              WHEN COALESCE(p.is_online, 0) = 0 THEN 0
              WHEN p.status = 'invisible' THEN 0
              ELSE 1
            END as is_online,
            CASE
              WHEN COALESCE(p.is_online, 0) = 0 THEN 'offline'
              WHEN p.status = 'invisible' THEN 'offline'
              ELSE COALESCE(p.status, 'online')
            END as status
        FROM server_members m
        JOIN users u ON u.id = m.user_id
        LEFT JOIN user_profile up ON up.user_id = u.id
        LEFT JOIN user_presence p ON p.user_id = u.id
        WHERE m.server_id = ?
        ORDER BY is_online DESC, u.username ASC
        "#,
    )
    .bind(server_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let members = rows
        .into_iter()
        .map(|r| MemberView {
            id: r.get("id"),
            username: r.get("username"),
            avatar_file_id: r.try_get::<Option<i64>, _>("avatar_file_id").ok().flatten(),
            role: r.get::<String, _>("role"),
            is_online: r.get::<i64, _>("is_online") != 0,
            status: r.get::<String, _>("status"),
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(members)).into_response()
}

async fn delete_server(
    State(st): State<AppState>,
    me: AuthUser,
    Path(server_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    let owner = sqlx::query_scalar::<_, i64>(
        "SELECT owner_id FROM servers WHERE id = ? LIMIT 1",
    )
    .bind(server_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(owner_id) = owner else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if owner_id != me.id {
        return StatusCode::FORBIDDEN.into_response();
    }

    let mut tx = match db.begin().await {
        Ok(t) => t,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    // collect chats
    let chat_ids = sqlx::query_scalar::<_, i64>("SELECT id FROM chats WHERE server_id = ?")
        .bind(server_id)
        .fetch_all(&mut *tx)
        .await
        .unwrap_or_default();

    for cid in chat_ids.iter().copied() {
        let _ = sqlx::query("DELETE FROM pinned_messages WHERE chat_id = ?")
            .bind(cid)
            .execute(&mut *tx)
            .await;
        let _ = sqlx::query("DELETE FROM chat_reads WHERE chat_id = ?")
            .bind(cid)
            .execute(&mut *tx)
            .await;
        let _ = sqlx::query("DELETE FROM files WHERE chat_id = ?")
            .bind(cid)
            .execute(&mut *tx)
            .await;
        let _ = sqlx::query("DELETE FROM message_reactions WHERE message_id IN (SELECT id FROM messages WHERE chat_id = ?)")
            .bind(cid)
            .execute(&mut *tx)
            .await;
        let _ = sqlx::query("DELETE FROM messages WHERE chat_id = ?")
            .bind(cid)
            .execute(&mut *tx)
            .await;
        let _ = sqlx::query("DELETE FROM chat_participants WHERE chat_id = ?")
            .bind(cid)
            .execute(&mut *tx)
            .await;
    }

    let _ = sqlx::query("DELETE FROM chats WHERE server_id = ?")
        .bind(server_id)
        .execute(&mut *tx)
        .await;

    let _ = sqlx::query("DELETE FROM server_members WHERE server_id = ?")
        .bind(server_id)
        .execute(&mut *tx)
        .await;

    let _ = sqlx::query("DELETE FROM servers WHERE id = ?")
        .bind(server_id)
        .execute(&mut *tx)
        .await;

    if tx.commit().await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    StatusCode::NO_CONTENT.into_response()
}
