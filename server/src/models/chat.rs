use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use chrono::{DateTime, Utc};
use sea_query::Iden;

#[derive(Iden)]
#[iden(rename = "chats")]
pub enum ChatIden {
    Table,
    Id,
    Name,
    ServerId,
    IsPrivate,
    CreatedAt,
    Kind,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Chat {
    pub id: i64,
    pub name: Option<String>,
    pub server_id: Option<i64>,
    pub is_private: bool,
    pub created_at: DateTime<Utc>,
    pub kind: String, // "text", "voice"
}

#[derive(Iden)]
#[iden(rename = "chat_participants")]
pub enum ChatParticipantIden {
    Table,
    ChatId,
    UserId,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ChatParticipant {
    pub chat_id: i64,
    pub user_id: i64,
}

#[derive(Iden)]
#[iden(rename = "chat_reads")]
pub enum ChatReadIden {
    Table,
    ChatId,
    UserId,
    LastReadMessageId,
    UpdatedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ChatRead {
    pub chat_id: i64,
    pub user_id: i64,
    pub last_read_message_id: i64,
    pub updated_at: DateTime<Utc>,
}

