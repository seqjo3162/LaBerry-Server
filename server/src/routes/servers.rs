use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{auth, server::AppState};
use crate::middleware::auth_guard::AuthUser;

#[derive(Deserialize)]
pub struct CreateServerBody {
    pub name: String,
    pub is_public: Option<bool>,
}

#[derive(Deserialize)]
pub struct UpdateServerBody {
    pub name: Option<String>,
    pub is_public: Option<bool>,
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
    pub is_public: bool,
    pub my_role: String,
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


#[derive(Deserialize)]
pub struct DiscoverQuery {
    pub q: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Serialize)]
pub struct DiscoverServerRow {
    pub id: i64,
    pub name: String,
    pub owner_id: i64,
    pub created_at: String,
    pub is_public: bool,
    pub members_count: i64,
}

#[derive(Deserialize)]
pub struct InviteBody {
    pub username: String,
}

#[derive(Deserialize)]
pub struct JoinRequestBody {
    pub from_server_id: Option<i64>,
}

#[derive(Serialize)]
pub struct JoinRequestView {
    pub id: i64,
    pub server_id: i64,
    pub server_name: String,
    pub server_is_public: bool,
    pub requester_id: i64,
    pub requester_username: String,
    pub requester_avatar_file_id: Option<i64>,
    pub from_server_id: Option<i64>,
    pub from_server_name: Option<String>,
    pub status: String,
    pub created_at: String,
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
        .route("/discover", get(discover))
        .route("/join-requests/incoming", get(list_incoming_join_requests))
        .route("/join-requests/outgoing", get(list_outgoing_join_requests))
        .route("/join-requests/:request_id/accept", post(accept_join_request))
        .route("/join-requests/:request_id/reject", post(reject_join_request))
        .route("/:server_id/join", post(join))
        .route("/:server_id/join-request", post(create_join_request))
        .route("/:server_id/invite", post(invite_member))
        .route("/:server_id", patch(update_server).delete(delete_server))
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
    pub public_encryption_key: Option<String>,
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

    let is_public = body.is_public.unwrap_or(true);

    let res = sqlx::query(
        "INSERT INTO servers(name, owner_id, created_at, is_public) VALUES(?, ?, ?, ?)",
    )
    .bind(&body.name)
    .bind(me.id)
    .bind(&created_at)
    .bind(if is_public { 1 } else { 0 })
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

    let is_public = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(is_public, 1) FROM servers WHERE id = ? LIMIT 1",
    )
    .bind(server_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(is_public) = is_public else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if is_public == 0 {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"detail":"private_server"})),
        )
            .into_response();
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
        SELECT DISTINCT s.id, s.name, s.owner_id, s.created_at,
               COALESCE(s.is_public, 1) AS is_public,
               COALESCE(m.role, 'member') AS my_role
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
            is_public: r.get::<i64, _>("is_public") != 0,
            my_role: r.get::<String, _>("my_role"),
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(servers)).into_response()
}


async fn discover(
    State(st): State<AppState>,
    me: AuthUser,
    Query(q): Query<DiscoverQuery>,
) -> impl IntoResponse {
    let db = &st.db;
    let query = q.q.unwrap_or_default().trim().to_string();
    let like = format!("%{}%", query);
    let limit = q.limit.unwrap_or(20).clamp(1, 50);

    let rows = sqlx::query(
        r#"
        SELECT
            s.id,
            s.name,
            s.owner_id,
            s.created_at,
            COALESCE(s.is_public, 1) AS is_public,
            (SELECT COUNT(1) FROM server_members sm WHERE sm.server_id = s.id) AS members_count
        FROM servers s
        WHERE (? = '' OR s.name LIKE ?)
          AND NOT EXISTS (
                SELECT 1 FROM server_members m
                WHERE m.server_id = s.id AND m.user_id = ?
          )
        ORDER BY members_count DESC, s.id DESC
        LIMIT ?
        "#,
    )
    .bind(&query)
    .bind(&like)
    .bind(me.id)
    .bind(limit)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let out = rows.into_iter().map(|r| DiscoverServerRow {
        id: r.get("id"),
        name: r.get("name"),
        owner_id: r.get("owner_id"),
        created_at: r.get("created_at"),
        is_public: r.get::<i64, _>("is_public") != 0,
        members_count: r.get::<i64, _>("members_count"),
    }).collect::<Vec<_>>();

    (StatusCode::OK, Json(out)).into_response()
}


async fn create_join_request(
    State(st): State<AppState>,
    me: AuthUser,
    Path(server_id): Path<i64>,
    Json(body): Json<JoinRequestBody>,
) -> impl IntoResponse {
    let db = &st.db;

    let row = sqlx::query(
        "SELECT id, COALESCE(is_public, 1) AS is_public FROM servers WHERE id = ? LIMIT 1",
    )
    .bind(server_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(r) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let is_public = r.get::<i64, _>("is_public") != 0;

    let already_member = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM server_members WHERE server_id = ? AND user_id = ? LIMIT 1",
    )
    .bind(server_id)
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some();

    if already_member {
        return (StatusCode::OK, Json(serde_json::json!({"status":"already_member"}))).into_response();
    }

    if is_public {
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
        return (StatusCode::OK, Json(serde_json::json!({ "status": "joined" }))).into_response();
    }

    let from_server_id = match body.from_server_id {
        Some(v) if v > 0 => {
            let allowed = sqlx::query_scalar::<_, i64>(
                "SELECT 1 FROM server_members WHERE server_id = ? AND user_id = ? LIMIT 1",
            )
            .bind(v)
            .bind(me.id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten()
            .is_some();
            if allowed { Some(v) } else { None }
        }
        _ => None,
    };

    let now = auth::now_iso();
    let res = sqlx::query(
        r#"
        INSERT INTO server_join_requests(server_id, requester_id, from_server_id, status, created_at)
        VALUES(?, ?, ?, 'pending', ?)
        ON CONFLICT(server_id, requester_id) DO UPDATE SET
            from_server_id = excluded.from_server_id,
            status = 'pending',
            created_at = excluded.created_at,
            decided_at = NULL,
            decided_by = NULL
        "#,
    )
    .bind(server_id)
    .bind(me.id)
    .bind(from_server_id)
    .bind(&now)
    .execute(db)
    .await;

    if res.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let request_id = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM server_join_requests WHERE server_id = ? AND requester_id = ? LIMIT 1",
    )
    .bind(server_id)
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    if let Some(request_id) = request_id {
        if let Some(status) = crate::ai_client::auto_decide_server_join_request_if_ai(st.clone(), request_id).await {
            let response_status = if status == "accepted" { "joined".to_string() } else { status.clone() };
            return (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": response_status,
                    "ai_decided": true,
                    "ai_request_status": status
                })),
            )
                .into_response();
        }
    }

    (StatusCode::OK, Json(serde_json::json!({ "status": "pending" }))).into_response()
}

async fn list_incoming_join_requests(
    State(st): State<AppState>,
    me: AuthUser,
) -> impl IntoResponse {
    let db = &st.db;

    let rows = sqlx::query(
        r#"
        SELECT
            r.id,
            r.server_id,
            s.name AS server_name,
            COALESCE(s.is_public, 1) AS server_is_public,
            r.requester_id,
            u.username AS requester_username,
            up.avatar_file_id AS requester_avatar_file_id,
            r.from_server_id,
            fs.name AS from_server_name,
            r.status,
            r.created_at
        FROM server_join_requests r
        JOIN servers s ON s.id = r.server_id
        JOIN users u ON u.id = r.requester_id
        LEFT JOIN user_profile up ON up.user_id = u.id
        LEFT JOIN servers fs ON fs.id = r.from_server_id
        JOIN server_members sm ON sm.server_id = r.server_id AND sm.user_id = ?
        WHERE r.status = 'pending'
          AND (s.owner_id = ? OR COALESCE(sm.role, 'member') = 'admin')
        ORDER BY r.id DESC
        LIMIT 100
        "#,
    )
    .bind(me.id)
    .bind(me.id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let out = rows.into_iter().map(join_request_from_row).collect::<Vec<_>>();
    (StatusCode::OK, Json(out)).into_response()
}

async fn list_outgoing_join_requests(
    State(st): State<AppState>,
    me: AuthUser,
) -> impl IntoResponse {
    let db = &st.db;

    let rows = sqlx::query(
        r#"
        SELECT
            r.id,
            r.server_id,
            s.name AS server_name,
            COALESCE(s.is_public, 1) AS server_is_public,
            r.requester_id,
            u.username AS requester_username,
            up.avatar_file_id AS requester_avatar_file_id,
            r.from_server_id,
            fs.name AS from_server_name,
            r.status,
            r.created_at
        FROM server_join_requests r
        JOIN servers s ON s.id = r.server_id
        JOIN users u ON u.id = r.requester_id
        LEFT JOIN user_profile up ON up.user_id = u.id
        LEFT JOIN servers fs ON fs.id = r.from_server_id
        WHERE r.requester_id = ?
        ORDER BY r.id DESC
        LIMIT 100
        "#,
    )
    .bind(me.id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let out = rows.into_iter().map(join_request_from_row).collect::<Vec<_>>();
    (StatusCode::OK, Json(out)).into_response()
}

fn join_request_from_row(r: sqlx::sqlite::SqliteRow) -> JoinRequestView {
    JoinRequestView {
        id: r.get("id"),
        server_id: r.get("server_id"),
        server_name: r.get("server_name"),
        server_is_public: r.get::<i64, _>("server_is_public") != 0,
        requester_id: r.get("requester_id"),
        requester_username: r.get("requester_username"),
        requester_avatar_file_id: r.try_get("requester_avatar_file_id").ok(),
        from_server_id: r.try_get("from_server_id").ok(),
        from_server_name: r.try_get("from_server_name").ok(),
        status: r.get("status"),
        created_at: r.get("created_at"),
    }
}

async fn accept_join_request(
    State(st): State<AppState>,
    me: AuthUser,
    Path(request_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    let row = sqlx::query(
        r#"
        SELECT r.id, r.server_id, r.requester_id
        FROM server_join_requests r
        JOIN servers s ON s.id = r.server_id
        LEFT JOIN server_members sm ON sm.server_id = r.server_id AND sm.user_id = ?
        WHERE r.id = ? AND r.status = 'pending'
          AND (s.owner_id = ? OR COALESCE(sm.role, 'member') = 'admin')
        LIMIT 1
        "#,
    )
    .bind(me.id)
    .bind(request_id)
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(r) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let server_id: i64 = r.get("server_id");
    let requester_id: i64 = r.get("requester_id");
    let now = auth::now_iso();

    let _ = sqlx::query(
        "INSERT OR IGNORE INTO server_members(server_id, user_id) VALUES(?, ?)",
    )
    .bind(server_id)
    .bind(requester_id)
    .execute(db)
    .await;

    let _ = sqlx::query(
        "UPDATE server_join_requests SET status = 'accepted', decided_at = ?, decided_by = ? WHERE id = ?",
    )
    .bind(&now)
    .bind(me.id)
    .bind(request_id)
    .execute(db)
    .await;

    (StatusCode::OK, Json(serde_json::json!({"status":"accepted"}))).into_response()
}

async fn reject_join_request(
    State(st): State<AppState>,
    me: AuthUser,
    Path(request_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    let row = sqlx::query(
        r#"
        SELECT r.id
        FROM server_join_requests r
        JOIN servers s ON s.id = r.server_id
        LEFT JOIN server_members sm ON sm.server_id = r.server_id AND sm.user_id = ?
        WHERE r.id = ? AND r.status = 'pending'
          AND (s.owner_id = ? OR COALESCE(sm.role, 'member') = 'admin')
        LIMIT 1
        "#,
    )
    .bind(me.id)
    .bind(request_id)
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    if row.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }

    let now = auth::now_iso();
    let _ = sqlx::query(
        "UPDATE server_join_requests SET status = 'rejected', decided_at = ?, decided_by = ? WHERE id = ?",
    )
    .bind(&now)
    .bind(me.id)
    .bind(request_id)
    .execute(db)
    .await;

    (StatusCode::OK, Json(serde_json::json!({"status":"rejected"}))).into_response()
}

async fn invite_member(
    State(st): State<AppState>,
    me: AuthUser,
    Path(server_id): Path<i64>,
    Json(body): Json<InviteBody>,
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

    if !can_manage_channels(db, server_id, me.id).await {
        return StatusCode::FORBIDDEN.into_response();
    }

    let username = body.username.trim();
    if username.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"detail":"bad_username"}))).into_response();
    }

    let user_id = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM users WHERE username = ? AND is_banned = 0 LIMIT 1",
    )
    .bind(username)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(user_id) = user_id else {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({"detail":"user_not_found"}))).into_response();
    };

    let _ = sqlx::query(
        "INSERT OR IGNORE INTO server_members(server_id, user_id) VALUES(?, ?)",
    )
    .bind(server_id)
    .bind(user_id)
    .execute(db)
    .await;

    (StatusCode::OK, Json(serde_json::json!({"status":"ok", "user_id": user_id}))).into_response()
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
            u.public_encryption_key as public_encryption_key,
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
            public_encryption_key: r.try_get::<Option<String>, _>("public_encryption_key").ok().flatten(),
            role: r.get::<String, _>("role"),
            is_online: r.get::<i64, _>("is_online") != 0,
            status: r.get::<String, _>("status"),
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(members)).into_response()
}

async fn update_server(
    State(st): State<AppState>,
    me: AuthUser,
    Path(server_id): Path<i64>,
    Json(body): Json<UpdateServerBody>,
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

    if let Some(name) = body.name {
        let name = name.trim().chars().take(80).collect::<String>();
        if name.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"detail":"server_name_required"})),
            )
                .into_response();
        }
        if sqlx::query("UPDATE servers SET name = ? WHERE id = ?")
            .bind(&name)
            .bind(server_id)
            .execute(db)
            .await
            .is_err()
        {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    if let Some(is_public) = body.is_public {
        if sqlx::query("UPDATE servers SET is_public = ? WHERE id = ?")
            .bind(if is_public { 1 } else { 0 })
            .bind(server_id)
            .execute(db)
            .await
            .is_err()
        {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "id": server_id,
            "status": "ok"
        })),
    )
        .into_response()
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
