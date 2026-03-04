use axum::extract::ws::{Message, WebSocket};
use dashmap::DashMap;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tokio::sync::mpsc;
use chrono;

macro_rules! ws_debug {
    ($($arg:tt)*) => {{
        let enabled = std::env::var("LB_DEBUG_WS")
            .ok()
            .map(|v| {
                let v = v.trim().to_ascii_lowercase();
                v == "1" || v == "true" || v == "yes" || v == "on"
            })
            .unwrap_or(false);
        if enabled {
            println!($($arg)*);
        }
    }};
}

pub type UserId = i64;
pub type ConnId = u64;
pub type VoiceChannelId = i64;

static CONN_ID_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RoomId {
    Channel(i64),
    Dm(i64),
    Voice(i64),
}

pub mod chat;
pub mod presence;
pub mod friends_events;

#[derive(Clone)]
pub struct Hub {
    pub presence: Arc<DashMap<UserId, DashMap<ConnId, mpsc::UnboundedSender<Value>>>>,
    pub rooms: Arc<DashMap<RoomId, DashMap<UserId, DashMap<ConnId, mpsc::UnboundedSender<Value>>>>>,
    pub active_conn: Arc<DashMap<UserId, ConnId>>,
    pub conn_details: Arc<DashMap<ConnId, ConnectionDetail>>,
    pub user_locks: Arc<DashMap<UserId, Arc<tokio::sync::Notify>>>,
    pub voice_by_conn: Arc<DashMap<ConnId, VoiceChannelId>>,
    pub voice_by_user: Arc<DashMap<UserId, VoiceChannelId>>,
    pub ss_by_voice: Arc<DashMap<VoiceChannelId, DashMap<UserId, ()>>>,
}

#[derive(Clone)]
pub struct ConnectionDetail {
    pub user_id: UserId,
    pub tx: mpsc::UnboundedSender<Value>,
    pub created_at: std::time::Instant,
    pub is_closing: Arc<AtomicBool>,
}

impl Hub {
    pub fn new() -> Self {
        Self {
            presence: Arc::new(DashMap::new()),
            rooms: Arc::new(DashMap::new()),
            active_conn: Arc::new(DashMap::new()),
            conn_details: Arc::new(DashMap::new()),
            user_locks: Arc::new(DashMap::new()),
            voice_by_conn: Arc::new(DashMap::new()),
            voice_by_user: Arc::new(DashMap::new()),
            ss_by_voice: Arc::new(DashMap::new()),
        }
    }

    pub fn voice_get_user_channel(&self, user_id: UserId) -> Option<VoiceChannelId> {
        self.voice_by_user.get(&user_id).map(|v| *v)
    }

    pub fn voice_get_conn_channel(&self, conn_id: ConnId) -> Option<VoiceChannelId> {
        self.voice_by_conn.get(&conn_id).map(|v| *v)
    }

    pub fn voice_set(
        &self,
        user_id: UserId,
        conn_id: ConnId,
        channel_id: VoiceChannelId,
    ) -> Option<VoiceChannelId> {
        let prev_user = self.voice_by_user.insert(user_id, channel_id);
        let prev_conn = self.voice_by_conn.insert(conn_id, channel_id);
        prev_user.or(prev_conn)
    }

    pub fn voice_clear(&self, user_id: UserId, conn_id: ConnId) -> Option<VoiceChannelId> {
        let prev_user = self.voice_by_user.remove(&user_id).map(|(_, v)| v);
        let prev_conn = self.voice_by_conn.remove(&conn_id).map(|(_, v)| v);
        prev_user.or(prev_conn)
    }

    pub fn ss_set(&self, channel_id: VoiceChannelId, user_id: UserId, is_on: bool) {
        if channel_id <= 0 || user_id <= 0 {
            return;
        }

        if is_on {
            let set = self.ss_by_voice.entry(channel_id).or_insert_with(DashMap::new);
            set.insert(user_id, ());
        } else {
            if let Some(set) = self.ss_by_voice.get_mut(&channel_id) {
                set.remove(&user_id);
                if set.is_empty() {
                    drop(set);
                    self.ss_by_voice.remove(&channel_id);
                }
            }
        }
    }

    pub fn ss_list(&self, channel_id: VoiceChannelId) -> Vec<UserId> {
        if channel_id <= 0 {
            return Vec::new();
        }
        let mut out: Vec<UserId> = Vec::new();
        if let Some(set) = self.ss_by_voice.get(&channel_id) {
            for kv in set.iter() {
                out.push(*kv.key());
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn ss_is_on(&self, channel_id: VoiceChannelId, user_id: UserId) -> bool {
        if channel_id <= 0 || user_id <= 0 {
            return false;
        }
        self.ss_by_voice
            .get(&channel_id)
            .map(|set| set.contains_key(&user_id))
            .unwrap_or(false)
    }

    pub fn presence_join(
        &self,
        user_id: UserId,
        conn_id: ConnId,
        tx: mpsc::UnboundedSender<Value>,
    ) {
        let user_conns = self
            .presence
            .entry(user_id)
            .or_insert_with(DashMap::new);

        user_conns.insert(conn_id, tx.clone());
        
        self.conn_details.insert(conn_id, ConnectionDetail {
            user_id,
            tx,
            created_at: std::time::Instant::now(),
            is_closing: Arc::new(AtomicBool::new(false)),
        });
    }

    pub fn presence_leave(&self, user_id: UserId, conn_id: ConnId) {
        if let Some(conns) = self.presence.get_mut(&user_id) {
            conns.remove(&conn_id);
            if conns.is_empty() {
                drop(conns);
                self.presence.remove(&user_id);
            }
        }
        
        self.conn_details.remove(&conn_id);
    }

    pub fn room_join(
        &self,
        room_id: RoomId,
        user_id: UserId,
        conn_id: ConnId,
        tx: mpsc::UnboundedSender<Value>,
    ) {
        let room = self.rooms.entry(room_id).or_insert_with(DashMap::new);
        let user_conns = room.entry(user_id).or_insert_with(DashMap::new);
        user_conns.insert(conn_id, tx);
    }

    pub fn room_leave(
        &self,
        room_id: &RoomId,
        user_id: UserId,
        conn_id: ConnId,
    ) {
        if let Some(room) = self.rooms.get_mut(room_id) {
            if let Some(conns) = room.get_mut(&user_id) {
                conns.remove(&conn_id);
                if conns.is_empty() {
                    drop(conns);
                    room.remove(&user_id);
                }
            }

            if room.is_empty() {
                drop(room);
                self.rooms.remove(room_id);
            }
        }
    }

    pub fn broadcast_room(&self, room_id: &RoomId, payload: &Value) {
        if let Some(room) = self.rooms.get(room_id) {
            for user_conns in room.iter() {
                for tx in user_conns.value().iter() {
                    let _ = tx.value().send(payload.clone());
                }
            }
        }
    }
    pub fn broadcast_room_except_user(&self, room_id: &RoomId, exclude_user: UserId, payload: &Value) {
        if let Some(room) = self.rooms.get(room_id) {
            for user_conns in room.iter() {
                if *user_conns.key() == exclude_user {
                    continue;
                }
                for tx in user_conns.value().iter() {
                    let _ = tx.value().send(payload.clone());
                }
            }
        }
    }

    pub fn send_to_user(&self, user_id: UserId, payload: &Value) {
        if let Some(conns) = self.presence.get(&user_id) {
            for tx in conns.iter() {
                let _ = tx.value().send(payload.clone());
            }
        }
    }

    pub fn send_to_conn(&self, conn_id: ConnId, payload: &Value) -> bool {
        if let Some(detail) = self.conn_details.get(&conn_id) {
            if detail.is_closing.load(Ordering::Relaxed) {
                return false;
            }
            return detail.tx.send(payload.clone()).is_ok();
        }
        false
    }

    pub fn broadcast_presence(&self, payload: &Value) {
        for user_conns in self.presence.iter() {
            for tx in user_conns.value().iter() {
                let _ = tx.value().send(payload.clone());
            }
        }
    }

    pub async fn swap_connection(&self, user_id: UserId, new_conn_id: ConnId) -> Option<ConnId> {
        let old_conn = self.active_conn.insert(user_id, new_conn_id);
        
        if let Some(old_conn_id) = old_conn {
            if let Some(detail) = self.conn_details.get(&old_conn_id) {
                detail.is_closing.store(true, Ordering::Relaxed);
                let _ = detail.tx.send(json!({
                    "type": "connection_taken_over",
                    "new_connection_id": new_conn_id,
                    "timestamp": chrono::Utc::now().timestamp_millis()
                }));
            }
            
            let hub_clone = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                hub_clone.cleanup_conn(user_id, old_conn_id).await;
            });
        }
        
        old_conn
    }
    
    pub fn get_active_conn(&self, user_id: UserId) -> Option<ConnId> {
        self.active_conn.get(&user_id).map(|c| *c)
    }
    
    pub fn is_conn_active(&self, conn_id: ConnId) -> bool {
        self.conn_details
            .get(&conn_id)
            .map(|d| !d.is_closing.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    pub async fn cleanup_conn(&self, user_id: UserId, conn_id: ConnId) {
        ws_debug!("[HUB] cleanup_conn: user={}, conn={}", user_id, conn_id);

        let voice_channel = self
            .voice_by_conn
            .get(&conn_id)
            .map(|v| *v)
            .or_else(|| self.voice_by_user.get(&user_id).map(|v| *v));

        let should_remove_from_active = match self.active_conn.get(&user_id) {
            Some(active_conn_id) => *active_conn_id.value() == conn_id,
            None => false,
        };

        self.presence_leave(user_id, conn_id);

        let room_ids: Vec<RoomId> = self.rooms.iter().map(|e| e.key().clone()).collect();
        for room_id in room_ids {
            self.room_leave(&room_id, user_id, conn_id);
        }

        self.voice_by_conn.remove(&conn_id);
        self.voice_by_user.remove(&user_id);

        if let Some(ch) = voice_channel {
            let payload = json!({
                "type": "voice_peer_left",
                "channel_id": ch,
                "user_id": user_id,
                "timestamp": chrono::Utc::now().timestamp_millis()
            });
            self.broadcast_room(&RoomId::Voice(ch), &payload);
        }
        
        if should_remove_from_active {
            ws_debug!("[HUB] Removing from active_conn: user={}, conn={}", user_id, conn_id);
            self.active_conn.remove(&user_id);
        }

        self.conn_details.remove(&conn_id);

        if let Some(notify) = self.user_locks.get(&user_id) {
            notify.value().notify_one();
        }
    }
    
    pub async fn wait_for_user_available(&self, user_id: UserId, timeout_ms: u64) -> bool {
    if let Some(active_conn_id) = self.get_active_conn(user_id) {
        ws_debug!("[HUB] User {} has active connection {}, waiting...", user_id, active_conn_id);
        
        let notify = {
            let entry = self.user_locks.entry(user_id);
            let notify_ref = entry.or_insert_with(|| Arc::new(tokio::sync::Notify::new()));
            Arc::clone(&notify_ref)
        };
        
        tokio::select! {
            _ = notify.notified() => {
                ws_debug!("[HUB] User {} is now available", user_id);
                true
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)) => {
                ws_debug!("[HUB] Timeout waiting for user {}", user_id);
                false
            }
        }
    } else {
        ws_debug!("[HUB] User {} has no active connection, available immediately", user_id);
        true
    }
}
}

pub async fn handle(
    mut socket: WebSocket,
    _db: SqlitePool,
    hub: Arc<Hub>,
    user_id: UserId,
) {
    let conn_id: ConnId = CONN_ID_SEQ.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();

    ws_debug!("[WS] user={} new conn={} requesting connection", user_id, conn_id);
    ws_debug!("[WS] handle: user={} conn={} waiting for lock", user_id, conn_id);

    if let Some(old_conn_id) = hub.get_active_conn(user_id) {
        ws_debug!("[WS] User {} already has active connection {}, taking over", user_id, old_conn_id);
    }

    let old_conn = hub.swap_connection(user_id, conn_id).await;
    if let Some(old_conn_id) = old_conn {
        ws_debug!("[WS] user={} replaced old conn={} with new conn={}", user_id, old_conn_id, conn_id);
    }

    hub.presence_join(user_id, conn_id, tx.clone());

    ws_debug!("[WS] user={} conn={} entering main loop", user_id, conn_id);
    
    loop {
        tokio::select! {
            Some(payload) = rx.recv() => {
                if socket.send(Message::Text(payload.to_string())).await.is_err() {
                    break;
                }
            }

            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }

    ws_debug!("[WS] user={} conn={} cleaning up", user_id, conn_id);
    hub.cleanup_conn(user_id, conn_id).await;
}
