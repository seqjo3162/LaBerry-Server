use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::{Hub, UserId};

pub async fn handle_friends_events() {
    println!("Friends events handler");
}

pub async fn friend_request_received(
    hub: &Arc<RwLock<Hub>>,
    receiver_id: UserId,
    sender_id: UserId,
) {
    let payload = json!({
        "event": "friend.request",
        "user_id": sender_id
    });

    hub.read().await.send_to_user(receiver_id, &payload);
}

pub async fn friend_request_accepted(
    hub: &Arc<RwLock<Hub>>,
    a: UserId,
    b: UserId,
) {
    let payload_a = json!({
        "event": "friend.accept",
        "user_id": b
    });

    let payload_b = json!({
        "event": "friend.accept",
        "user_id": a
    });

    let h = hub.read().await;
    h.send_to_user(a, &payload_a);
    h.send_to_user(b, &payload_b);
}

pub async fn friend_removed(
    hub: &Arc<RwLock<Hub>>,
    a: UserId,
    b: UserId,
) {
    let payload_a = json!({
        "event": "friend.remove",
        "user_id": b
    });

    let payload_b = json!({
        "event": "friend.remove",
        "user_id": a
    });

    let h = hub.read().await;
    h.send_to_user(a, &payload_a);
    h.send_to_user(b, &payload_b);
}
