use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::QueryBuilder;
use std::collections::{HashMap, HashSet};

use crate::{auth, server::AppState};
use crate::middleware::auth_guard::AuthUser;
use crate::ws::RoomId;

#[derive(Serialize)]
pub struct DmChatView {
    pub chat_id: i64,
    pub other_user_id: i64,
    pub other_username: String,
    pub last_message_id: Option<i64>,
    pub last_message_at: Option<String>,
    pub last_message_preview: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct SendMessageBody {
    pub content: String,
    pub reply_to_id: Option<i64>,
}

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
    pub reactions: Option<Vec<crate::routes::messages::ReactionItem>>,
}

#[derive(Serialize)]
pub struct ReplyPreview {
    pub id: i64,
    pub sender_id: i64,
    pub sender_username: String,
    pub sender_avatar_file_id: Option<i64>,
    pub content: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_my))
        .route("/with/:user_id", post(get_or_create_with))
        .route("/:chat_id/messages", get(list_messages).post(send_message))
}

async fn is_blocked_pair(db: &sqlx::SqlitePool, a: i64, b: i64) -> bool {
    sqlx::query_scalar::<_, i64>(
        r#"
        SELECT 1
        FROM user_blocks
        WHERE (blocker_id = ? AND blocked_id = ?)
           OR (blocker_id = ? AND blocked_id = ?)
        LIMIT 1
        "#,
    )
    .bind(a)
    .bind(b)
    .bind(b)
    .bind(a)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some()
}

async fn can_dm(db: &sqlx::SqlitePool, me: i64, other: i64) -> bool {
    let row = sqlx::query("SELECT settings_json FROM user_settings WHERE user_id = ? LIMIT 1")
        .bind(other)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let mode = row
        .and_then(|r| r.try_get::<String, _>("settings_json").ok())
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.get("dms").and_then(|x| x.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| "friends_and_server".to_string());

    let are_friends = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM friendships WHERE user_id = ? AND friend_id = ? LIMIT 1",
    )
    .bind(me)
    .bind(other)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some();

    let share_server = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT 1
        FROM server_members s1
        JOIN server_members s2 ON s1.server_id = s2.server_id
        WHERE s1.user_id = ? AND s2.user_id = ?
        LIMIT 1
        "#,
    )
    .bind(me)
    .bind(other)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some();

    match mode.as_str() {
        "friends_only" => are_friends,
        "friends_and_server" => are_friends || share_server,
        _ => true,
    }
}

async fn get_or_create_with(
    State(st): State<AppState>,
    me: AuthUser,
    Path(other_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    if other_id == me.id {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"detail":"Cannot DM yourself"})),
        )
            .into_response();
    }

    let other_row = sqlx::query("SELECT username, is_banned FROM users WHERE id = ? LIMIT 1")
        .bind(other_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let Some(orow) = other_row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let is_banned: i64 = orow.get("is_banned");
    if is_banned != 0 {
        return StatusCode::FORBIDDEN.into_response();
    }

    if is_blocked_pair(db, me.id, other_id).await {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"detail":"Blocked"})),
        )
            .into_response();
    }

    if !can_dm(db, me.id, other_id).await {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"detail":"DMs are restricted by user settings"})),
        )
            .into_response();
    }

    let (a, b) = if me.id < other_id { (me.id, other_id) } else { (other_id, me.id) };

    if let Some(chat_id) = sqlx::query_scalar::<_, i64>(
        "SELECT chat_id FROM dm_chats WHERE user_a = ? AND user_b = ? LIMIT 1",
    )
    .bind(a)
    .bind(b)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    {
        return (StatusCode::OK, Json(serde_json::json!({"chat_id": chat_id}))).into_response();
    }

    let created_at = auth::now_iso();

    let res = sqlx::query(
        r#"INSERT INTO chats(name, server_id, is_private, created_at) VALUES(NULL, NULL, 1, ?)"#,
    )
    .bind(&created_at)
    .execute(db)
    .await;

    let Ok(r) = res else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    let chat_id = r.last_insert_rowid();

    let _ = sqlx::query("INSERT OR IGNORE INTO chat_participants(chat_id, user_id) VALUES(?, ?)")
        .bind(chat_id)
        .bind(me.id)
        .execute(db)
        .await;
    let _ = sqlx::query("INSERT OR IGNORE INTO chat_participants(chat_id, user_id) VALUES(?, ?)")
        .bind(chat_id)
        .bind(other_id)
        .execute(db)
        .await;

    let _ = sqlx::query(
        "INSERT INTO dm_chats(chat_id, user_a, user_b, created_at) VALUES(?, ?, ?, ?)",
    )
    .bind(chat_id)
    .bind(a)
    .bind(b)
    .bind(&created_at)
    .execute(db)
    .await;

    (StatusCode::OK, Json(serde_json::json!({"chat_id": chat_id}))).into_response()
}

async fn list_my(
    State(st): State<AppState>,
    me: AuthUser,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    let db = &st.db;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);

    let mut rows = sqlx::query(
        r#"
        SELECT d.chat_id,
               CASE WHEN d.user_a = ? THEN d.user_b ELSE d.user_a END AS other_user_id,
               u.username AS other_username,
               lm.id AS last_message_id,
               lm.timestamp AS last_message_at,
               substr(lm.content, 1, 80) AS last_message_preview
        FROM dm_chats d
        JOIN users u ON u.id = (CASE WHEN d.user_a = ? THEN d.user_b ELSE d.user_a END)
        LEFT JOIN messages lm
          ON lm.id = (
            SELECT m2.id FROM messages m2 WHERE m2.chat_id = d.chat_id ORDER BY m2.id DESC LIMIT 1
          )
        WHERE d.user_a = ? OR d.user_b = ?
        ORDER BY COALESCE(lm.id, 0) DESC
        LIMIT ?
        "#,
    )
    .bind(me.id)
    .bind(me.id)
    .bind(me.id)
    .bind(me.id)
    .bind(limit)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    if rows.is_empty() {
        rows = sqlx::query(
            r#"

        SELECT c.id AS chat_id,
               p2.user_id AS other_user_id,
               u.username AS other_username,
               lm.id AS last_message_id,
               lm.timestamp AS last_message_at,
               substr(lm.content, 1, 80) AS last_message_preview
        FROM chats c
        JOIN chat_participants p1 ON p1.chat_id = c.id AND p1.user_id = ?
        JOIN chat_participants p2 ON p2.chat_id = c.id AND p2.user_id <> ?
        JOIN users u ON u.id = p2.user_id
        LEFT JOIN messages lm
          ON lm.id = (
            SELECT m2.id FROM messages m2 WHERE m2.chat_id = c.id ORDER BY m2.id DESC LIMIT 1
          )
        WHERE c.is_private = 1
          AND c.server_id IS NULL
          AND (SELECT count(*) FROM chat_participants cp WHERE cp.chat_id = c.id) = 2
        ORDER BY COALESCE(lm.id, 0) DESC
        LIMIT ?
        "#,
        )
        .bind(me.id)
        .bind(me.id)
        .bind(limit)
        .fetch_all(db)
        .await
        .unwrap_or_default();
    }

    let out = rows
        .into_iter()
        .map(|r| DmChatView {
            chat_id: r.get("chat_id"),
            other_user_id: r.get("other_user_id"),
            other_username: r.get("other_username"),
            last_message_id: r.try_get("last_message_id").ok(),
            last_message_at: r.try_get("last_message_at").ok(),
            last_message_preview: r.try_get("last_message_preview").ok(),
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(out)).into_response()
}

async fn ensure_dm_participant(db: &sqlx::SqlitePool, chat_id: i64, user_id: i64) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM chat_participants WHERE chat_id = ? AND user_id = ? LIMIT 1",
    )
    .bind(chat_id)
    .bind(user_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some()
}

async fn list_messages(
    State(st): State<AppState>,
    me: AuthUser,
    Path(chat_id): Path<i64>,
) -> impl IntoResponse {
    let db = &st.db;

    if !ensure_dm_participant(db, chat_id, me.id).await {
        return StatusCode::FORBIDDEN.into_response();
    }

    let rows = sqlx::query(
        r#"
        SELECT m.id,
               m.chat_id,
               m.sender_id,
               u.username AS sender_username,
               up.avatar_file_id AS sender_avatar_file_id,
               m.content,
               m.timestamp,
               m.reply_to_message_id,
               ru.id AS r_id,
               ru.username AS r_sender_username,
               rup.avatar_file_id AS r_sender_avatar_file_id,
               rm.sender_id AS r_sender_id,
               rm.content AS r_content
        FROM messages m
        JOIN users u ON u.id = m.sender_id
        LEFT JOIN user_profile up ON up.user_id = u.id
        LEFT JOIN messages rm ON rm.id = m.reply_to_message_id
        LEFT JOIN users ru ON ru.id = rm.sender_id
        LEFT JOIN user_profile rup ON rup.user_id = ru.id
        WHERE m.chat_id = ?
        ORDER BY m.id DESC
        LIMIT 200
        "#,
    )
    .bind(chat_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let mut out = Vec::with_capacity(rows.len());

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
            r.try_get::<String, _>("r_sender_username").ok(),
            r.try_get::<i64, _>("r_sender_id").ok(),
            r.try_get::<i64, _>("r_sender_avatar_file_id").ok(),
            r.try_get::<String, _>("r_content").ok(),
        ) {
            (Some(id), Some(su), Some(sid), r_avatar, Some(rc)) => Some(ReplyPreview {
                id,
                sender_id: sid,
                sender_username: su,
                sender_avatar_file_id: r_avatar,
                content: rc.chars().take(80).collect(),
            }),
            _ => None,
        };

        out.push(MessageRow {
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

    out.reverse();

    // attach reactions summary
    if !out.is_empty() {
        let ids = out.iter().map(|m| m.id).collect::<Vec<_>>();

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

        for m in out.iter_mut() {
            let Some(list) = counts.get(&m.id) else { continue; };
            let mine_set = mine.get(&m.id);
            let items = list
                .iter()
                .map(|(e, c)| crate::routes::messages::ReactionItem {
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

    (StatusCode::OK, Json(out)).into_response()
}

async fn send_message(
    State(st): State<AppState>,
    me: AuthUser,
    Path(chat_id): Path<i64>,
    Json(body): Json<SendMessageBody>,
) -> impl IntoResponse {
    let db = &st.db;

    if !ensure_dm_participant(db, chat_id, me.id).await {
        return StatusCode::FORBIDDEN.into_response();
    }

    let other_id = sqlx::query_scalar::<_, i64>(
        "SELECT user_id FROM chat_participants WHERE chat_id = ? AND user_id != ? LIMIT 1",
    )
    .bind(chat_id)
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(other_id) = other_id else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    if is_blocked_pair(db, me.id, other_id).await {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"detail":"Blocked"})),
        )
            .into_response();
    }

    let content_trimmed = body.content.trim();
    if content_trimmed.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
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
    .bind(content_trimmed)
    .bind(&timestamp)
    .bind(body.reply_to_id)
    .execute(db)
    .await;

    let Ok(r) = res else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    let message_id = r.last_insert_rowid();

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
        "content": content_trimmed,
        "timestamp": timestamp,
        "reply_to_id": body.reply_to_id,
        "reply_preview": reply_preview
    });

    st.hub.broadcast_room(&RoomId::Channel(chat_id), &out);

    (
        StatusCode::OK,
        Json(serde_json::json!({ "id": message_id, "timestamp": timestamp })),
    )
        .into_response()
}
