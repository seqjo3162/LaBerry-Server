use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use chrono::{DateTime, Utc};
use sea_query::Iden;

#[derive(Iden)]
#[sea_query(rename = "files")]
pub enum FileIden {
    Table,
    Id,
    Filename,
    OriginalName,
    FileSize,
    MimeType,
    StoragePath,
    UploadedBy,
    ChatId,
    MessageId,
    CreatedAt,
    ContentHash,
    NormalizedHash,
    ContentHashAlgo,
    StorageKind,
    ExpiresAt,
    DeletedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct File {
    pub id: i64,
    pub filename: String,
    pub original_name: String,
    pub file_size: i64,
    pub mime_type: String,
    pub storage_path: String,
    pub uploaded_by: i64,
    pub chat_id: i64,
    pub message_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub content_hash: Option<String>,
    pub normalized_hash: Option<String>,
    pub content_hash_algo: Option<String>,
    pub storage_kind: String, // temporary, permanent
    pub expires_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
}