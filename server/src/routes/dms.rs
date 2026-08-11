use regex::Regex;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::QueryBuilder;
use sqlx::Row;
use std::collections::{HashMap, HashSet};

use crate::{auth, server::AppState};
use crate::middleware::auth_guard::AuthUser;
use crate::ws::RoomId;

const MAX_MESSAGE_CHARS: usize = 65535;

fn encrypted_or_file_reference(content: &str) -> bool {
    content.starts_with("[[e2ee:v1|")
        || content.contains("[[file:")
        || content.contains("[[file=")
}

fn encrypted_message_required_response() -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "detail": "encrypted_message_required"
        })),
    )
        .into_response()
}

#[derive(Serialize)]
pub struct DmChatView {
    pub chat_id: i64,
    pub other_user_id: i64,
    pub other_username: String,
    pub other_avatar_file_id: Option<i64>,
    pub title: String,
    pub is_group: bool,
    pub member_count: i64,
    pub member_names: Vec<String>,
    pub last_message_id: Option<i64>,
    pub last_message_at: Option<String>,
    pub last_message_preview: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateGroupBody {
    pub name: Option<String>,
    pub user_ids: Vec<i64>,
}

#[derive(Serialize)]
pub struct DmParticipantView {
    pub id: i64,
    pub username: String,
    pub avatar_file_id: Option<i64>,
    pub public_encryption_key: Option<String>,
    pub is_me: bool,
    pub is_online: bool,
    pub status: String,
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub before_id: Option<i64>,
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
        .route("/groups", post(create_group))
        .route("/with/{user_id}", post(get_or_create_with))
        .route("/{chat_id}/participants", get(list_participants))
        .route("/{chat_id}/messages", get(list_messages).post(send_message))
}

async fn is_blocked_pair(db: &sqlx::PgPool, a: i64, b: i64) -> bool {
    sqlx::query_scalar::<_, i64>(
        r#"
        SELECT 1::bigint
        FROM user_blocks
        WHERE (blocker_id = $1 AND blocked_id = $2)
           OR (blocker_id = $3 AND blocked_id = $4)
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

async fn can_dm(db: &sqlx::PgPool, me: i64, other: i64) -> bool {
    let row = sqlx::query("SELECT settings_json FROM user_settings WHERE user_id = $1 LIMIT 1")
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
        "SELECT 1::bigint FROM friendships WHERE user_id = $1 AND friend_id = $2 LIMIT 1",
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
        SELECT 1::bigint
        FROM server_members s1
        JOIN server_members s2 ON s1.server_id = s2.server_id
        WHERE s1.user_id = $1 AND s2.user_id = $2
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

    let other_row = sqlx::query("SELECT username, is_banned FROM users WHERE id = $1 LIMIT 1")
        .bind(other_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let Some(orow) = other_row else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let is_banned: bool = orow.get("is_banned");
    if is_banned {
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
        "SELECT chat_id FROM dm_chats WHERE user_a = $1 AND user_b = $2 LIMIT 1",
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

    let res = sqlx::query_scalar::<_, i64>(
        r#"INSERT INTO chats(name, server_id, is_private, created_at) VALUES(NULL, NULL, TRUE, $1) RETURNING id"#,
    )
    .bind(created_at)
    .fetch_one(db)
    .await;

    let Ok(chat_id) = res else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    let _ = sqlx::query("INSERT INTO chat_participants(chat_id, user_id) VALUES($1, $2) ON CONFLICT DO NOTHING")
        .bind(chat_id)
        .bind(me.id)
        .execute(db)
        .await;
    let _ = sqlx::query("INSERT INTO chat_participants(chat_id, user_id) VALUES($1, $2) ON CONFLICT DO NOTHING")
        .bind(chat_id)
        .bind(other_id)
        .execute(db)
        .await;

    let _ = sqlx::query(
        "INSERT INTO dm_chats(chat_id, user_a, user_b, created_at) VALUES($1, $2, $3, $4)",
    )
    .bind(chat_id)
    .bind(a)
    .bind(b)
    .bind(created_at)
    .execute(db)
    .await;

    (StatusCode::OK, Json(serde_json::json!({"chat_id": chat_id}))).into_response()
}

fn normalize_group_title(raw: Option<String>, fallback_names: &[String]) -> String {
    let title = raw.unwrap_or_default().trim().to_string();
    if !title.is_empty() {
        return title.chars().take(80).collect::<String>();
    }

    let joined = fallback_names
        .iter()
        .take(4)
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let fallback = if joined.is_empty() {
        "Групповой чат".to_string()
    } else {
        joined
    };
    fallback.chars().take(80).collect::<String>()
}

async fn create_group(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<CreateGroupBody>,
) -> impl IntoResponse {
    let db = &st.db;

    let mut seen = HashSet::new();
    let mut user_ids = Vec::new();
    for id in body.user_ids {
        if id == me.id || id <= 0 || !seen.insert(id) {
            continue;
        }
        user_ids.push(id);
    }

    if user_ids.len() < 2 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"detail":"group_requires_at_least_two_other_users"})),
        )
            .into_response();
    }

    if user_ids.len() > 24 {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({"detail":"too_many_group_members","max_other_users":24})),
        )
            .into_response();
    }

    let mut users = Vec::<(i64, String)>::new();
    for uid in user_ids {
        let row = sqlx::query("SELECT username, is_banned FROM users WHERE id = $1 LIMIT 1")
            .bind(uid)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();

        let Some(r) = row else {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"detail":"unknown_group_member","user_id":uid})),
            )
                .into_response();
        };

        if r.get::<bool, _>("is_banned") {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"detail":"banned_group_member","user_id":uid})),
            )
                .into_response();
        }

        if is_blocked_pair(db, me.id, uid).await {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"detail":"blocked_group_member","user_id":uid})),
            )
                .into_response();
        }

        if !can_dm(db, me.id, uid).await {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"detail":"dm_restricted_group_member","user_id":uid})),
            )
                .into_response();
        }

        users.push((uid, r.get::<String, _>("username")));
    }

    users.sort_by_key(|(_, name)| name.to_lowercase());
    let names = users.iter().map(|(_, name)| name.clone()).collect::<Vec<_>>();
    let title = normalize_group_title(body.name, &names);
    let created_at = auth::now_iso();

    let res = sqlx::query_scalar::<_, i64>(
        "INSERT INTO chats(name, server_id, is_private, kind, created_at) VALUES($1, NULL, TRUE, 'text', $2) RETURNING id",
    )
    .bind(&title)
    .bind(created_at)
    .fetch_one(db)
    .await;

    let Ok(chat_id) = res else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let _ = sqlx::query("INSERT INTO chat_participants(chat_id, user_id) VALUES($1, $2) ON CONFLICT DO NOTHING")
        .bind(chat_id)
        .bind(me.id)
        .execute(db)
        .await;

    for (uid, _) in &users {
        let _ = sqlx::query("INSERT INTO chat_participants(chat_id, user_id) VALUES($1, $2) ON CONFLICT DO NOTHING")
            .bind(chat_id)
            .bind(uid)
            .execute(db)
            .await;
    }

    let mut member_names = Vec::with_capacity(users.len() + 1);
    member_names.push(me.username.clone());
    member_names.extend(users.iter().map(|(_, name)| name.clone()));

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "chat_id": chat_id,
            "title": title,
            "is_group": true,
            "member_count": member_names.len(),
            "member_names": member_names
        })),
    )
        .into_response()
}

async fn list_my(
    State(st): State<AppState>,
    me: AuthUser,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    let db = &st.db;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);

    let rows = sqlx::query(
        r#"
        SELECT c.id AS chat_id,
               COALESCE(c.name, '') AS chat_name,
               (SELECT COUNT(1) FROM chat_participants cp WHERE cp.chat_id = c.id) AS member_count,
               lm.id AS last_message_id,
               lm.timestamp AS last_message_at,
               substring(lm.content, 1, 80) AS last_message_preview
        FROM chats c
        JOIN chat_participants mep ON mep.chat_id = c.id AND mep.user_id = $1
        LEFT JOIN messages lm
          ON lm.id = (
            SELECT m2.id FROM messages m2 WHERE m2.chat_id = c.id ORDER BY m2.id DESC LIMIT 1
          )
        WHERE c.is_private = TRUE
          AND c.server_id IS NULL
        ORDER BY COALESCE(lm.id, 0) DESC, c.id DESC
        LIMIT $2
        "#,
    )
    .bind(me.id)
    .bind(limit)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let mut out = Vec::with_capacity(rows.len());

    for r in rows {
        let chat_id: i64 = r.get("chat_id");
        let member_count: i64 = r.get("member_count");
        let chat_name: String = r.get("chat_name");

        let members = sqlx::query(
            r#"
            SELECT u.id, u.username, up.avatar_file_id
            FROM chat_participants cp
            JOIN users u ON u.id = cp.user_id
            LEFT JOIN user_profile up ON up.user_id = u.id
            WHERE cp.chat_id = $1
            ORDER BY lower(u.username)
            "#,
        )
        .bind(chat_id)
        .fetch_all(db)
        .await
        .unwrap_or_default();

        let mut member_names = Vec::new();
        let mut others = Vec::<(i64, String, Option<i64>)>::new();
        for m in members {
            let uid: i64 = m.get("id");
            let username: String = m.get("username");
            let avatar_file_id: Option<i64> = m.try_get("avatar_file_id").ok().flatten();
            member_names.push(username.clone());
            if uid != me.id {
                others.push((uid, username, avatar_file_id));
            }
        }

        let is_group = member_count > 2;
        let title = if is_group {
            let name = chat_name.trim();
            if name.is_empty() {
                let joined = others
                    .iter()
                    .take(4)
                    .map(|(_, name, _)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                if joined.is_empty() {
                    "Групповой чат".to_string()
                } else {
                    joined
                }
            } else {
                name.to_string()
            }
        } else {
            others
                .first()
                .map(|(_, name, _)| name.clone())
                .unwrap_or_else(|| "Личный чат".to_string())
        };

        let (other_user_id, other_username, other_avatar_file_id) = if is_group {
            (0, title.clone(), None)
        } else {
            others
                .first()
                .map(|(id, name, avatar_file_id)| (*id, name.clone(), *avatar_file_id))
                .unwrap_or((0, title.clone(), None))
        };

        out.push(DmChatView {
            chat_id,
            other_user_id,
            other_username,
            other_avatar_file_id,
            title,
            is_group,
            member_count,
            member_names,
            last_message_id: r.try_get("last_message_id").ok(),
            last_message_at: r.try_get::<chrono::DateTime<chrono::Utc>, _>("last_message_at").ok().map(|d| d.to_rfc3339()),
            last_message_preview: r.try_get("last_message_preview").ok(),
        });
    }

    (StatusCode::OK, Json(out)).into_response()
}

async fn ensure_dm_participant(db: &sqlx::PgPool, chat_id: i64, _user_id: i64) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT 1::bigint FROM chat_participants WHERE chat_id = $1 AND user_id = $2 LIMIT 1",
    )
    .bind(chat_id)
    .bind(_user_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some()
}

async fn list_participants(
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
        SELECT u.id,
               u.username,
               u.public_encryption_key,
               up.avatar_file_id,
               COALESCE(p.is_online, FALSE) AS is_online,
               COALESCE(p.status, 'offline') AS status
        FROM chat_participants cp
        JOIN users u ON u.id = cp.user_id
        LEFT JOIN user_profile up ON up.user_id = u.id
        LEFT JOIN user_presence p ON p.user_id = u.id
        WHERE cp.chat_id = $1
        ORDER BY CASE WHEN u.id = $2 THEN 0 ELSE 1 END, lower(u.username)
        "#,
    )
    .bind(chat_id)
    .bind(me.id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let out = rows
        .into_iter()
        .map(|r| {
            let id: i64 = r.get("id");
            DmParticipantView {
                id,
                username: r.get("username"),
                avatar_file_id: r.try_get("avatar_file_id").ok(),
                public_encryption_key: r.try_get("public_encryption_key").ok(),
                is_me: id == me.id,
                is_online: r.get::<bool, _>("is_online"),
                status: r.get("status"),
            }
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(out)).into_response()
}

async fn list_messages(
    State(st): State<AppState>,
    me: AuthUser,
    Path(chat_id): Path<i64>,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    let db = &st.db;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);

    if !ensure_dm_participant(db, chat_id, me.id).await {
        return StatusCode::FORBIDDEN.into_response();
    }

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
            WHERE m.chat_id = $1
              AND m.id < $2
            ORDER BY m.id DESC
            LIMIT $3
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
            WHERE m.chat_id = $1
            ORDER BY m.id DESC
            LIMIT $2
            "#,
        )
        .bind(chat_id)
        .bind(limit)
        .fetch_all(db)
        .await
        .unwrap_or_default()
    };

    let mut out = Vec::with_capacity(rows.len());

    for r in rows {
        let sender_id: i64 = r.get("sender_id");
        let sender_avatar_file_id: Option<i64> = r.try_get("sender_avatar_file_id").ok();
        let mut content: String = r.get("content");

        let blocked = sqlx::query_scalar::<_, i64>(
            "SELECT 1::bigint FROM user_blocks WHERE blocker_id = $1 AND blocked_id = $2 LIMIT 1",
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
            timestamp: r.get::<chrono::DateTime<chrono::Utc>, _>("timestamp").to_rfc3339(),
            reply_to_id,
            reply_preview,
            reactions: None,
        });
    }

    out.reverse();

    // attach reactions summary
    if !out.is_empty() {
        let ids = out.iter().map(|m| m.id).collect::<Vec<_>>();

        let mut qb = QueryBuilder::<sqlx::Postgres>::new(
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

        let mut qb2 = QueryBuilder::<sqlx::Postgres>::new(
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

    let recipient_rows = sqlx::query(
        "SELECT user_id FROM chat_participants WHERE chat_id = $1 AND user_id != $2",
    )
    .bind(chat_id)
    .bind(me.id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    if recipient_rows.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    for row in recipient_rows {
        let other_id: i64 = row.get("user_id");
        if is_blocked_pair(db, me.id, other_id).await {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"detail":"Blocked"})),
            )
                .into_response();
        }
    }

    let content_trimmed = body.content.trim();
    if content_trimmed.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if content_trimmed.chars().count() > MAX_MESSAGE_CHARS {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({
                "detail": "message_too_long",
                "max_chars": MAX_MESSAGE_CHARS
            })),
        )
            .into_response();
    }

    if !encrypted_or_file_reference(content_trimmed) {
        return encrypted_message_required_response();
    }

    if let Some(reply_to_id) = body.reply_to_id {
        let reply_ok = sqlx::query_scalar::<_, i64>(
            "SELECT 1::bigint FROM messages WHERE id = $1 AND chat_id = $2 LIMIT 1",
        )
        .bind(reply_to_id)
        .bind(chat_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .is_some();

        if !reply_ok {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"detail":"reply_to_message_not_in_chat"})),
            )
                .into_response();
        }
    }

    let timestamp = auth::now_iso();

    let res = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO messages (chat_id, sender_id, content, timestamp, reply_to_message_id)
        VALUES ($1, $2, $3, $4, $5) RETURNING id
        "#,
    )
    .bind(chat_id)
    .bind(me.id)
    .bind(content_trimmed)
    .bind(timestamp)
    .bind(body.reply_to_id)
    .fetch_one(db)
    .await;

    let Ok(message_id) = res else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    // Привязка загруженных файлов к сообщению ЛС, чтобы cleanup не удалил их как temporary.
    if let Ok(re) = Regex::new(r"\[\[file[:=](\d+)\|") {
        let mut file_ids: Vec<i64> = re
            .captures_iter(content_trimmed)
            .filter_map(|c| c.get(1).and_then(|m| m.as_str().parse::<i64>().ok()))
            .collect();
        file_ids.sort_unstable();
        file_ids.dedup();

        for fid in file_ids {
            let _ = sqlx::query(
                r#"
                UPDATE files
                SET message_id = $1,
                    storage_kind = 'message',
                    expires_at = NULL
                WHERE id = $2
                  AND chat_id = $3
                  AND uploaded_by = $4
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
        "SELECT avatar_file_id FROM user_profile WHERE user_id = $1 LIMIT 1",
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
            WHERE rm.id = $1
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
