use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, put},
    Json, Router,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::QueryBuilder;
use std::collections::{HashMap, HashSet};

use crate::auth;
use crate::middleware::auth_guard::AuthUser;
use crate::server::AppState;
use crate::ws::RoomId;

#[derive(Serialize)]
pub struct MessageRow {
    pub id: i64,
    pub chat_id: i64,
    pub sender_id: i64,
    pub sender_username: String,
    pub sender_avatar_file_id: Option<i64>,
    pub content: String,
    pub timestamp: String,
    pub reply_to_id: Option<i64>,
    pub reply_preview: Option<ReplyPreview>,
    pub reactions: Option<Vec<ReactionItem>>,
}

#[derive(Serialize)]
pub struct ReplyPreview {
    pub id: i64,
    pub sender_id: i64,
    pub sender_username: String,
    pub sender_avatar_file_id: Option<i64>,
    pub content: String,
}

#[derive(Deserialize)]
pub struct SendMessageBody {
    pub content: String,
    pub reply_to_id: Option<i64>,
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub before_id: Option<i64>,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/", get(list).post(send))
}

pub fn global_router() -> Router<AppState> {
    Router::new()
        .route("/:message_id", get(get_one))
        .route("/:message_id/reactions", get(get_reactions))
        .route("/:message_id/pin", put(pin_message).delete(unpin_message))
        .route(
            "/:message_id/reactions/:emoji",
            put(add_reaction).delete(remove_reaction),
        )
}

#[derive(Serialize)]
pub struct MessageOneResp {
    pub id: i64,
    pub chat_id: i64,
    pub sender_id: i64,
    pub sender_username: String,
    pub sender_avatar_file_id: Option<i64>,
    pub content: String,
    pub timestamp: String,
    pub reply_to_id: Option<i64>,
}

async fn get_one(
    State(st): State<AppState>,
    me: AuthUser,
    Path(message_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    let Ok(access) = can_access_message(&st, me.id, message_id).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let Some((_access_chat_id, _access_kind)) = access else {
        return StatusCode::FORBIDDEN.into_response();
    };

    let row = sqlx::query(
        r#"
        SELECT
            m.id,
            m.chat_id,
            m.sender_id,
            u.username AS sender_username,
            u.avatar_file_id AS sender_avatar_file_id,
            m.content,
            m.timestamp,
            m.reply_to_id
        FROM messages m
        JOIN users u ON u.id = m.sender_id
        WHERE m.id = ?
        LIMIT 1
        "#,
    )
    .bind(message_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(r) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let out = MessageOneResp {
        id: r.get("id"),
        chat_id: r.get("chat_id"),
        sender_id: r.get("sender_id"),
        sender_username: r.get("sender_username"),
        sender_avatar_file_id: r.try_get::<i64, _>("sender_avatar_file_id").ok(),
        content: r.get::<String, _>("content"),
        timestamp: r.get::<String, _>("timestamp"),
        reply_to_id: r.try_get::<i64, _>("reply_to_id").ok(),
    };

    (StatusCode::OK, Json(out)).into_response()
}

#[derive(Serialize)]
pub struct ReactionItem {
    pub emoji: String,
    pub count: i64,
    pub me: bool,
}

#[derive(Serialize)]
pub struct ReactionsResp {
    pub message_id: i64,
    pub items: Vec<ReactionItem>,
}

async fn can_access_message(
    st: &AppState,
    user_id: i64,
    message_id: i64,
) -> anyhow::Result<Option<(i64, String)>> {
    let db = &st.db;

    let row = sqlx::query(
        r#"
        SELECT c.id AS chat_id, c.server_id, c.is_private, COALESCE(c.kind, 'text') AS kind
        FROM messages m
        JOIN chats c ON c.id = m.chat_id
        WHERE m.id = ?
        LIMIT 1
        "#
    )
    .bind(message_id)
    .fetch_optional(db)
    .await?;

    let Some(r) = row else { return Ok(None); };

    let chat_id: i64 = r.get("chat_id");
    let server_id: Option<i64> = r.try_get("server_id").ok();
    let is_private: i64 = r.get("is_private");
    let kind: String = r.get("kind");

    let allowed = if let Some(sid) = server_id {
        sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM server_members WHERE server_id = ? AND user_id = ? LIMIT 1",
        )
        .bind(sid)
        .bind(user_id)
        .fetch_optional(db)
        .await?
        .is_some()
    } else if is_private != 0 {
        sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM chat_participants WHERE chat_id = ? AND user_id = ? LIMIT 1",
        )
        .bind(chat_id)
        .bind(user_id)
        .fetch_optional(db)
        .await?
        .is_some()
    } else {
        false
    };

    if !allowed {
        return Ok(None);
    }

    if kind == "voice" {
        if st.hub.voice_get_user_channel(user_id) != Some(chat_id) {
            return Ok(None);
        }
    }

    Ok(Some((chat_id, kind)))
}

async fn get_reactions(
    State(st): State<AppState>,
    me: AuthUser,
    Path(message_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    let Ok(access) = can_access_message(&st, me.id, message_id).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let Some((_access_chat_id, _access_kind)) = access else {
        return StatusCode::FORBIDDEN.into_response();
    };

    let mine = sqlx::query("SELECT emoji FROM message_reactions WHERE message_id = ? AND user_id = ?")
        .bind(message_id)
        .bind(me.id)
        .fetch_all(db)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| r.try_get::<String, _>("emoji").ok())
        .collect::<std::collections::HashSet<_>>();

    let rows = sqlx::query(
        r#"SELECT emoji, COUNT(*) as cnt FROM message_reactions WHERE message_id = ? GROUP BY emoji ORDER BY cnt DESC"#,
    )
    .bind(message_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let items = rows
        .into_iter()
        .map(|r| {
            let e: String = r.get("emoji");
            ReactionItem {
                emoji: e.clone(),
                count: r.get::<i64, _>("cnt"),
                me: mine.contains(&e),
            }
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(ReactionsResp { message_id, items })).into_response()
}

async fn add_reaction(
    State(st): State<AppState>,
    me: AuthUser,
    Path((message_id, emoji)): Path<(i64, String)>,
) -> impl IntoResponse {
    let db = &st.db;

    let Ok(access) = can_access_message(&st, me.id, message_id).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let Some((chat_id, kind)) = access else {
        return StatusCode::FORBIDDEN.into_response();
    };

    let e = emoji.trim();
    if e.is_empty() || e.len() > 32 {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let now = auth::now_iso();
    let q = sqlx::query(
        r#"INSERT OR IGNORE INTO message_reactions(message_id, user_id, emoji, created_at) VALUES(?, ?, ?, ?)"#,
    )
    .bind(message_id)
    .bind(me.id)
    .bind(e)
    .bind(now)
    .execute(db)
    .await;

    if q.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let out = serde_json::json!({
        "type": "reaction",
        "room_id": chat_id,
        "message_id": message_id,
        "emoji": e.to_string(),
        "user_id": me.id,
        "added": true
    });
    let room = if kind == "voice" {
        RoomId::Voice(chat_id)
    } else {
        RoomId::Channel(chat_id)
    };
    st.hub.broadcast_room(&room, &out);

    StatusCode::NO_CONTENT.into_response()
}

async fn remove_reaction(
    State(st): State<AppState>,
    me: AuthUser,
    Path((message_id, emoji)): Path<(i64, String)>,
) -> impl IntoResponse {
    let db = &st.db;

    let Ok(access) = can_access_message(&st, me.id, message_id).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let Some((chat_id, kind)) = access else {
        return StatusCode::FORBIDDEN.into_response();
    };

    let e = emoji.trim();
    if e.is_empty() || e.len() > 32 {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let q = sqlx::query("DELETE FROM message_reactions WHERE message_id = ? AND user_id = ? AND emoji = ?")
        .bind(message_id)
        .bind(me.id)
        .bind(e)
        .execute(db)
        .await;
    if q.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let out = serde_json::json!({
        "type": "reaction",
        "room_id": chat_id,
        "message_id": message_id,
        "emoji": e.to_string(),
        "user_id": me.id,
        "added": false
    });
    let room = if kind == "voice" {
        RoomId::Voice(chat_id)
    } else {
        RoomId::Channel(chat_id)
    };
    st.hub.broadcast_room(&room, &out);

    StatusCode::NO_CONTENT.into_response()
}

pub async fn list(
    State(st): State<AppState>,
    me: AuthUser,
    Path((server_id, chat_id)): Path<(i64, i64)>,
    Query(q): Query<ListQuery>,
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

    let meta = sqlx::query("SELECT server_id, is_private, COALESCE(kind,'text') AS kind FROM chats WHERE id = ? LIMIT 1")
        .bind(chat_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let Some(m) = meta else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let chat_server_id: Option<i64> = m.try_get("server_id").ok();
    let is_private: i64 = m.get("is_private");
    let kind: String = m.get("kind");

    if chat_server_id != Some(server_id) || is_private != 0 {
        return StatusCode::NOT_FOUND.into_response();
    }

    if kind == "voice" {
        if st.hub.voice_get_user_channel(me.id) != Some(chat_id) {
            return StatusCode::FORBIDDEN.into_response();
        }
    }

    let limit = q.limit.unwrap_or(50).clamp(1, 200);

    let rows = if let Some(before_id) = q.before_id {
        sqlx::query(
            r#"
            SELECT m.id,
                   m.chat_id,
                   m.sender_id,
                   u.username AS sender_username,
                   up.avatar_file_id AS sender_avatar_file_id,
                   m.content,
                   m.timestamp,
                   m.reply_to_message_id,
                   rm.id AS r_id,
                   rm.sender_id AS r_sender_id,
                   ru.username AS r_sender_username,
                   rup.avatar_file_id AS r_sender_avatar_file_id,
                   rm.content AS r_content
            FROM messages m
            JOIN users u ON u.id = m.sender_id
            LEFT JOIN user_profile up ON up.user_id = u.id
            LEFT JOIN messages rm ON rm.id = m.reply_to_message_id
            LEFT JOIN users ru ON ru.id = rm.sender_id
            LEFT JOIN user_profile rup ON rup.user_id = ru.id
            WHERE m.chat_id = ?
              AND m.id < ?
            ORDER BY m.id DESC
            LIMIT ?
            "#,
        )
        .bind(chat_id)
        .bind(before_id)
        .bind(limit)
        .fetch_all(db)
        .await
        .unwrap_or_default()
    } else {
        sqlx::query(
            r#"
            SELECT m.id,
                   m.chat_id,
                   m.sender_id,
                   u.username AS sender_username,
                   up.avatar_file_id AS sender_avatar_file_id,
                   m.content,
                   m.timestamp,
                   m.reply_to_message_id,
                   rm.id AS r_id,
                   rm.sender_id AS r_sender_id,
                   ru.username AS r_sender_username,
                   rup.avatar_file_id AS r_sender_avatar_file_id,
                   rm.content AS r_content
            FROM messages m
            JOIN users u ON u.id = m.sender_id
            LEFT JOIN user_profile up ON up.user_id = u.id
            LEFT JOIN messages rm ON rm.id = m.reply_to_message_id
            LEFT JOIN users ru ON ru.id = rm.sender_id
            LEFT JOIN user_profile rup ON rup.user_id = ru.id
            WHERE m.chat_id = ?
            ORDER BY m.id DESC
            LIMIT ?
            "#,
        )
        .bind(chat_id)
        .bind(limit)
        .fetch_all(db)
        .await
        .unwrap_or_default()
    };

    let mut messages = Vec::with_capacity(rows.len());

    for r in rows {
        let sender_id: i64 = r.get("sender_id");
        let sender_avatar_file_id: Option<i64> = r.try_get("sender_avatar_file_id").ok();
        let mut content: String = r.get("content");

        let blocked = sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM user_blocks WHERE blocker_id = ? AND blocked_id = ? LIMIT 1",
        )
        .bind(me.id)
        .bind(sender_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .is_some();
        if blocked {
            content = "[сообщение скрыто — пользователь заблокирован]".to_string();
        }

        let reply_to_id: Option<i64> = r.try_get("reply_to_message_id").ok();
        let reply_preview = match (
            r.try_get::<i64, _>("r_id").ok(),
            r.try_get::<i64, _>("r_sender_id").ok(),
            r.try_get::<String, _>("r_sender_username").ok(),
            r.try_get::<i64, _>("r_sender_avatar_file_id").ok(),
            r.try_get::<String, _>("r_content").ok(),
        ) {
            (Some(id), Some(rsid), Some(rsu), r_avatar, Some(rc)) => Some(ReplyPreview {
                id,
                sender_id: rsid,
                sender_username: rsu,
                sender_avatar_file_id: r_avatar,
                content: rc.chars().take(80).collect(),
            }),
            _ => None,
        };

        messages.push(MessageRow {
            id: r.get("id"),
            chat_id: r.get("chat_id"),
            sender_id,
            sender_username: r.get("sender_username"),
            sender_avatar_file_id,
            content,
            timestamp: r.get("timestamp"),
            reply_to_id,
            reply_preview,
            reactions: None,
        });
    }

    messages.reverse();
    if !messages.is_empty() {
        let ids = messages.iter().map(|m| m.id).collect::<Vec<_>>();

        let mut qb = QueryBuilder::<sqlx::Sqlite>::new(
            "SELECT message_id, emoji, COUNT(*) as cnt FROM message_reactions WHERE message_id IN (",
        );
        {
            let mut s = qb.separated(", ");
            for id in &ids {
                s.push_bind(id);
            }
        }
        qb.push(") GROUP BY message_id, emoji ORDER BY cnt DESC");
        let rows = qb.build().fetch_all(db).await.unwrap_or_default();

        let mut counts: HashMap<i64, Vec<(String, i64)>> = HashMap::new();
        for r in rows {
            let mid: i64 = r.get("message_id");
            let emoji: String = r.get("emoji");
            let cnt: i64 = r.get("cnt");
            counts.entry(mid).or_default().push((emoji, cnt));
        }

        let mut qb2 = QueryBuilder::<sqlx::Sqlite>::new(
            "SELECT message_id, emoji FROM message_reactions WHERE user_id = ",
        );
        qb2.push_bind(me.id);
        qb2.push(" AND message_id IN (");
        {
            let mut s = qb2.separated(", ");
            for id in &ids {
                s.push_bind(id);
            }
        }
        qb2.push(")");
        let rows2 = qb2.build().fetch_all(db).await.unwrap_or_default();

        let mut mine: HashMap<i64, HashSet<String>> = HashMap::new();
        for r in rows2 {
            let mid: i64 = r.get("message_id");
            let emoji: String = r.get("emoji");
            mine.entry(mid).or_default().insert(emoji);
        }

        for m in messages.iter_mut() {
            let Some(list) = counts.get(&m.id) else { continue; };
            let mine_set = mine.get(&m.id);
            let items = list
                .iter()
                .map(|(e, c)| ReactionItem {
                    emoji: e.clone(),
                    count: *c,
                    me: mine_set.map(|s| s.contains(e)).unwrap_or(false),
                })
                .collect::<Vec<_>>();

            if !items.is_empty() {
                m.reactions = Some(items);
            }
        }
    }

    if q.before_id.is_none() {
        if let Some(last) = messages.last() {
            let now = auth::now_iso();
            let _ = sqlx::query(
                r#"
                INSERT INTO chat_reads(chat_id, user_id, last_read_message_id, updated_at)
                VALUES(?, ?, ?, ?)
                ON CONFLICT(chat_id, user_id) DO UPDATE SET
                    last_read_message_id = excluded.last_read_message_id,
                    updated_at = excluded.updated_at
                "#,
            )
            .bind(chat_id)
            .bind(me.id)
            .bind(last.id)
            .bind(&now)
            .execute(db)
            .await;
        }
    }

    (StatusCode::OK, Json(messages)).into_response()
}

pub async fn send(
    State(st): State<AppState>,
    me: AuthUser,
    Path((server_id, chat_id)): Path<(i64, i64)>,
    Json(body): Json<SendMessageBody>,
) -> impl IntoResponse {
    let db = &st.db;

    let content_trimmed = body.content.trim();
    if content_trimmed.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let content = content_trimmed.to_string();
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

    let meta = sqlx::query("SELECT server_id, is_private, COALESCE(kind,'text') AS kind FROM chats WHERE id = ? LIMIT 1")
        .bind(chat_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let Some(m) = meta else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let chat_server_id: Option<i64> = m.try_get("server_id").ok();
    let is_private: i64 = m.get("is_private");
    let kind: String = m.get("kind");

    if chat_server_id != Some(server_id) || is_private != 0 {
        return StatusCode::NOT_FOUND.into_response();
    }

    if kind == "voice" {
        if st.hub.voice_get_user_channel(me.id) != Some(chat_id) {
            return StatusCode::FORBIDDEN.into_response();
        }
    }

    let timestamp = auth::now_iso();

    let res = sqlx::query(
        r#"
        INSERT INTO messages (chat_id, sender_id, content, timestamp, reply_to_message_id)
        VALUES (?, ?, ?, ?, ?)
        "#,
    )
    .bind(chat_id)
    .bind(me.id)
    .bind(&content)
    .bind(&timestamp)
    .bind(body.reply_to_id)
    .execute(db)
    .await;

    let Ok(r) = res else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    let message_id = r.last_insert_rowid();

    if let Ok(re) = Regex::new(r"\[\[file:(\d+)\|") {
        let mut file_ids: Vec<i64> = re
            .captures_iter(&content)
            .filter_map(|c| c.get(1).and_then(|m| m.as_str().parse::<i64>().ok()))
            .collect();
        file_ids.sort_unstable();
        file_ids.dedup();

        for fid in file_ids {
            let _ = sqlx::query(
                r#"
                UPDATE files
                SET message_id = ?
                WHERE id = ?
                  AND chat_id = ?
                  AND uploaded_by = ?
                  AND (message_id IS NULL OR message_id = 0)
                "#,
            )
            .bind(message_id)
            .bind(fid)
            .bind(chat_id)
            .bind(me.id)
            .execute(db)
            .await;
        }
    }

    let sender_avatar_file_id: Option<i64> = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT avatar_file_id FROM user_profile WHERE user_id = ? LIMIT 1",
    )
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .flatten();

    let reply_preview = if let Some(rid) = body.reply_to_id {
        let row = sqlx::query(
            r#"
            SELECT rm.id AS r_id,
                   rm.sender_id AS r_sender_id,
                   ru.username AS r_sender_username,
                   rup.avatar_file_id AS r_sender_avatar_file_id,
                   rm.content AS r_content
            FROM messages rm
            JOIN users ru ON ru.id = rm.sender_id
            LEFT JOIN user_profile rup ON rup.user_id = ru.id
            WHERE rm.id = ?
            LIMIT 1
            "#,
        )
        .bind(rid)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

        row.map(|r| {
            serde_json::json!({
                "id": r.get::<i64, _>("r_id"),
                "sender_id": r.get::<i64, _>("r_sender_id"),
                "sender_username": r.get::<String, _>("r_sender_username"),
                "sender_avatar_file_id": r.try_get::<i64, _>("r_sender_avatar_file_id").ok(),
                "content": r.get::<String, _>("r_content").chars().take(80).collect::<String>()
            })
        })
    } else {
        None
    };

    let out = serde_json::json!({
        "type": "message",
        "id": message_id,
        "room_id": chat_id,
        "sender_id": me.id,
        "sender_username": me.username,
        "sender_avatar_file_id": sender_avatar_file_id,
        "content": content,
        "timestamp": timestamp,
        "reply_to_id": body.reply_to_id,
        "reply_preview": reply_preview
    });

    let room = if kind == "voice" { RoomId::Voice(chat_id) } else { RoomId::Channel(chat_id) };
    st.hub.broadcast_room(&room, &out);

    (
        StatusCode::OK,
        Json(serde_json::json!({ "id": message_id, "timestamp": timestamp })),
    )
        .into_response()
}

async fn pin_message(
    State(st): State<AppState>,
    me: AuthUser,
    Path(message_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    let Ok(access) = can_access_message(&st, me.id, message_id).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let Some((chat_id, _kind)) = access else {
        return StatusCode::FORBIDDEN.into_response();
    };

    let now = auth::now_iso();

    let q = sqlx::query(
        r#"INSERT OR IGNORE INTO pinned_messages(chat_id, message_id, pinned_by, pinned_at)
           VALUES(?, ?, ?, ?)"#,
    )
    .bind(chat_id)
    .bind(message_id)
    .bind(me.id)
    .bind(&now)
    .execute(db)
    .await;

    if q.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    StatusCode::NO_CONTENT.into_response()
}

async fn unpin_message(
    State(st): State<AppState>,
    me: AuthUser,
    Path(message_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    let Ok(access) = can_access_message(&st, me.id, message_id).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let Some((chat_id, _kind)) = access else {
        return StatusCode::FORBIDDEN.into_response();
    };


    let q = sqlx::query("DELETE FROM pinned_messages WHERE chat_id = ? AND message_id = ?")
        .bind(chat_id)
        .bind(message_id)
        .execute(db)
        .await;

    if q.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    StatusCode::NO_CONTENT.into_response()
}
