use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use chrono::{DateTime, Utc};
use sea_query::Iden;

#[derive(Iden)]
#[sea_query(rename = "e2ee_room_keys")]
pub enum E2eeRoomKeyIden {
    Table,
    UserId,
    ChatId,
    EncryptedKey,
    Nonce,
    CreatedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct E2eeRoomKey {
    pub user_id: i64,
    pub chat_id: i64,
    pub encrypted_key: String,
    pub nonce: String,
    pub created_at: DateTime<Utc>,
}