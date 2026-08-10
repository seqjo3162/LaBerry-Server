use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use chrono::{DateTime, Utc};
use sea_query::Iden;

#[derive(Iden)]
#[sea_query(rename = "servers")]
pub enum ServerIden {
    Table,
    Id,
    Name,
    OwnerId,
    CreatedAt,
    IsPublic,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Server {
    pub id: i64,
    pub name: String,
    pub owner_id: i64,
    pub created_at: DateTime<Utc>,
    pub is_public: bool,
}

#[derive(Iden)]
#[sea_query(rename = "server_members")]
pub enum ServerMemberIden {
    Table,
    ServerId,
    UserId,
    Role,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ServerMember {
    pub server_id: i64,
    pub user_id: i64,
    pub role: String, // "member", "admin", "owner"
}

#[derive(Iden)]
#[sea_query(rename = "server_join_requests")]
pub enum ServerJoinRequestIden {
    Table,
    Id,
    ServerId,
    RequesterId,
    FromServerId,
    Status,
    CreatedAt,
    DecidedAt,
    DecidedBy,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ServerJoinRequest {
    pub id: i64,
    pub server_id: i64,
    pub requester_id: i64,
    pub from_server_id: Option<i64>,
    pub status: String, // pending, accepted, rejected
    pub created_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_by: Option<i64>,
}