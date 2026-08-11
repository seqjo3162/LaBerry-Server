use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use chrono::{DateTime, Utc};
use sea_query::Iden;

#[derive(Iden)]
#[iden(rename = "dm_chats")]
pub enum DmChatIden {
    Table,
    ChatId,
    UserA,
    UserB,
    CreatedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct DmChat {
    pub chat_id: i64,
    pub user_a: i64,
    pub user_b: i64,
    pub created_at: DateTime<Utc>,
}