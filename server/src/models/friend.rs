use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use chrono::{DateTime, Utc};
use sea_query::Iden;

#[derive(Iden)]
#[sea_query(rename = "friendships")]
pub enum FriendshipIden {
    Table,
    Id,
    UserId,
    FriendId,
    CreatedAt,
    IsFavorite,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Friendship {
    pub id: i64,
    pub user_id: i64,
    pub friend_id: i64,
    pub created_at: DateTime<Utc>,
    pub is_favorite: bool,
}

#[derive(Iden)]
#[sea_query(rename = "friend_requests")]
pub enum FriendRequestIden {
    Table,
    Id,
    SenderId,
    ReceiverId,
    Status,
    CreatedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct FriendRequest {
    pub id: i64,
    pub sender_id: i64,
    pub receiver_id: i64,
    pub status: String, // pending, accepted, rejected
    pub created_at: DateTime<Utc>,
}