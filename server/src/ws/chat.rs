use axum::extract::ws::{Message, WebSocket};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Instant;

use crate::ws::{ConnId, Hub, RoomId, UserId};
use sqlx::SqlitePool;
use tokio::{
    sync::mpsc,
    time::{interval, Duration},
};

static CONN_ID_SEQ: AtomicU64 = AtomicU64::new(1);
const GLOBAL_CHAT_ID: i64 = 1;

pub async fn handle_single_ws(
    mut socket: WebSocket,
    db: SqlitePool,
    hub: Arc<Hub>,  // <-- ИЗМЕНЕНО: Теперь Arc<Hub>
    user_id: UserId,
    username: String,
) {
    println!(
        "[{}][TRACE] ENTER handle_single_ws",
        chrono::Utc::now().to_rfc3339()
    );
    let conn_id: ConnId = CONN_ID_SEQ.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();

    // === IMPROVED CONNECTION MANAGEMENT ===
    println!(
        "[WS] user={} new conn={} requesting connection",
        user_id, conn_id
    );

    // Простая проверка без блокировок
    if let Some(active_conn) = hub.get_active_conn(user_id) {
        println!("[WS] Warning: User {} already connected (conn={}), taking over", user_id, active_conn);
        // Можно отправить сообщение старому соединению о "вытеснении"
    }

    // Perform atomic connection swap
    let old_conn = hub.swap_connection(user_id, conn_id).await;
    if let Some(old_conn_id) = old_conn {
        println!(
            "[WS] user={} swapped: old={} -> new={}",
            user_id, old_conn_id, conn_id
        );
    }

    println!(
        "[{}][TRACE] registering presence",
        chrono::Utc::now().to_rfc3339()
    );
    hub.presence_join(user_id, conn_id, tx.clone());

    // === Подключаем доступные каналы и ДМ ===
    println!(
        "[{}][TRACE] querying accessible channels and dms",
        chrono::Utc::now().to_rfc3339()
    );
    let channels = get_accessible_channels(&db, user_id).await;
    let dms = get_accessible_dms(&db, user_id).await;

    for ch in channels {
        println!(
            "[{}][TRACE] joining channel {}",
            chrono::Utc::now().to_rfc3339(),
            ch
        );
        hub.room_join(RoomId::Channel(ch), user_id, conn_id, tx.clone());
    }

    for dm in dms {
        println!(
            "[{}][TRACE] joining dm {}",
            chrono::Utc::now().to_rfc3339(),
            dm
        );
        hub.room_join(RoomId::Dm(dm), user_id, conn_id, tx.clone());
    }

    // === HEARTBEAT CONFIG ===
    let mut heartbeat = interval(Duration::from_secs(5));
    let mut last_ping = Instant::now();
    println!("[WS] user={} conn={} connected", user_id, conn_id);

    // Send welcome message
    let _ = tx.send(json!({
        "type": "connected",
        "connection_id": conn_id,
        "user_id": user_id,
        "timestamp": chrono::Utc::now().timestamp_millis()
    }));

    println!(
        "[{}][TRACE] entering main loop",
        chrono::Utc::now().to_rfc3339()
    );

    // === MAIN LOOP ===
    'main_loop: loop {
        tokio::select! {
            Some(payload) = rx.recv() => {
                println!("[{}][TRACE] received from rx len={}", chrono::Utc::now().to_rfc3339(), payload.to_string().len());
                
                // Check if connection is marked as closing
                if hub.is_conn_active(conn_id) {
                    if socket.send(Message::Text(payload.to_string())).await.is_err() {
                        println!("[WS] user={} conn={} closed (send fail)", user_id, conn_id);
                        break 'main_loop;
                    }
                } else {
                    println!("[WS] user={} conn={} is closing, breaking loop", user_id, conn_id);
                    break 'main_loop;
                }
            }

            _ = heartbeat.tick() => {
                println!("[{}][TRACE] heartbeat tick user={}", chrono::Utc::now().to_rfc3339(), user_id);
                
                if !hub.is_conn_active(conn_id) {
                    println!("[WS] user={} conn={} is closing, breaking heartbeat", user_id, conn_id);
                    break 'main_loop;
                }
                
                if socket.send(Message::Ping(vec![])).await.is_err() {
                    println!("[HEARTBEAT] user={} conn={} lost heartbeat (send fail)", user_id, conn_id);
                    break 'main_loop;
                }
                
                if last_ping.elapsed() > Duration::from_secs(60) {
                    println!("[HEARTBEAT] user={} conn={} timeout (no client ping)", user_id, conn_id);
                    break 'main_loop;
                }
            }

            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        println!("[{}][TRACE] incoming text len={} user={}", chrono::Utc::now().to_rfc3339(), text.len(), user_id);
                        
                        if !hub.is_conn_active(conn_id) {
                            println!("[WS] user={} conn={} received message while closing", user_id, conn_id);
                            break 'main_loop;
                        }
                        
                        if let Ok(v) = serde_json::from_str::<Value>(&text) {
                            if v.get("type").and_then(|t| t.as_str()) == Some("ping") {
                                last_ping = Instant::now();
                                println!("[{}][TRACE] received ping user={}", chrono::Utc::now().to_rfc3339(), user_id);
                                let pong = json!({
                                    "type": "pong",
                                    "t": chrono::Utc::now().timestamp_millis()
                                });
                                if socket.send(Message::Text(pong.to_string())).await.is_err() {
                                    println!("[WS] pong send failed user={} conn={}", user_id, conn_id);
                                    break 'main_loop;
                                }
                                continue;
                            }
                        }
                        handle_incoming_message(&text, &db, &hub, user_id, conn_id, &username, &tx).await;
                    }
                    Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {
                        last_ping = Instant::now();
                        println!("[{}][TRACE] ping/pong updated user={}", chrono::Utc::now().to_rfc3339(), user_id);
                        continue;
                    }
                    Some(Ok(Message::Binary(_))) => continue,
                    Some(Ok(Message::Close(frame))) => {
                        println!("[WS] user={} conn={} closed gracefully: {:?}", user_id, conn_id, frame);
                        println!("[{}][TRACE] closing gracefully", chrono::Utc::now().to_rfc3339());
                        break 'main_loop;
                    }
                    None => {
                        println!("[WS] user={} conn={} client gone", user_id, conn_id);
                        break 'main_loop;
                    }
                    Some(Err(e)) => {
                        println!("[WS] user={} conn={} recv error: {:?}", user_id, conn_id, e);
                        println!("[{}][TRACE] socket recv error", chrono::Utc::now().to_rfc3339());
                        break 'main_loop;
                    }
                }
            }
        }
        
        if rx.is_closed() {
            println!("[WS] rx closed, exiting user={} conn={}", user_id, conn_id);
            println!("[{}][TRACE] rx channel closed", chrono::Utc::now().to_rfc3339());
            break 'main_loop;
        }
    }

    // === Чистое завершение ===
    println!(
        "[WS] user={} conn={} performing cleanup",
        user_id, conn_id
    );
    drop(rx);
    println!("[{}][TRACE] dropped rx", chrono::Utc::now().to_rfc3339());

    // Send close frame if possible
    if let Ok(_) = socket.send(Message::Close(None)).await {
        println!("[WS] close frame sent user={} conn={}", user_id, conn_id);
    }

    // Small delay to ensure graceful shutdown
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Cleanup connection from hub
    hub.cleanup_conn(user_id, conn_id).await;
    println!("[WS] cleanup complete user={} conn={}", user_id, conn_id);
    println!(
        "[{}][TRACE] EXIT handle_single_ws",
        chrono::Utc::now().to_rfc3339()
    );
}

// ======================================================
// SEPARATED HANDLER
// ======================================================
async fn handle_incoming_message(
    text: &str,
    db: &SqlitePool,
    hub: &Hub,
    user_id: UserId,
    conn_id: ConnId,
    username: &str,
    tx: &mpsc::UnboundedSender<Value>,
) {
    println!(
        "[{}][TRACE] ENTER handle_incoming_message",
        chrono::Utc::now().to_rfc3339()
    );
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return;
    };
    let typ = v.get("type").and_then(|x| x.as_str()).unwrap_or("");

    match typ {
        "join_chat" => {
            println!("[{}][TRACE] join_chat received", chrono::Utc::now().to_rfc3339());
            if let Some(room) = parse_room_join_chat(&v) {
                println!("[WS] user={} using protocol=v2 (join_chat)", user_id);
                if matches!(room, RoomId::Channel(GLOBAL_CHAT_ID)) {
                    hub.room_join(room.clone(), user_id, conn_id, tx.clone());
                    let _ = tx.send(json!({ "type": "joined", "room": room_to_json(&room) }));
                    return;
                }
                if !check_membership(db, &room, user_id).await {
                    let _ = tx.send(json!({
                        "type": "error", "code": "not_member", "room": room_to_json(&room)
                    }));
                    return;
                }
                hub.room_join(room, user_id, conn_id, tx.clone());
            }
        }
        "send_message" => {
            println!(
                "[{}][TRACE] send_message received",
                chrono::Utc::now().to_rfc3339()
            );
            if let Some(room) = parse_room_send_message(&v) {
                println!("[WS] user={} using protocol=v2 (send_message)", user_id);
                if matches!(room, RoomId::Channel(GLOBAL_CHAT_ID))
                    || check_membership(db, &room, user_id).await
                {
                    handle_message_frontend(db, hub, &room, user_id, username, &v).await;
                } else {
                    let _ = tx.send(json!({
                        "type": "error", "code": "not_member", "room": room_to_json(&room)
                    }));
                }
            }
        }
        "join" => {
            println!("[{}][TRACE] legacy join", chrono::Utc::now().to_rfc3339());
            if let Some(room) = parse_room_old(&v) {
                println!("[WS] user={} using legacy protocol=v1 (join)", user_id);
                if check_membership(db, &room, user_id).await {
                    hub.room_join(room, user_id, conn_id, tx.clone());
                } else {
                    let _ = tx.send(json!({
                        "type": "error", "code": "not_member", "room": room_to_json(&room)
                    }));
                }
            }
        }
        "message" => {
            println!("[{}][TRACE] legacy message", chrono::Utc::now().to_rfc3339());
            if let Some(room) = parse_room_old(&v) {
                println!("[WS] user={} using legacy protocol=v1 (message)", user_id);
                if check_membership(db, &room, user_id).await {
                    handle_message_old(db, hub, &room, user_id, username, &v).await;
                } else {
                    let _ = tx.send(json!({
                        "type": "error", "code": "not_member", "room": room_to_json(&room)
                    }));
                }
            }
        }
        _ => {
            println!("[WS] user={} unknown type={:?}", user_id, typ);
            let _ = tx.send(json!({
                "type": "error", "code": "unknown_type", "got": typ
            }));
        }
    }
    println!(
        "[{}][TRACE] EXIT handle_incoming_message",
        chrono::Utc::now().to_rfc3339()
    );
}

// ======================================================
// HANDLERS
// ======================================================
async fn handle_message_frontend(
    db: &SqlitePool,
    hub: &Hub,
    room: &RoomId,
    user_id: UserId,
    username: &str,
    payload: &Value,
) {
    println!(
        "[{}][TRACE] handle_message_frontend",
        chrono::Utc::now().to_rfc3339()
    );
    let Some(text) = payload
        .get("data")
        .and_then(|d| d.get("content"))
        .and_then(|v| v.as_str())
    else {
        return;
    };
    persist_message(db, room, user_id, text).await;

    let out = json!({
        "type": "chat_message",
        "data": {
            "chat_id": room_id_num(room),
            "sender_id": user_id,
            "sender_username": username,
            "content": text
        }
    });
    hub.broadcast_room(room, &out);
}

async fn handle_message_old(
    db: &SqlitePool,
    hub: &Hub,
    room: &RoomId,
    user_id: UserId,
    username: &str,
    payload: &Value,
) {
    println!(
        "[{}][TRACE] handle_message_old",
        chrono::Utc::now().to_rfc3339()
    );
    let Some(text) = payload.get("text").and_then(|v| v.as_str()) else {
        return;
    };
    persist_message(db, room, user_id, text).await;
    let out = json!({
        "type": "message",
        "room": room_to_json(room),
        "text": text,
        "user": username,
        "timestamp": chrono::Utc::now().to_rfc3339()
    });
    hub.broadcast_room(room, &out);
}

async fn persist_message(db: &SqlitePool, room: &RoomId, user_id: UserId, text: &str) {
    println!(
        "[{}][TRACE] persist_message",
        chrono::Utc::now().to_rfc3339()
    );
    let ts = chrono::Utc::now().to_rfc3339();
    match room {
        RoomId::Channel(id) => {
            let _ = sqlx::query(
                "INSERT INTO messages(kind, channel_id, sender_id, content, timestamp)
                 VALUES('channel', ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(user_id)
            .bind(text)
            .bind(&ts)
            .execute(db)
            .await;
        }
        RoomId::Dm(id) => {
            let _ = sqlx::query(
                "INSERT INTO messages(kind, dm_id, sender_id, content, timestamp)
                 VALUES('dm', ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(user_id)
            .bind(text)
            .bind(&ts)
            .execute(db)
            .await;
        }
    }
}

// ======================================================
// PARSERS
// ======================================================
fn parse_room_old(v: &Value) -> Option<RoomId> {
    println!("[{}][TRACE] parse_room_old", chrono::Utc::now().to_rfc3339());
    let kind = v.get("room")?.get("kind")?.as_str()?;
    let id = v.get("room")?.get("id")?.as_i64()?;
    match kind {
        "channel" => Some(RoomId::Channel(id)),
        "dm" => Some(RoomId::Dm(id)),
        _ => None,
    }
}

fn parse_room_join_chat(v: &Value) -> Option<RoomId> {
    println!(
        "[{}][TRACE] parse_room_join_chat",
        chrono::Utc::now().to_rfc3339()
    );
    let chat_id = v.get("data")?.get("chat_id")?.as_i64()?;
    Some(RoomId::Channel(chat_id))
}

fn parse_room_send_message(v: &Value) -> Option<RoomId> {
    println!(
        "[{}][TRACE] parse_room_send_message",
        chrono::Utc::now().to_rfc3339()
    );
    let chat_id = v.get("data")?.get("chat_id")?.as_i64()?;
    Some(RoomId::Channel(chat_id))
}

// ======================================================
// MEMBERSHIP + QUERIES
// ======================================================
async fn check_membership(db: &SqlitePool, room: &RoomId, user_id: UserId) -> bool {
    println!(
        "[{}][TRACE] check_membership",
        chrono::Utc::now().to_rfc3339()
    );
    match room {
        RoomId::Channel(id) => {
            sqlx::query_scalar::<_, i64>(
                "SELECT 1 FROM server_members sm JOIN channels c ON c.server_id = sm.server_id WHERE c.id = ? AND sm.user_id = ?",
            )
            .bind(id)
            .bind(user_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten()
            .is_some()
        }
        RoomId::Dm(id) => {
            sqlx::query_scalar::<_, i64>(
                "SELECT 1 FROM dm_dialogs WHERE id = ? AND (user_a = ? OR user_b = ?)",
            )
            .bind(id)
            .bind(user_id)
            .bind(user_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten()
            .is_some()
        }
    }
}

async fn get_accessible_channels(db: &SqlitePool, user_id: UserId) -> Vec<i64> {
    println!(
        "[{}][TRACE] get_accessible_channels",
        chrono::Utc::now().to_rfc3339()
    );
    sqlx::query_scalar::<_, i64>(
        "SELECT c.id FROM channels c JOIN server_members sm ON sm.server_id = c.server_id WHERE sm.user_id = ?",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
    .unwrap_or_default()
}

async fn get_accessible_dms(db: &SqlitePool, user_id: UserId) -> Vec<i64> {
    println!(
        "[{}][TRACE] get_accessible_dms",
        chrono::Utc::now().to_rfc3339()
    );
    sqlx::query_scalar::<_, i64>(
        "SELECT id FROM dm_dialogs WHERE user_a = ? OR user_b = ?",
    )
    .bind(user_id)
    .bind(user_id)
    .fetch_all(db)
    .await
    .unwrap_or_default()
}

// ======================================================
// JSON HELPERS
// ======================================================
fn room_to_json(room: &RoomId) -> Value {
    println!("[{}][TRACE] room_to_json", chrono::Utc::now().to_rfc3339());
    match room {
        RoomId::Channel(id) => json!({ "kind": "channel", "id": id }),
        RoomId::Dm(id) => json!({ "kind": "dm", "id": id }),
    }
}

fn room_id_num(room: &RoomId) -> i64 {
    match room {
        RoomId::Channel(id) => *id,
        RoomId::Dm(id) => *id,
    }
}