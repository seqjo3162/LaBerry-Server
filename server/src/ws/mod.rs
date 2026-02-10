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

// ======================================================
// TYPEDEFS
// ======================================================

pub type UserId = i64;
pub type ConnId = u64;
pub type VoiceChannelId = i64;

static CONN_ID_SEQ: AtomicU64 = AtomicU64::new(1);

// ======================================================
// ROOM ID (каналы + ЛС)
// ======================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RoomId {
    Channel(i64),
    Dm(i64),
}

// Подмодули
pub mod chat;
pub mod presence;
pub mod friends_events;

// ======================================================
// IMPROVED HUB STRUCTURE WITH CONNECTION MANAGEMENT
// ======================================================

#[derive(Clone)]
pub struct Hub {
    /// presence: user_id -> conn_id -> sender
    pub presence: Arc<DashMap<UserId, DashMap<ConnId, mpsc::UnboundedSender<Value>>>>,

    /// rooms: room_id -> user_id -> conn_id -> sender
    pub rooms: Arc<DashMap<RoomId, DashMap<UserId, DashMap<ConnId, mpsc::UnboundedSender<Value>>>>>,

    /// 🔥 idempotent WS: user_id -> active conn_id
    pub active_conn: Arc<DashMap<UserId, ConnId>>,
    
    /// 🔥 NEW: Connection details for quick access and cleanup
    pub conn_details: Arc<DashMap<ConnId, ConnectionDetail>>,
    
    /// 🔥 NEW: User connection locks to prevent race conditions
    pub user_locks: Arc<DashMap<UserId, Arc<tokio::sync::Notify>>>,
}

/// 🔥 NEW: Connection details for management
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
        }
    }

    // ===================
    // PRESENCE
    // ===================
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
        
        // 🔥 Store connection details
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
        
        // 🔥 Remove connection details
        self.conn_details.remove(&conn_id);
    }

    // ===================
    // ROOMS
    // ===================
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

    // ===================
    // DIRECT SEND
    // ===================
    pub fn send_to_user(&self, user_id: UserId, payload: &Value) {
        if let Some(conns) = self.presence.get(&user_id) {
            for tx in conns.iter() {
                let _ = tx.value().send(payload.clone());
            }
        }
    }

    // 🔥 NEW: Send to specific connection
    pub fn send_to_conn(&self, conn_id: ConnId, payload: &Value) -> bool {
        if let Some(detail) = self.conn_details.get(&conn_id) {
            if detail.is_closing.load(Ordering::Relaxed) {
                return false;
            }
            return detail.tx.send(payload.clone()).is_ok();
        }
        false
    }

    // ===================
    // PRESENCE BROADCAST
    // ===================
    pub fn broadcast_presence(&self, payload: &Value) {
        for user_conns in self.presence.iter() {
            for tx in user_conns.value().iter() {
                let _ = tx.value().send(payload.clone());
            }
        }
    }

    // ===================
    // IMPROVED CONNECTION MANAGEMENT
    // ===================
    
    /// 🔥 NEW: Atomic connection swap with immediate cleanup
    pub async fn swap_connection(&self, user_id: UserId, new_conn_id: ConnId) -> Option<ConnId> {
        let old_conn = self.active_conn.insert(user_id, new_conn_id);
        
        if let Some(old_conn_id) = old_conn {
            // Mark old connection as closing
            if let Some(detail) = self.conn_details.get(&old_conn_id) {
                detail.is_closing.store(true, Ordering::Relaxed);
                
                // Send takeover notification to old connection
                let _ = detail.tx.send(json!({
                    "type": "connection_taken_over",
                    "new_connection_id": new_conn_id,
                    "timestamp": chrono::Utc::now().timestamp_millis()
                }));
            }
            
            // Schedule cleanup of old connection
            let hub_clone = self.clone();
            tokio::spawn(async move {
                // Wait a bit for graceful close
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                hub_clone.cleanup_conn(user_id, old_conn_id).await;
            });
        }
        
        old_conn
    }
    
    /// 🔥 NEW: Get current active connection for user
    pub fn get_active_conn(&self, user_id: UserId) -> Option<ConnId> {
        self.active_conn.get(&user_id).map(|c| *c)
    }
    
    /// 🔥 NEW: Check if connection is still active (not closing)
    pub fn is_conn_active(&self, conn_id: ConnId) -> bool {
        self.conn_details
            .get(&conn_id)
            .map(|d| !d.is_closing.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    // ===================
    // IMPROVED CLEANUP CONNECTION
    // ===================
    pub async fn cleanup_conn(&self, user_id: UserId, conn_id: ConnId) {
        println!("[HUB] cleanup_conn: user={}, conn={}", user_id, conn_id);
        
        // Check if this connection is still the active one
        let should_remove_from_active = match self.active_conn.get(&user_id) {
            Some(active_conn_id) => *active_conn_id.value() == conn_id,
            None => false,
        };
        
        // Clean up presence and rooms
        self.presence_leave(user_id, conn_id);

        for entry in self.rooms.iter() {
            let room_id = entry.key().clone();
            self.room_leave(&room_id, user_id, conn_id);
        }

        // Remove from active_conn if it's still the current one
        if should_remove_from_active {
            println!("[HUB] Removing from active_conn: user={}, conn={}", user_id, conn_id);
            self.active_conn.remove(&user_id);
        }
        
        // Clean up connection details
        self.conn_details.remove(&conn_id);
        
        // Notify any waiting locks
        if let Some(notify) = self.user_locks.get(&user_id) {
            notify.value().notify_one();
        }
    }
    
    // 🔥 NEW: Wait for user to be available (for preventing rapid reconnects)
    pub async fn wait_for_user_available(&self, user_id: UserId, timeout_ms: u64) -> bool {
    // Проверяем, есть ли активное соединение
    if let Some(active_conn_id) = self.get_active_conn(user_id) {
        println!("[HUB] User {} has active connection {}, waiting...", user_id, active_conn_id);
        
        // Получаем notify для этого пользователя
        let notify = {
            let entry = self.user_locks.entry(user_id);
            let notify_ref = entry.or_insert_with(|| Arc::new(tokio::sync::Notify::new()));
            Arc::clone(&notify_ref)
        };
        
        // Ждем, пока активное соединение не освободит пользователя
        tokio::select! {
            _ = notify.notified() => {
                println!("[HUB] User {} is now available", user_id);
                true
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)) => {
                println!("[HUB] Timeout waiting for user {}", user_id);
                false
            }
        }
    } else {
        // Нет активного соединения - пользователь доступен сразу
        println!("[HUB] User {} has no active connection, available immediately", user_id);
        true
    }
}
}

// ======================================================
// IMPROVED BASE WS HANDLER (IDEMPOTENT WITH LOCKS)
// ======================================================

pub async fn handle(
    mut socket: WebSocket,
    _db: SqlitePool,
    hub: Arc<Hub>,
    user_id: UserId,
) {
    let conn_id: ConnId = CONN_ID_SEQ.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();

    // ======================
    // WAIT FOR USER TO BE AVAILABLE (PREVENT RAPID RECONNECTS)
    // ======================
    println!(
    "[WS] user={} new conn={} requesting connection",
    user_id, conn_id
    );
    println!("[WS] handle: user={} conn={} waiting for lock", user_id, conn_id);
    
    // Wait up to 1 second for user to become available
    /* 
    if !hub.wait_for_user_available(user_id, 1000).await {
        println!("[WS] user={} timeout waiting for lock, closing", user_id);
        let _ = socket.close().await;
        return;
    }
    */

    // 🔥 ВМЕСТО ЭТОГО: Просто проверяем, есть ли активное соединение

    if let Some(old_conn_id) = hub.get_active_conn(user_id) {
        println!(
            "[WS] User {} already has active connection {}, taking over",
            user_id, old_conn_id
        );
    }

    // ======================
    // ATOMIC CONNECTION SWAP
    // ======================
    let old_conn = hub.swap_connection(user_id, conn_id).await;
    if let Some(old_conn_id) = old_conn {
        println!("[WS] user={} replaced old conn={} with new conn={}", 
                 user_id, old_conn_id, conn_id);
    }

    // регистрируем новый коннект
    hub.presence_join(user_id, conn_id, tx.clone());

    // ======================
    // MAIN LOOP
    // ======================
    println!("[WS] user={} conn={} entering main loop", user_id, conn_id);
    
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

    // ======================
    // CLEANUP (idempotent)
    // ======================
    println!("[WS] user={} conn={} cleaning up", user_id, conn_id);
    hub.cleanup_conn(user_id, conn_id).await;
}
