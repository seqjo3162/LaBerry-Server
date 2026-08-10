use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use chrono::{DateTime, Utc};
use sea_query::Iden;

#[derive(Iden)]
#[sea_query(rename = "messages")]
pub enum MessageIden {
    Table,
    Id,
    ChatId,
    SenderId,
    Content,
    Timestamp,
    ReplyToMessageId,
    EditedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Message {
    pub id: i64,
    pub chat_id: i64,
    pub sender_id: i64,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub reply_to_message_id: Option<i64>,
    pub edited_at: Option<DateTime<Utc>>,
}

#[derive(Iden)]
#[sea_query(rename = "message_reactions")]
pub enum MessageReactionIden {
    Table,
    MessageId,
    UserId,
    Emoji,
    CreatedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct MessageReaction {
    pub message_id: i64,
    pub user_id: i64,
    pub emoji: String,
    pub created_at: DateTime<Utc>,
}