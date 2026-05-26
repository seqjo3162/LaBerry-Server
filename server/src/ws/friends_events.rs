use serde_json::json;

use super::{Hub, UserId};

pub async fn friend_request_received(hub: &Hub, receiver_id: UserId, sender_id: UserId) {
    let payload = json!({
        "type": "friend_request",
        "event": "received",
        "user_id": sender_id
    });
    hub.send_to_user(receiver_id, &payload);
}

pub async fn friend_request_accepted(hub: &Hub, a: UserId, b: UserId) {
    let payload_a = json!({
        "type": "friend_request",
        "event": "accepted",
        "user_id": b
    });
    let payload_b = json!({
        "type": "friend_request",
        "event": "accepted",
        "user_id": a
    });
    hub.send_to_user(a, &payload_a);
    hub.send_to_user(b, &payload_b);
}

pub async fn friend_removed(hub: &Hub, a: UserId, b: UserId) {
    let payload_a = json!({
        "type": "friend",
        "event": "removed",
        "user_id": b
    });
    let payload_b = json!({
        "type": "friend",
        "event": "removed",
        "user_id": a
    });
    hub.send_to_user(a, &payload_a);
    hub.send_to_user(b, &payload_b);
}
