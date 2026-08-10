use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use chrono::{DateTime, Utc};
use sea_query::Iden;

#[derive(Iden)]
#[sea_query(rename = "ai_settings")]
pub enum AiSettingIden {
    Table,
    Id,
    Enabled,
    BaseUrl,
    Model,
    UserName,
    Label,
    Mode,
    DmEnabled,
    ChannelEnabled,
    AcceptFriendRequests,
    AcceptServerJoinRequests,
    StartDmEnabled,
    DmCooldownSeconds,
    ChannelCooldownSeconds,
    ContextMessages,
    MaxTokens,
    Temperature,
    TopP,
    SystemPrompt,
    UpdatedAt,
    KindnessScore,
    NoReplyCount,
    ViolationCount,
    LastEventAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AiSetting {
    pub id: i64,
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
    pub user_name: String,
    pub label: String,
    pub mode: String,
    pub dm_enabled: bool,
    pub channel_enabled: bool,
    pub accept_friend_requests: bool,
    pub accept_server_join_requests: bool,
    pub start_dm_enabled: bool,
    pub dm_cooldown_seconds: i32,
    pub channel_cooldown_seconds: i32,
    pub context_messages: i32,
    pub max_tokens: i32,
    pub temperature: f64,
    pub top_p: f64,
    pub system_prompt: String,
    pub updated_at: DateTime<Utc>,
    pub kindness_score: i32,
    pub no_reply_count: i32,
    pub violation_count: i32,
    pub last_event_at: Option<DateTime<Utc>>,
}

#[derive(Iden)]
#[sea_query(rename = "ai_chat_state")]
pub enum AiChatStateIden {
    Table,
    ChatId,
    LastReplyAt,
    LastSeenMessageId,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AiChatState {
    pub chat_id: i64,
    pub last_reply_at: Option<DateTime<Utc>>,
    pub last_seen_message_id: i64,
}