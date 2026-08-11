use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};

use crate::auth;
use crate::ws::{
    ConnId, Hub, RoomId, UserId, WS_CHANNEL_BUFFER,
};

static CONN_ID_SEQ: AtomicU64 = AtomicU64::new(1);

pub async fn handle_single_ws(
    socket: WebSocket,
    db: PgPool,
    hub: Arc<Hub>,
    user_id: UserId,
    username: String,
) {
    eprintln!("[WS_RAW] handle_single_ws START user={}", user_id);
    tracing::error!("[WS_RAW] handle_single_ws START user={}", user_id);
    tracing::info!("[WS] handle_single_ws started for user {}", user_id);
    let conn_id: ConnId = CONN_ID_SEQ.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = mpsc::channel::<Value>(WS_CHANNEL_BUFFER);
    let (mut ws_sender, mut ws_receiver) = socket.split();

    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering as AtomicOrdering;
    let last_seen_ts = Arc::new(AtomicU64::new(chrono::Utc::now().timestamp_millis() as u64));

    hub.presence_join(user_id, conn_id, tx.clone());
    let became_online = hub.user_conn_ids(user_id).len() == 1;

    // db presence upsert
    let now = auth::now_iso();
    let _ = sqlx::query(
        r#"
        INSERT INTO user_presence(user_id, is_online, status, updated_at)
        VALUES($1, TRUE, 'online', $2)
        ON CONFLICT(user_id) DO UPDATE SET
          is_online = TRUE,
          updated_at = excluded.updated_at
        "#,
    )
    .bind(user_id)
    .bind(now)
    .execute(&db)
    .await;

    if became_online {
        hub.broadcast_presence(&json!({
            "type": "user_online",
            "user_id": user_id,
            "timestamp": chrono::Utc::now().timestamp_millis()
        }));
    }

    let rooms = get_accessible_rooms(&db, user_id).await;
    tracing::info!("[WS] user {} subscribing to {} rooms: {:?}", user_id, rooms.len(), rooms);
    for room in rooms {
        hub.room_join(room, user_id, conn_id, tx.clone());
    }

    let _ = tx.try_send(json!({
        "type": "connected",
        "connection_id": conn_id,
        "user_id": user_id,
        "timestamp": chrono::Utc::now().timestamp_millis()
    }));

    let hub_for_writer = hub.clone();
    let last_seen_writer = Arc::clone(&last_seen_ts);
    let writer_conn = conn_id;
    let writer = tokio::spawn(async move {
        let mut heartbeat = interval(Duration::from_secs(10));
        loop {
            tokio::select! {
                Some(payload) = rx.recv() => {
                    let should_close = payload
                        .get("type")
                        .and_then(|t| t.as_str())
                        .map(|t| t == "force_logout")
                        .unwrap_or(false);

                    if ws_sender.send(Message::Text(payload.to_string().into())).await.is_err() {
                        break;
                    }

                    if should_close {
                        break;
                    }
                }
                _ = heartbeat.tick() => {
                    if !hub_for_writer.is_conn_active(writer_conn) {
                        break;
                    }
                    if ws_sender.send(Message::Ping(Vec::new().into())).await.is_err() {
                        break;
                    }

                    let last = last_seen_writer.load(AtomicOrdering::Relaxed) as i64;
                    let now = chrono::Utc::now().timestamp_millis();
                    if now - last > 90_000 {
                        break;
                    }
                }
            }
        }
    });

    'main: loop {
        match ws_receiver.next().await {
            Some(Ok(Message::Text(text))) => {
                // update last_seen
                last_seen_ts.store(chrono::Utc::now().timestamp_millis() as u64, AtomicOrdering::Relaxed);

                if !hub.is_conn_active(conn_id) {
                    break 'main;
                }

                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    if v.get("type").and_then(|t| t.as_str()) == Some("ping") {
                        let t = v.get("t")
                            .and_then(|x| x.as_i64())
                            .or_else(|| v.get("timestamp").and_then(|x| x.as_i64()))
                            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());

                        let _ = tx.try_send(json!({"type":"pong","t": t}));
                        continue;
                    }
                }

                handle_incoming_message(&text, &db, &hub, user_id, conn_id, &username, &tx).await;
            }

            Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {
                last_seen_ts.store(chrono::Utc::now().timestamp_millis() as u64, AtomicOrdering::Relaxed);
            }

            Some(Ok(Message::Close(_))) | None => break 'main,

            Some(Ok(Message::Binary(_))) => {}

            Some(Err(_)) => break 'main,
        }
    }

    writer.abort();

    hub.cleanup_conn(user_id, conn_id).await;

    let still_online = hub
        .presence
        .get(&user_id)
        .map(|conns| !conns.is_empty())
        .unwrap_or(false);

    if !still_online {
        let now = auth::now_iso();
        let _ = sqlx::query(
            "UPDATE user_presence SET is_online = FALSE, updated_at = $1 WHERE user_id = $2",
        )
        .bind(now)
        .bind(user_id)
        .execute(&db)
        .await;

        hub.broadcast_presence(&json!({
            "type": "user_offline",
            "user_id": user_id,
            "timestamp": chrono::Utc::now().timestamp_millis()
        }));
    }
}

pub async fn handle_incoming_message(
    text: &str,
    db: &PgPool,
    hub: &Hub,
    user_id: UserId,
    conn_id: ConnId,
    username: &str,
    tx: &mpsc::Sender<Value>,
) {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return; };
    let typ = v.get("type").and_then(|x| x.as_str()).unwrap_or("");

    match typ {
        "typing" | "upload_state" => {
            let Some(chat_id) = v.get("data").and_then(|d| d.get("chat_id")).and_then(|x| x.as_i64()) else {
                return;
            };

            if !can_access_chat(db, user_id, chat_id).await {
                return;
            }

            let state = v.get("data").and_then(|d| d.get("state")).and_then(|x| x.as_str()).unwrap_or("start");
            let activity = v.get("data").and_then(|d| d.get("activity")).and_then(|x| x.as_str()).unwrap_or("text");
            let payload = json!({
                "type": typ,
                "chat_id": chat_id,
                "user_id": user_id,
                "username": username,
                "state": state,
                "activity": activity,
                "timestamp": chrono::Utc::now().timestamp_millis()
            });

            let is_voice = chat_is_voice(db, chat_id).await;
            if is_voice {
                if hub.voice_get_conn_channel(conn_id) != Some(chat_id) {
                    return;
                }
                hub.broadcast_room_except_user(&RoomId::Voice(chat_id), user_id, &payload);
            } else {
                hub.broadcast_room_except_user(&RoomId::Channel(chat_id), user_id, &payload);
            }
        }


        // =====================
        // DM CALLS (ring + accept/decline)
        // =====================
        "dm_call_invite" => {
            let Some(chat_id) = v.get("data").and_then(|d| d.get("chat_id")).and_then(|x| x.as_i64()) else {
                let _ = tx.try_send(json!({"type":"error","code":"bad_request"}));
                return;
            };

            if !can_access_chat(db, user_id, chat_id).await {
                let _ = tx.try_send(json!({"type":"error","code":"not_member","chat_id": chat_id}));
                return;
            }

            let Some(peer_id) = dm_peer_user_id(db, chat_id, user_id).await else {
                let _ = tx.try_send(json!({"type":"error","code":"not_dm","chat_id": chat_id}));
                return;
            };

            let timestamp = chrono::Utc::now().timestamp_millis();
            let invite = json!({
                "type": "dm_call_invite",
                "chat_id": chat_id,
                "from_user_id": user_id,
                "from_username": username,
                "timestamp": timestamp
            });

            let delivered = hub.send_to_user(peer_id, &invite);
            if !delivered {
                hub.queue_for_user(peer_id, json!({
                    "type": "dm_call_missed",
                    "chat_id": chat_id,
                    "from_user_id": user_id,
                    "from_username": username,
                    "timestamp": timestamp
                }));
            }

            let _ = tx.try_send(json!({"type":"dm_call_invite_sent","chat_id": chat_id,"delivered":delivered}));
        }

        "dm_call_accept" => {
            let Some(chat_id) = v.get("data").and_then(|d| d.get("chat_id")).and_then(|x| x.as_i64()) else {
                let _ = tx.try_send(json!({"type":"error","code":"bad_request"}));
                return;
            };

            if !can_access_chat(db, user_id, chat_id).await {
                let _ = tx.try_send(json!({"type":"error","code":"not_member","chat_id": chat_id}));
                return;
            }

            let Some(peer_id) = dm_peer_user_id(db, chat_id, user_id).await else {
                let _ = tx.try_send(json!({"type":"error","code":"not_dm","chat_id": chat_id}));
                return;
            };

            hub.send_to_user(peer_id, &json!({
                "type": "dm_call_accept",
                "chat_id": chat_id,
                "from_user_id": user_id,
                "from_username": username,
                "timestamp": chrono::Utc::now().timestamp_millis()
            }));

            let _ = tx.try_send(json!({"type":"dm_call_accept_sent","chat_id": chat_id}));
        }

        "dm_call_decline" => {
            let Some(chat_id) = v.get("data").and_then(|d| d.get("chat_id")).and_then(|x| x.as_i64()) else {
                let _ = tx.try_send(json!({"type":"error","code":"bad_request"}));
                return;
            };

            let reason = v.get("data").and_then(|d| d.get("reason")).and_then(|x| x.as_str()).unwrap_or("declined");

            if !can_access_chat(db, user_id, chat_id).await {
                let _ = tx.try_send(json!({"type":"error","code":"not_member","chat_id": chat_id}));
                return;
            }

            let Some(peer_id) = dm_peer_user_id(db, chat_id, user_id).await else {
                let _ = tx.try_send(json!({"type":"error","code":"not_dm","chat_id": chat_id}));
                return;
            };

            hub.send_to_user(peer_id, &json!({
                "type": "dm_call_decline",
                "chat_id": chat_id,
                "from_user_id": user_id,
                "from_username": username,
                "reason": reason,
                "timestamp": chrono::Utc::now().timestamp_millis()
            }));

            let _ = tx.try_send(json!({"type":"dm_call_decline_sent","chat_id": chat_id}));
        }

        "dm_call_cancel" => {
            let Some(chat_id) = v.get("data").and_then(|d| d.get("chat_id")).and_then(|x| x.as_i64()) else {
                let _ = tx.try_send(json!({"type":"error","code":"bad_request"}));
                return;
            };

            let reason = v.get("data").and_then(|d| d.get("reason")).and_then(|x| x.as_str()).unwrap_or("cancel");

            if !can_access_chat(db, user_id, chat_id).await {
                let _ = tx.try_send(json!({"type":"error","code":"not_member","chat_id": chat_id}));
                return;
            }

            let Some(peer_id) = dm_peer_user_id(db, chat_id, user_id).await else {
                let _ = tx.try_send(json!({"type":"error","code":"not_dm","chat_id": chat_id}));
                return;
            };

            hub.send_to_user(peer_id, &json!({
                "type": "dm_call_cancel",
                "chat_id": chat_id,
                "from_user_id": user_id,
                "from_username": username,
                "reason": reason,
                "timestamp": chrono::Utc::now().timestamp_millis()
            }));

            let _ = tx.try_send(json!({"type":"dm_call_cancel_sent","chat_id": chat_id}));
        }

        "dm_call_end" => {
            let Some(chat_id) = v.get("data").and_then(|d| d.get("chat_id")).and_then(|x| x.as_i64()) else {
                let _ = tx.try_send(json!({"type":"error","code":"bad_request"}));
                return;
            };

            if !can_access_chat(db, user_id, chat_id).await {
                let _ = tx.try_send(json!({"type":"error","code":"not_member","chat_id": chat_id}));
                return;
            }

            let Some(peer_id) = dm_peer_user_id(db, chat_id, user_id).await else {
                let _ = tx.try_send(json!({"type":"error","code":"not_dm","chat_id": chat_id}));
                return;
            };

            hub.send_to_user(peer_id, &json!({
                "type": "dm_call_end",
                "chat_id": chat_id,
                "from_user_id": user_id,
                "from_username": username,
                "timestamp": chrono::Utc::now().timestamp_millis()
            }));

            let _ = tx.try_send(json!({"type":"dm_call_end_sent","chat_id": chat_id}));
        }

        // =====================
        // VOICE (WebRTC signaling)
        // =====================
        "voice_join" => {
            let Some(channel_id) = v.get("data").and_then(|d| d.get("channel_id")).and_then(|x| x.as_i64()) else {
                let _ = tx.try_send(json!({"type":"error","code":"bad_request"}));
                return;
            };

            if !can_access_chat(db, user_id, channel_id).await {
                let _ = tx.try_send(json!({"type":"error","code":"not_member","channel_id": channel_id}));
                return;
            }

            if !is_voice_allowed(db, channel_id).await {
                let _ = tx.try_send(json!({"type":"error","code":"not_voice_channel","channel_id": channel_id}));
                return;
            }

            if hub.voice_get_conn_channel(conn_id) == Some(channel_id) {
                let peers = voice_peers(hub, channel_id, Some(user_id));
                let _ = tx.try_send(json!({
                    "type": "voice_joined",
                    "channel_id": channel_id,
                    "peers": peers,
                    "screen_shares": hub.ss_list(channel_id),
                    "timestamp": chrono::Utc::now().timestamp_millis()
                }));
                return;
            }

            // A browser tab owns one voice connection. If the same account joins
            // from another tab, close the older voice session so RTC signaling
            // does not fan out to multiple PeerConnections with the same user id.
            for (other_conn_id, prev) in hub.voice_user_conns(user_id) {
                if other_conn_id != conn_id {
                    voice_leave_conn_internal(hub, user_id, other_conn_id, None, prev, true);
                }
            }

            // If this connection is already in some voice channel -> leave it first.
            if let Some(prev) = hub.voice_get_conn_channel(conn_id) {
                if prev != channel_id {
                    voice_leave_internal(hub, user_id, conn_id, tx, prev, true);
                }
            }

            // Join voice room
            hub.room_join(RoomId::Voice(channel_id), user_id, conn_id, tx.clone());
            hub.voice_set(user_id, conn_id, channel_id);

            // Peers list (excluding self)
            let peers = voice_peers(hub, channel_id, Some(user_id));

            // Ack to self
            let _ = tx.try_send(json!({
                "type": "voice_joined",
                "channel_id": channel_id,
                "peers": peers,
                "screen_shares": hub.ss_list(channel_id),
                    "timestamp": chrono::Utc::now().timestamp_millis()
            }));

            // Notify others
            let payload = json!({
                "type": "voice_peer_joined",
                "channel_id": channel_id,
                "user_id": user_id,
                "timestamp": chrono::Utc::now().timestamp_millis()
            });
            broadcast_room_excluding_conn(hub, &RoomId::Voice(channel_id), conn_id, &payload);
        }

        "voice_leave" => {
            let channel_id_opt = v.get("data")
                .and_then(|d| d.get("channel_id"))
                .and_then(|x| x.as_i64());

            let current = hub.voice_get_conn_channel(conn_id);
            let Some(current_ch) = current else {
                let _ = tx.try_send(json!({"type":"voice_left","channel_id": channel_id_opt}));
                return;
            };

            if let Some(req_ch) = channel_id_opt {
                if req_ch != current_ch {
                    let _ = tx.try_send(json!({"type":"error","code":"not_in_that_voice","channel_id": req_ch,"current_channel_id": current_ch}));
                    return;
                }
            }

            voice_leave_internal(hub, user_id, conn_id, tx, current_ch, false);
        }

        "rtc_offer" | "rtc_answer" | "rtc_candidate" | "rtc_negotiate" => {
            let Some(data) = v.get("data") else { return; };
            let Some(channel_id) = data.get("channel_id").and_then(|x| x.as_i64()) else { return; };
            let Some(to_user_id) = data.get("to_user_id").and_then(|x| x.as_i64()) else { return; };

            // Only allow signaling inside the same voice channel
            if hub.voice_get_conn_channel(conn_id) != Some(channel_id) {
                let _ = tx.try_send(json!({"type":"error","code":"not_in_voice","channel_id": channel_id}));
                return;
            }
            if hub.voice_get_user_channel(to_user_id) != Some(channel_id) {
                let _ = tx.try_send(json!({"type":"error","code":"peer_not_in_voice","channel_id": channel_id,"to_user_id": to_user_id}));
                return;
            }

            let mut out = json!({
                "type": typ,
                "channel_id": channel_id,
                "from_user_id": user_id,
                "to_user_id": to_user_id,
                "timestamp": chrono::Utc::now().timestamp_millis()
            });

            match typ {
                "rtc_offer" | "rtc_answer" => {
                    if let Some(sdp) = data.get("sdp") {
                        out["sdp"] = sdp.clone();
                    } else {
                        let _ = tx.try_send(json!({"type":"error","code":"bad_request"}));
                        return;
                    }
                }
                "rtc_candidate" => {
                    if let Some(cand) = data.get("candidate") {
                        out["candidate"] = cand.clone();
                    } else {
                        let _ = tx.try_send(json!({"type":"error","code":"bad_request"}));
                        return;
                    }
                }
                "rtc_negotiate" => {
                    // no payload, just a renegotiation request
                }
                _ => {}
            }

            // Send only to the peer connection that is actually in this voice room.
            if !send_to_voice_user(hub, channel_id, to_user_id, &out) {
                let _ = tx.try_send(json!({"type":"error","code":"peer_not_in_voice","channel_id": channel_id,"to_user_id": to_user_id}));
            }
        }

        // =====================
        // VOICE SCREEN SHARE (signaling + presence)
        // =====================
        "voice_ss_start" => {
            let Some(channel_id) = v.get("data").and_then(|d| d.get("channel_id")).and_then(|x| x.as_i64()) else {
                let _ = tx.try_send(json!({"type":"error","code":"bad_request"}));
                return;
            };

            if hub.voice_get_conn_channel(conn_id) != Some(channel_id) {
                let _ = tx.try_send(json!({"type":"error","code":"not_in_voice","channel_id": channel_id}));
                return;
            }

            // set state
            hub.ss_set(channel_id, user_id, true);

            // ack to self
            let _ = tx.try_send(json!({
                "type": "voice_ss_started",
                "channel_id": channel_id,
                "user_id": user_id,
                "timestamp": chrono::Utc::now().timestamp_millis()
            }));

            // notify others
            let payload = json!({
                "type": "voice_ss_started",
                "channel_id": channel_id,
                "user_id": user_id,
                "timestamp": chrono::Utc::now().timestamp_millis()
            });
            broadcast_room_excluding_conn(hub, &RoomId::Voice(channel_id), conn_id, &payload);
        }

        "voice_ss_stop" => {
            let Some(channel_id) = v.get("data").and_then(|d| d.get("channel_id")).and_then(|x| x.as_i64()) else {
                let _ = tx.try_send(json!({"type":"error","code":"bad_request"}));
                return;
            };

            if hub.voice_get_conn_channel(conn_id) != Some(channel_id) {
                let _ = tx.try_send(json!({"type":"error","code":"not_in_voice","channel_id": channel_id}));
                return;
            }

            // clear state
            hub.ss_set(channel_id, user_id, false);

            // ack to self
            let _ = tx.try_send(json!({
                "type": "voice_ss_stopped",
                "channel_id": channel_id,
                "user_id": user_id,
                "timestamp": chrono::Utc::now().timestamp_millis()
            }));

            // notify others
            let payload = json!({
                "type": "voice_ss_stopped",
                "channel_id": channel_id,
                "user_id": user_id,
                "timestamp": chrono::Utc::now().timestamp_millis()
            });
            broadcast_room_excluding_conn(hub, &RoomId::Voice(channel_id), conn_id, &payload);
        }

        "voice_ss_watch" | "voice_ss_unwatch" => {
            let Some(data) = v.get("data") else { return; };
            let Some(channel_id) = data.get("channel_id").and_then(|x| x.as_i64()) else { return; };
            let Some(to_user_id) = data.get("to_user_id").and_then(|x| x.as_i64()) else { return; };

            // Only allow inside same voice channel
            if hub.voice_get_conn_channel(conn_id) != Some(channel_id) {
                let _ = tx.try_send(json!({"type":"error","code":"not_in_voice","channel_id": channel_id}));
                return;
            }
            if hub.voice_get_user_channel(to_user_id) != Some(channel_id) {
                let _ = tx.try_send(json!({"type":"error","code":"peer_not_in_voice","channel_id": channel_id,"to_user_id": to_user_id}));
                return;
            }

            let out = json!({
                "type": typ,
                "channel_id": channel_id,
                "from_user_id": user_id,
                "to_user_id": to_user_id,
                "timestamp": chrono::Utc::now().timestamp_millis()
            });

            if !send_to_voice_user(hub, channel_id, to_user_id, &out) {
                let _ = tx.try_send(json!({"type":"error","code":"peer_not_in_voice","channel_id": channel_id,"to_user_id": to_user_id}));
            }
        }

        // v2
        "join_chat" => {
            let Some(chat_id) = v.get("data").and_then(|d| d.get("chat_id")).and_then(|x| x.as_i64()) else {
                let _ = tx.try_send(json!({"type":"error","code":"bad_request"}));
                return;
            };

            if !can_access_chat(db, user_id, chat_id).await {
                let _ = tx.try_send(json!({"type":"error","code":"not_member","chat_id": chat_id}));
                return;
            }            let is_voice = chat_is_voice(db, chat_id).await;

            let room = if is_voice {
                if hub.voice_get_conn_channel(conn_id) != Some(chat_id) {
                    let _ = tx.try_send(json!({"type":"error","code":"not_in_voice","chat_id": chat_id}));
                    return;
                }
                RoomId::Voice(chat_id)
            } else {
                RoomId::Channel(chat_id)
            };

            hub.room_join(room.clone(), user_id, conn_id, tx.clone());
            let _ = tx.try_send(json!({"type":"joined","room": room_to_json(if is_voice {"voice"} else {"channel"}, chat_id)}));
        }

        "send_message" => {
            let Some(chat_id) = v.get("data").and_then(|d| d.get("chat_id")).and_then(|x| x.as_i64()) else { 
                tracing::warn!("[WS] send_message: bad_request - no chat_id");
                return; 
            };
            let Some(content) = v.get("data").and_then(|d| d.get("content")).and_then(|x| x.as_str()) else { 
                tracing::warn!("[WS] send_message: bad_request - no content");
                return; 
            };
            let content = content.trim();
            if content.is_empty() { 
                tracing::warn!("[WS] send_message: empty content");
                return; 
            }

            if !can_access_chat(db, user_id, chat_id).await {
                tracing::warn!("[WS] send_message: not_member - user {} chat {}", user_id, chat_id);
                let _ = tx.try_send(json!({"type": "error", "code": "not_member", "chat_id": chat_id}));
                return;
            }

            let ts = auth::now_iso();
            let message_id = persist_message(db, chat_id, user_id, content, &ts).await;
            
            tracing::info!("[WS] send_message: persisted id={} chat={} user={}", message_id, chat_id, user_id);

            let sender_avatar_file_id: Option<i64> = sqlx::query_scalar::<_, Option<i64>>(
                "SELECT avatar_file_id FROM user_profile WHERE user_id = $1 LIMIT 1",
            )
            .bind(user_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten()
            .flatten();

            let out = json!({
                "type": "message",
                "id": message_id,
                "room_id": chat_id,
                "sender_id": user_id,
                "sender_username": username,
                "sender_avatar_file_id": sender_avatar_file_id,
                "content": content,
                "timestamp": ts
            });

            let is_voice = chat_is_voice(db, chat_id).await;
            if is_voice {
                if hub.voice_get_conn_channel(conn_id) != Some(chat_id) {
                    let _ = tx.try_send(json!({"type": "error", "code": "not_in_voice", "chat_id": chat_id}));
                    return;
                }
                tracing::info!("[WS] broadcast_room Voice chat={}", chat_id);
                hub.broadcast_room(&RoomId::Voice(chat_id), &out);
            } else {
                tracing::info!("[WS] broadcast_room Channel chat={}", chat_id);
                hub.broadcast_room(&RoomId::Channel(chat_id), &out);
            }
        }
        
        "join" => {
            let Some((kind, id)) = parse_legacy_room(&v) else { return; };
            let chat_id = id;

            if !can_access_chat(db, user_id, chat_id).await {
                let _ = tx.try_send(json!({"type":"error","code":"not_member","chat_id": chat_id}));
                return;
            }

            let is_voice = chat_is_voice(db, chat_id).await;

            if is_voice {
                if hub.voice_get_conn_channel(conn_id) != Some(chat_id) {
                    let _ = tx.try_send(json!({"type":"error","code":"not_in_voice","chat_id": chat_id}));
                    return;
                }
                hub.room_join(RoomId::Voice(chat_id), user_id, conn_id, tx.clone());
                let _ = tx.try_send(json!({"type":"joined","room": room_to_json("voice", chat_id)}));
            } else {
                hub.room_join(RoomId::Channel(chat_id), user_id, conn_id, tx.clone());
                let _ = tx.try_send(json!({"type":"joined","room": room_to_json(&kind, chat_id)}));
            }
        }

        // legacy: {type:"message", room:{kind,id}, text:"..."}
        "message" => {
            let Some((_kind, id)) = parse_legacy_room(&v) else { return; };
            let Some(content) = v.get("text").and_then(|x| x.as_str()) else { return; };
            let content = content.trim();
            if content.is_empty() { return; }

            let chat_id = id;
            if !can_access_chat(db, user_id, chat_id).await {
                let _ = tx.try_send(json!({"type":"error","code":"not_member","chat_id": chat_id}));
                return;
            }

            let ts = auth::now_iso();
            let message_id = persist_message(db, chat_id, user_id, content, &ts).await;

            let sender_avatar_file_id: Option<i64> = sqlx::query_scalar::<_, Option<i64>>(
                "SELECT avatar_file_id FROM user_profile WHERE user_id = $1 LIMIT 1",
            )
            .bind(user_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten()
            .flatten();

            let out = json!({
                "type": "message",
                "id": message_id,
                "room_id": chat_id,
                "sender_id": user_id,
                "sender_username": username,
                "sender_avatar_file_id": sender_avatar_file_id,
                "content": content,
                "timestamp": ts
            });

            hub.broadcast_room(&RoomId::Channel(chat_id), &out);
        }

        _ => {}
    }
}

fn broadcast_room_excluding_conn(hub: &Hub, room_id: &RoomId, exclude_conn_id: ConnId, payload: &Value) {
    if let Some(room) = hub.rooms.get(room_id) {
        for user_conns in room.iter() {
            for tx in user_conns.value().iter() {
                if *tx.key() == exclude_conn_id {
                    continue;
                }
                let _ = tx.value().try_send(payload.clone());
            }
        }
    }
}

fn send_to_voice_user(hub: &Hub, channel_id: i64, user_id: UserId, payload: &Value) -> bool {
    let room_id = RoomId::Voice(channel_id);
    let Some(room) = hub.rooms.get(&room_id) else {
        return false;
    };
    let Some(conns) = room.get(&user_id) else {
        return false;
    };

    let mut sent = false;
    for tx in conns.iter() {
        if tx.value().try_send(payload.clone()).is_ok() {
            sent = true;
        }
    }
    sent
}

fn voice_peers(hub: &Hub, channel_id: i64, exclude_user: Option<i64>) -> Vec<i64> {
    let mut out = Vec::new();
    if let Some(room) = hub.rooms.get(&RoomId::Voice(channel_id)) {
        for user_conns in room.iter() {
            let uid = *user_conns.key();
            if exclude_user == Some(uid) {
                continue;
            }
            out.push(uid);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn voice_leave_internal(
    hub: &Hub,
    user_id: UserId,
    conn_id: ConnId,
    tx: &mpsc::Sender<Value>,
    channel_id: i64,
    switched: bool,
) {
    voice_leave_conn_internal(hub, user_id, conn_id, Some(tx), channel_id, switched);
}

fn voice_leave_conn_internal(
    hub: &Hub,
    user_id: UserId,
    conn_id: ConnId,
    tx: Option<&mpsc::Sender<Value>>,
    channel_id: i64,
    switched: bool,
) {
    hub.room_leave(&RoomId::Voice(channel_id), user_id, conn_id);
    hub.voice_clear(user_id, conn_id);

    // If user was sharing screen in this voice channel — clear and notify
    if hub.ss_is_on(channel_id, user_id) {
        hub.ss_set(channel_id, user_id, false);
        let payload = json!({
            "type": "voice_ss_stopped",
            "channel_id": channel_id,
            "user_id": user_id,
            "timestamp": chrono::Utc::now().timestamp_millis()
        });
        broadcast_room_excluding_conn(hub, &RoomId::Voice(channel_id), conn_id, &payload);
    }

    let left = json!({
        "type": "voice_left",
        "channel_id": channel_id,
        "switched": switched,
        "timestamp": chrono::Utc::now().timestamp_millis()
    });
    if let Some(tx) = tx {
        let _ = tx.try_send(left);
    } else {
        let _ = hub.send_to_conn(conn_id, &left);
    }

    let payload = json!({
        "type": "voice_peer_left",
        "channel_id": channel_id,
        "user_id": user_id,
        "timestamp": chrono::Utc::now().timestamp_millis()
    });
    broadcast_room_excluding_conn(hub, &RoomId::Voice(channel_id), conn_id, &payload);
}

async fn is_voice_allowed(db: &PgPool, chat_id: i64) -> bool {
    // Allowed:
    // 1) real voice channel (kind=voice)
    // 2) DM chat (is_private=1 and server_id IS NULL)
    // This enables calls in DMs without creating separate voice chats in DB.

    let row = sqlx::query("SELECT COALESCE(kind, 'text') AS kind, is_private, server_id FROM chats WHERE id = $1")
        .bind(chat_id)
        .fetch_optional(db)
        .await;

    let Ok(Some(r)) = row else { return false; };

    let kind: String = r.get("kind");
    let is_private: bool = r.get("is_private");
    let server_id: Option<i64> = r.get("server_id");

    if kind == "voice" {
        return true;
    }

    is_private && server_id.is_none()
}

async fn persist_message(db: &PgPool, chat_id: i64, user_id: UserId, content: &str, ts: &chrono::DateTime<chrono::Utc>) -> i64 {
    let res = sqlx::query_scalar::<_, i64>(
        r#"INSERT INTO messages (chat_id, sender_id, content, timestamp)
           VALUES ($1, $2, $3, $4) RETURNING id"#,
    )
    .bind(chat_id)
    .bind(user_id)
    .bind(content)
    .bind(ts)
    .fetch_one(db)
    .await;

    res.unwrap_or(0)
}

async fn dm_peer_user_id(db: &PgPool, chat_id: i64, me: UserId) -> Option<UserId> {
    // Only for DM chats: is_private=1 AND server_id IS NULL.
    let meta = sqlx::query_as::<_, (Option<i64>, bool)>(
        "SELECT server_id, is_private FROM chats WHERE id = $1 LIMIT 1",
    )
    .bind(chat_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()?;

    let (server_id, is_private) = meta;
    if server_id.is_some() || !is_private {
        return None;
    }

    sqlx::query_scalar::<_, i64>(
        "SELECT user_id FROM chat_participants WHERE chat_id = $1 AND user_id <> $2 LIMIT 1",
    )
    .bind(chat_id)
    .bind(me)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

async fn chat_is_voice(db: &PgPool, chat_id: i64) -> bool {
    sqlx::query_scalar::<_, String>("SELECT COALESCE(kind, 'text') FROM chats WHERE id = $1 LIMIT 1")
        .bind(chat_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|k| k == "voice")
        .unwrap_or(false)
}

async fn can_access_chat(db: &PgPool, user_id: UserId, chat_id: i64) -> bool {
    let meta = sqlx::query_as::<_, (Option<i64>, bool)>(
        "SELECT server_id, is_private FROM chats WHERE id = $1 LIMIT 1",
    )
    .bind(chat_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some((server_id, is_private)) = meta else {
        return false;
    };

    if is_private {
        return sqlx::query_scalar::<_, i64>(
            "SELECT 1::bigint FROM chat_participants WHERE chat_id = $1 AND user_id = $2 LIMIT 1",
        )
        .bind(chat_id)
        .bind(user_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .is_some();
    }

    if let Some(sid) = server_id {
        return sqlx::query_scalar::<_, i64>(
            "SELECT 1::bigint FROM server_members WHERE server_id = $1 AND user_id = $2 LIMIT 1",
        )
        .bind(sid)
        .bind(user_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .is_some();
    }

    sqlx::query_scalar::<_, i64>(
        "SELECT 1::bigint FROM chat_participants WHERE chat_id = $1 AND user_id = $2 LIMIT 1",
    )
    .bind(chat_id)
    .bind(user_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some()
}

async fn get_accessible_rooms(db: &PgPool, user_id: UserId) -> Vec<RoomId> {
    let ids = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT c.id
        FROM chats c
        JOIN chat_participants p ON p.chat_id = c.id
        WHERE p.user_id = $1
          AND COALESCE(c.kind, 'text') <> 'voice'

        UNION

        SELECT c.id
        FROM chats c
        JOIN server_members sm ON sm.server_id = c.server_id
        WHERE sm.user_id = $1
          AND c.server_id IS NOT NULL
          AND NOT c.is_private
          AND COALESCE(c.kind, 'text') <> 'voice'
        "#,
    )
    .bind(user_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    ids.into_iter().map(RoomId::Channel).collect()
}

fn parse_legacy_room(v: &Value) -> Option<(String, i64)> {
    let kind = v.get("room")?.get("kind")?.as_str()?;
    let id = v.get("room")?.get("id")?.as_i64()?;

    match kind {
        "channel" => Some(("channel".to_string(), id)),
        "dm" => Some(("dm".to_string(), id)),
        _ => None,
    }
}

fn room_to_json(kind: &str, id: i64) -> Value {
    json!({"kind": kind, "id": id})
}
