use axum::extract::ws::{Message, WebSocket};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;

use crate::ws::{ConnId, Hub, UserId};

static CONN_ID_SEQ: AtomicU64 = AtomicU64::new(1);

pub async fn handle(
    mut socket: WebSocket,
    db: SqlitePool,
    hub: Hub,
    username: String,
) {
    let user_id = match get_user_id(&db, &username).await {
        Some(id) => id,
        None => {
            let _ = socket.close().await;
            return;
        }
    };

    let conn_id: ConnId = CONN_ID_SEQ.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();

    // ===== REGISTER PRESENCE =====
    let was_offline = hub.presence.get(&user_id).is_none();
    hub.presence_join(user_id, conn_id, tx.clone());

    let _ = tx.send(json!({
        "event": "connected",
        "user_id": user_id
    }));

    if was_offline {
        let _ = set_online(&db, user_id, true).await;
        hub.broadcast_presence(&json!({
            "event": "user_online",
            "user_id": user_id
        }));
    }

    // ===== MAIN LOOP =====
    let _ = async {
        loop {
            tokio::select! {

                // inbound (client → server)
                msg = socket.recv() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            let _ = socket
                                .send(Message::Text(
                                    json!({ "event": "ack" }).to_string()
                                ))
                                .await;
                            tracing::debug!("ws text: {}", text);
                        }

                        Some(Ok(Message::Close(_))) => break,
                        None => break,
                        _ => {}
                    }
                }

                // outbound (server → client)
                Some(payload) = rx.recv() => {
                    if socket
                        .send(Message::Text(payload.to_string()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    }.await;

    // ===== CLEANUP =====
    hub.presence_leave(user_id, conn_id);

    let still_online = hub.presence.get(&user_id).is_some();
    if !still_online {
        let _ = set_online(&db, user_id, false).await;
        hub.broadcast_presence(&json!({
            "event": "user_offline",
            "user_id": user_id
        }));
    }
}

// =======================
// DB HELPERS
// =======================

async fn get_user_id(db: &SqlitePool, username: &str) -> Option<UserId> {
    sqlx::query_scalar::<_, i64>(
        "SELECT id FROM users WHERE username = ? AND is_banned = 0",
    )
    .bind(username)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

async fn set_online(
    db: &SqlitePool,
    user_id: UserId,
    online: bool,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO user_presence(user_id, is_online)
        VALUES(?, ?)
        ON CONFLICT(user_id)
        DO UPDATE SET is_online = excluded.is_online
        "#,
    )
    .bind(user_id)
    .bind(if online { 1 } else { 0 })
    .execute(db)
    .await?;
    Ok(())
}
