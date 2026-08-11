use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;

use crate::{auth, server::AppState};
use crate::middleware::auth_guard::AuthUser;

#[derive(Deserialize)]
pub struct CreateChatBody {
    pub name: Option<String>,
    pub server_id: Option<i64>,
    pub is_private: Option<bool>,
    pub participant_ids: Option<Vec<i64>>,
}

#[derive(Serialize)]
pub struct ChatRow {
    pub id: i64,
    pub name: Option<String>,
    pub server_id: Option<i64>,
    pub is_private: i64,
    pub created_at: String,

    // text/voice
    pub kind: String,

    // computed for list
    pub unread_count: i64,
    pub has_unread: bool,
    pub last_message_id: Option<i64>,
    pub last_read_message_id: Option<i64>,
    pub last_message_preview: Option<String>,
}


pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create).get(list_my))
        .route("/{chat_id}", get(get_one))
        .route("/{chat_id}/join", post(join))
        .route("/{chat_id}/read", post(mark_read))
        .route("/{chat_id}/pins", get(list_pins))// join теперь безопасный
}

fn default_settings_json() -> Value {
    serde_json::json!({
        "dms": "friends_and_server"
    })
}

async fn get_user_settings_json(db: &sqlx::PgPool, user_id: i64) -> Value {
    let row = sqlx::query("SELECT settings_json FROM user_settings WHERE user_id = $1 LIMIT 1")
        .bind(user_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let Some(r) = row else {
        return default_settings_json();
    };

    let raw: String = r.get("settings_json");
    serde_json::from_str::<Value>(&raw).unwrap_or_else(|_| default_settings_json())
}

async fn get_user_dm_mode(db: &sqlx::PgPool, user_id: i64) -> String {
    let s = get_user_settings_json(db, user_id).await;
    s.get("dms")
        .and_then(|v| v.as_str())
        .unwrap_or("friends_and_server")
        .to_string()
}

async fn are_friends(db: &sqlx::PgPool, a: i64, b: i64) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT 1::bigint FROM friendships WHERE user_id = $1 AND friend_id = $2 LIMIT 1",
    )
    .bind(a)
    .bind(b)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some()
}

async fn share_server(db: &sqlx::PgPool, a: i64, b: i64) -> bool {
    sqlx::query_scalar::<_, i64>(
        r#"
        SELECT 1::bigint
        FROM server_members s1
        JOIN server_members s2 ON s1.server_id = s2.server_id
        WHERE s1.user_id = $1 AND s2.user_id = $2
        LIMIT 1
        "#,
    )
    .bind(a)
    .bind(b)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some()
}

async fn create(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<CreateChatBody>,
) -> impl IntoResponse {
    let db = &st.db;
    let created_at = auth::now_iso();
    let is_private = body.is_private.unwrap_or(false);

    // privacy: private chats (DMs)
    if is_private {
        if let Some(ids) = body.participant_ids.as_ref() {
            for uid in ids.iter().copied() {
                if uid == me.id {
                    continue;
                }

                // ensure user exists & not banned (also avoids leaking)
                let ok_user = sqlx::query_scalar::<_, i64>(
                    "SELECT 1::bigint FROM users WHERE id = $1 AND NOT is_banned LIMIT 1",
                )
                .bind(uid)
                .fetch_optional(db)
                .await
                .ok()
                .flatten()
                .is_some();

                if !ok_user {
                    return StatusCode::FORBIDDEN.into_response();
                }

                let mode = get_user_dm_mode(db, uid).await;
                let allowed = match mode.as_str() {
                    "friends_only" => are_friends(db, me.id, uid).await,
                    "friends_and_server" => {
                        are_friends(db, me.id, uid).await || share_server(db, me.id, uid).await
                    }
                    _ => true,
                };

                if !allowed {
                    return (
                        StatusCode::FORBIDDEN,
                        Json(serde_json::json!({"detail":"DMs are restricted by user settings"})),
                    )
                        .into_response();
                }
            }
        }
    }

    // server chat: creator MUST be server member
    if let Some(server_id) = body.server_id {
        let member = sqlx::query_scalar::<_, i64>(
            "SELECT 1::bigint FROM server_members WHERE server_id = $1 AND user_id = $2 LIMIT 1",
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
    }

    let res = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO chats(name, server_id, is_private, created_at)
        VALUES($1, $2, $3, $4) RETURNING id
        "#,
    )
    .bind(&body.name)
    .bind(body.server_id)
    .bind(is_private)
    .bind(&created_at)
    .fetch_one(db)
    .await;

    let Ok(chat_id) = res else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    // creator is always participant
    let _ = sqlx::query(
        r#"INSERT INTO chat_participants(chat_id, user_id)
           VALUES($1, $2)"#,
    )
    .bind(chat_id)
    .bind(me.id)
    .execute(db)
    .await;

    // private chat: explicitly listed participants only
    if is_private {
        if let Some(ids) = body.participant_ids {
            for uid in ids {
                // skip creator duplication
                if uid == me.id {
                    continue;
                }

                // ensure user exists & not banned
                let ok = sqlx::query_scalar::<_, i64>(
                    "SELECT 1::bigint FROM users WHERE id = $1 AND NOT is_banned LIMIT 1",
                )
                .bind(uid)
                .fetch_optional(db)
                .await
                .ok()
                .flatten()
                .is_some();

                if ok {
                    let _ = sqlx::query(
                        r#"INSERT INTO chat_participants(chat_id, user_id)
                           VALUES($1, $2) ON CONFLICT DO NOTHING"#,
                    )
                    .bind(chat_id)
                    .bind(uid)
                    .execute(db)
                    .await;
                }
            }
        }
    }

    (StatusCode::OK, Json(serde_json::json!({ "id": chat_id }))).into_response()
}

async fn join(
    State(st): State<AppState>,
    me: AuthUser,
    Path(chat_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    let row = sqlx::query(
        "SELECT is_private, server_id FROM chats WHERE id = $1 LIMIT 1",
    )
    .bind(chat_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(r) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let is_private: bool = r.get("is_private");
    let server_id: Option<i64> = r.get("server_id");

    // ❌ private chats cannot be joined
    if is_private {
        return StatusCode::FORBIDDEN.into_response();
    }

    // server chat: must be server member
    if let Some(sid) = server_id {
        let member = sqlx::query_scalar::<_, i64>(
            "SELECT 1::bigint FROM server_members WHERE server_id = $1 AND user_id = $2 LIMIT 1",
        )
        .bind(sid)
        .bind(me.id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .is_some();

        if !member {
            return StatusCode::FORBIDDEN.into_response();
        }
    }

    let _ = sqlx::query(
        r#"INSERT INTO chat_participants(chat_id, user_id)
           VALUES($1, $2) ON CONFLICT DO NOTHING"#,
    )
    .bind(chat_id)
    .bind(me.id)
    .execute(db)
    .await;

    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
}

async fn get_one(
    State(st): State<AppState>,
    me: AuthUser,
    Path(chat_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;
    #[derive(sqlx::FromRow)]
    struct ChatMeta {
        server_id: Option<i64>,
        is_private: bool,
        kind: Option<String>,
    }

    let meta: Option<ChatMeta> = sqlx::query_as("SELECT server_id, is_private, kind FROM chats WHERE id = $1 LIMIT 1")
        .bind(chat_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let Some(meta) = meta else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let kind = meta.kind.unwrap_or_else(|| "text".to_string());

    // access: private -> participants; server public -> server_members
    let member = if meta.is_private {
        sqlx::query_scalar::<_, i64>(
            "SELECT 1::bigint FROM chat_participants WHERE chat_id = $1 AND user_id = $2 LIMIT 1",
        )
        .bind(chat_id)
        .bind(me.id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .is_some()
    } else if let Some(sid) = meta.server_id {
        sqlx::query_scalar::<_, i64>(
            "SELECT 1::bigint FROM server_members WHERE server_id = $1 AND user_id = $2 LIMIT 1",
        )
        .bind(sid)
        .bind(me.id)
        .fetch_optional(db)
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

    // voice text: only while user is in this voice channel
    if kind == "voice" && st.hub.voice_get_user_channel(me.id) != Some(chat_id) {
        return StatusCode::FORBIDDEN.into_response();
    }


    let row = sqlx::query(
        r#"
        SELECT id, name, server_id, is_private, created_at, kind
        FROM chats
        WHERE id = $1
        LIMIT 1
        "#,
    )
    .bind(chat_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(r) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let chat = ChatRow {
        id: r.get("id"),
        name: r.get("name"),
        server_id: r.get("server_id"),
        is_private: r.get::<bool, _>("is_private") as i64,
        created_at: r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
        kind: r.get::<String, _>("kind"),
        unread_count: 0,
        has_unread: false,
        last_message_id: None,
        last_message_preview: None,
        last_read_message_id: None,
    };

    (StatusCode::OK, Json(chat)).into_response()
}



#[derive(Serialize)]
pub struct PinnedItem {
    pub message_id: i64,
    pub pinned_by: i64,
    pub pinned_by_username: String,
    pub pinned_at: String,
    pub message_exists: bool,
    pub sender_id: Option<i64>,
    pub sender_username: Option<String>,
    pub sender_avatar_file_id: Option<i64>,
    pub content: Option<String>,
}

async fn list_pins(
    State(st): State<AppState>,
    me: AuthUser,
    Path(chat_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    // Access rules must match the rest of the API:
    // - server chat -> server_members
    // - private chat -> chat_participants
    #[derive(sqlx::FromRow)]
    struct ChatInfo {
        server_id: Option<i64>,
        is_private: bool,
    }

    let chat: Option<ChatInfo> = sqlx::query_as("SELECT server_id, is_private FROM chats WHERE id = $1")
        .bind(chat_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let Some(chat) = chat else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let allowed = if let Some(server_id) = chat.server_id {
        sqlx::query_scalar::<_, i64>(
            "SELECT 1::bigint FROM server_members WHERE server_id = $1 AND user_id = $2 LIMIT 1",
        )
        .bind(server_id)
        .bind(me.id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .is_some()
    } else {
        // Support both current and legacy DM/private chat rows.
        let in_participants = sqlx::query_scalar::<_, i64>(
            "SELECT 1::bigint FROM chat_participants WHERE chat_id = $1 AND user_id = $2 LIMIT 1",
        )
        .bind(chat_id)
        .bind(me.id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .is_some();

        if in_participants {
            true
        } else {
            sqlx::query_scalar::<_, i64>(
                "SELECT 1::bigint FROM dm_chats WHERE chat_id = $1 AND (user_a = $2 OR user_b = $3) LIMIT 1",
            )
            .bind(chat_id)
            .bind(me.id)
            .bind(me.id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten()
            .is_some()
        }
    };

    if !allowed {
        return StatusCode::FORBIDDEN.into_response();
    }

    let rows = sqlx::query(
        r#"
        SELECT
            pm.message_id,
            pm.pinned_by,
            pu.username AS pinned_by_username,
            pm.pinned_at,
            m.id AS message_exists_id,
            m.sender_id,
            su.username AS sender_username,
            sup.avatar_file_id AS sender_avatar_file_id,
            m.content
        FROM pinned_messages pm
        JOIN users pu ON pu.id = pm.pinned_by
        LEFT JOIN messages m ON m.id = pm.message_id AND m.chat_id = pm.chat_id
        LEFT JOIN users su ON su.id = m.sender_id
        LEFT JOIN user_profile sup ON sup.user_id = su.id
        WHERE pm.chat_id = $1
        ORDER BY pm.pinned_at DESC
        LIMIT 100
        "#,
    )
    .bind(chat_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let out = rows
        .into_iter()
        .map(|r| PinnedItem {
            message_id: r.get("message_id"),
            pinned_by: r.get("pinned_by"),
            pinned_by_username: r.get("pinned_by_username"),
            pinned_at: r.get::<chrono::DateTime<chrono::Utc>, _>("pinned_at").to_rfc3339(),
            message_exists: r.try_get::<i64, _>("message_exists_id").ok().is_some(),
            sender_id: r.try_get::<i64, _>("sender_id").ok(),
            sender_username: r.try_get::<String, _>("sender_username").ok(),
            sender_avatar_file_id: r.try_get::<i64, _>("sender_avatar_file_id").ok(),
            content: r.try_get::<String, _>("content").ok(),
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(out)).into_response()
}

#[derive(Deserialize)]
pub struct MarkReadBody {
    pub last_read_message_id: Option<i64>,
}

async fn mark_read(
    State(st): State<AppState>,
    me: AuthUser,
    Path(chat_id): Path<i64>,
    Json(body): Json<MarkReadBody>,
) -> impl IntoResponse {
    let db = &st.db;

    let access = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT 1::bigint
        FROM chats c
        WHERE c.id = $1
          AND (
            EXISTS (
                SELECT 1::bigint
                FROM chat_participants p
                WHERE p.chat_id = c.id AND p.user_id = $2
            )
            OR (
                c.server_id IS NOT NULL AND c.is_private = FALSE AND EXISTS (
                    SELECT 1::bigint
                    FROM server_members sm
                    WHERE sm.server_id = c.server_id AND sm.user_id = $3
                )
            )
          )
        LIMIT 1
        "#,
    )
    .bind(chat_id)
    .bind(me.id)
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some();

    if !access {
        return StatusCode::FORBIDDEN.into_response();
    }

    let max_id = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(MAX(id), 0) FROM messages WHERE chat_id = $1",
    )
    .bind(chat_id)
    .fetch_one(db)
    .await
    .unwrap_or(0);

    let requested = body.last_read_message_id.unwrap_or(max_id).max(0).min(max_id);

    let last_read = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(MAX(id), 0) FROM messages WHERE chat_id = $1 AND id <= $2",
    )
    .bind(chat_id)
    .bind(requested)
    .fetch_one(db)
    .await
    .unwrap_or(0);

    let now = auth::now_iso();
    let q = sqlx::query(
        r#"
        INSERT INTO chat_reads(chat_id, user_id, last_read_message_id, updated_at)
        VALUES($1, $2, $3, $4)
        ON CONFLICT(chat_id, user_id) DO UPDATE SET
            last_read_message_id = excluded.last_read_message_id,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(chat_id)
    .bind(me.id)
    .bind(last_read)
    .bind(&now)
    .execute(db)
    .await;

    if q.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    (StatusCode::OK, Json(serde_json::json!({"chat_id": chat_id, "last_read_message_id": last_read}))).into_response()
}

async fn list_my(
    State(st): State<AppState>,
    me: AuthUser,
) -> impl IntoResponse {
    let db = &st.db;

    let rows = sqlx::query(
        r#"
        SELECT
            c.id,
            c.name,
            c.server_id,
            c.is_private,
            c.created_at,
            c.kind,
            (
                SELECT m.id
                FROM messages m
                WHERE m.chat_id = c.id
                ORDER BY m.id DESC
                LIMIT 1
            ) AS last_message_id,
            (
                SELECT substring(m.content, 1, 120)
                FROM messages m
                WHERE m.chat_id = c.id
                ORDER BY m.id DESC
                LIMIT 1
            ) AS last_message_preview,
            (
                SELECT COALESCE(MAX(m.id), 0)
                FROM messages m
                WHERE m.chat_id = c.id
                  AND m.id <= COALESCE((
                    SELECT r.last_read_message_id
                    FROM chat_reads r
                    WHERE r.chat_id = c.id AND r.user_id = $1
                    LIMIT 1
                  ), 0)
            ) AS last_read_message_id,
            CASE WHEN EXISTS(
                SELECT 1::bigint
                FROM messages m
                WHERE m.chat_id = c.id
                  AND m.id > COALESCE((
                    SELECT r.last_read_message_id
                    FROM chat_reads r
                    WHERE r.chat_id = c.id AND r.user_id = $2
                    LIMIT 1
                  ), 0)
                LIMIT 1
            ) THEN 1 ELSE 0 END AS unread_count
        FROM chats c
        JOIN chat_participants p ON p.chat_id = c.id
        WHERE p.user_id = $3
        ORDER BY COALESCE(last_message_id, c.id) DESC
        "#,
    )
    .bind(me.id)
    .bind(me.id)
    .bind(me.id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let out = rows
        .into_iter()
        .map(|r| ChatRow {
            id: r.get("id"),
            name: r.get("name"),
            server_id: r.get("server_id"),
            is_private: r.get::<bool, _>("is_private") as i64,
            created_at: r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
            kind: r.get::<String, _>("kind"),
            unread_count: r.get::<i64, _>("unread_count"),
            has_unread: r.get::<i64, _>("unread_count") > 0,
            last_message_id: r.try_get("last_message_id").ok(),
            last_read_message_id: r.try_get("last_read_message_id").ok(),
            last_message_preview: r.try_get("last_message_preview").ok(),
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(out)).into_response()
}
