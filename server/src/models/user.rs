use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use chrono::{DateTime, Utc};
use sea_query::Iden;

#[derive(Iden)]
#[sea_query(rename = "users")]
pub enum UserIden {
    Table,
    Id,
    Username,
    Email,
    EmailVerified,
    EmailPending,
    PasswordHash,
    IsBanned,
    CreatedAt,
    TokenVersion,
    Is2faEnabled,
    TwoFactorSecretCodeHash,
    TwoFactorCodeSentAt,
    PublicEncryptionKey,
    TermsAcceptedAt,
    TermsAgreementVersion,
    CookieConsentStatus,
    CookieConsentAt,
    TrustFactor,
    TrustReviewStatus,
    TrustReviewReason,
    TrustReviewAt,
    IsAi,
    AiLabel,
    TwoFactorCodeExpiresAt,
    TwoFactorCodeAttempts,
    TwoFactorLockedUntil,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub email_pending: Option<String>,
    pub password_hash: String,
    pub is_banned: bool,
    pub created_at: DateTime<Utc>,
    pub token_version: i32,
    pub is_2fa_enabled: bool,
    pub two_factor_secret_code_hash: Option<String>,
    pub two_factor_code_sent_at: Option<DateTime<Utc>>,
    pub public_encryption_key: Option<String>,
    pub terms_accepted_at: Option<DateTime<Utc>>,
    pub terms_agreement_version: Option<String>,
    pub cookie_consent_status: String,
    pub cookie_consent_at: Option<DateTime<Utc>>,
    pub trust_factor: i32,
    pub trust_review_status: String,
    pub trust_review_reason: Option<String>,
    pub trust_review_at: Option<DateTime<Utc>>,
    pub is_ai: bool,
    pub ai_label: Option<String>,
    pub two_factor_code_expires_at: Option<DateTime<Utc>>,
    pub two_factor_code_attempts: i32,
    pub two_factor_locked_until: Option<DateTime<Utc>>,
}

#[derive(Iden)]
#[sea_query(rename = "user_presence")]
pub enum UserPresenceIden {
    Table,
    UserId,
    IsOnline,
    Status,
    UpdatedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UserPresence {
    pub user_id: i64,
    pub is_online: bool,
    pub status: String, // online, offline, away, busy
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Iden)]
#[sea_query(rename = "user_settings")]
pub enum UserSettingsIden {
    Table,
    UserId,
    SettingsJson,
    UpdatedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UserSettings {
    pub user_id: i64,
    pub settings_json: String, // JSON-строка с настройками
    pub updated_at: DateTime<Utc>,
}

#[derive(Iden)]
#[sea_query(rename = "user_blocks")]
pub enum UserBlockIden {
    Table,
    BlockerId,
    BlockedId,
    CreatedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UserBlock {
    pub blocker_id: i64,
    pub blocked_id: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Iden)]
#[sea_query(rename = "user_profile")]
pub enum UserProfileIden {
    Table,
    UserId,
    AvatarFileId,
    BannerFileId,
    AccentColor,
    About,
    StatusText,
    IntegrationsJson,
    UpdatedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UserProfile {
    pub user_id: i64,
    pub avatar_file_id: Option<i64>,
    pub banner_file_id: Option<i64>,
    pub accent_color: Option<String>,
    pub about: Option<String>,
    pub status_text: Option<String>,
    pub integrations_json: String, // JSON
    pub updated_at: DateTime<Utc>,
}

#[derive(Iden)]
#[sea_query(rename = "pinned_messages")]
pub enum PinnedMessageIden {
    Table,
    ChatId,
    MessageId,
    PinnedBy,
    PinnedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PinnedMessage {
    pub chat_id: i64,
    pub message_id: i64,
    pub pinned_by: i64,
    pub pinned_at: DateTime<Utc>,
}

#[derive(Iden)]
#[sea_query(rename = "profile_files")]
pub enum ProfileFileIden {
    Table,
    Id,
    Filename,
    OriginalName,
    FileSize,
    MimeType,
    StoragePath,
    UploadedBy,
    CreatedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ProfileFile {
    pub id: i64,
    pub filename: String,
    pub original_name: String,
    pub file_size: i64,
    pub mime_type: String,
    pub storage_path: String,
    pub uploaded_by: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Iden)]
#[sea_query(rename = "user_sessions")]
pub enum UserSessionIden {
    Table,
    Id,
    UserId,
    TokenHash,
    UserAgent,
    Ip,
    CreatedAt,
    LastSeenAt,
    RevokedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UserSession {
    pub id: i64,
    pub user_id: i64,
    pub token_hash: String,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Iden)]
#[sea_query(rename = "refresh_sessions")]
pub enum RefreshSessionIden {
    Table,
    Id,
    UserId,
    RefreshTokenHash,
    UserAgent,
    Ip,
    CreatedAt,
    LastUsedAt,
    ExpiresAt,
    RevokedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RefreshSession {
    pub id: i64,
    pub user_id: i64,
    pub refresh_token_hash: String,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Iden)]
#[sea_query(rename = "email_codes")]
pub enum EmailCodeIden {
    Table,
    Id,
    UserId,
    Purpose,
    CodeHash,
    SentToEmail,
    CreatedAt,
    ExpiresAt,
    ConsumedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct EmailCode {
    pub id: i64,
    pub user_id: i64,
    pub purpose: String,
    pub code_hash: String,
    pub sent_to_email: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
}

#[derive(Iden)]
#[sea_query(rename = "user_device_keys")]
pub enum UserDeviceKeyIden {
    Table,
    DeviceId,
    UserId,
    PublicJwk,
    Label,
    CreatedAt,
    LastSeen,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UserDeviceKey {
    pub device_id: String,
    pub user_id: i64,
    pub public_jwk: String,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_seen: Option<DateTime<Utc>>,
}

#[derive(Iden)]
#[sea_query(rename = "e2ee_key_pins")]
pub enum E2eeKeyPinIden {
    Table,
    Id,
    UserId,
    DeviceId,
    Fingerprint,
    CreatedAt,
    LastVerifiedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct E2eeKeyPin {
    pub id: i64,
    pub user_id: i64,
    pub device_id: String,
    pub fingerprint: String,
    pub created_at: DateTime<Utc>,
    pub last_verified_at: DateTime<Utc>,
}

#[derive(Iden)]
#[sea_query(rename = "rate_limit_logs")]
pub enum RateLimitLogIden {
    Table,
    Id,
    Key,
    Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RateLimitLog {
    pub id: i64,
    pub key: String,
    pub timestamp: i64, // Unix timestamp
}

#[derive(Iden)]
#[sea_query(rename = "csrf_tokens")]
pub enum CsrfTokenIden {
    Table,
    TokenHash,
    UserId,
    CreatedAt,
    ExpiresAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct CsrfToken {
    pub token_hash: String,
    pub user_id: i64,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Iden)]
#[sea_query(rename = "user_reports")]
pub enum UserReportIden {
    Table,
    Id,
    ReporterId,
    TargetUserId,
    MessageId,
    Reason,
    Message,
    Status,
    CreatedAt,
    ResolvedAt,
    ResolvedBy,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UserReport {
    pub id: i64,
    pub reporter_id: i64,
    pub target_user_id: i64,
    pub message_id: Option<i64>,
    pub reason: String,
    pub message: String,
    pub status: String, // open, resolved, rejected
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolved_by: Option<i64>,
}

#[derive(Iden)]
#[sea_query(rename = "user_suggestions")]
pub enum UserSuggestionIden {
    Table,
    Id,
    UserId,
    Title,
    Message,
    Status,
    CreatedAt,
    ReviewedAt,
    ReviewedBy,
    AdminNote,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UserSuggestion {
    pub id: i64,
    pub user_id: i64,
    pub title: String,
    pub message: String,
    pub status: String, // open, reviewed, closed
    pub created_at: DateTime<Utc>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub reviewed_by: Option<i64>,
    pub admin_note: String,
}

#[derive(Iden)]
#[sea_query(rename = "moderation_events")]
pub enum ModerationEventIden {
    Table,
    Id,
    UserId,
    AdminId,
    Kind,
    Reason,
    Details,
    CreatedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ModerationEvent {
    pub id: i64,
    pub user_id: i64,
    pub admin_id: Option<i64>,
    pub kind: String,
    pub reason: String,
    pub details: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Iden)]
#[sea_query(rename = "two_factor_backup_codes")]
pub enum TwoFactorBackupCodeIden {
    Table,
    Id,
    UserId,
    CodeHash,
    IsUsed,
    UsedAt,
    CreatedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct TwoFactorBackupCode {
    pub id: i64,
    pub user_id: i64,
    pub code_hash: String,
    pub is_used: bool,
    pub used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Iden)]
#[sea_query(rename = "audit_logs")]
pub enum AuditLogIden {
    Table,
    Id,
    UserId,
    Action,
    ResourceType,
    ResourceId,
    Status,
    Details,
    IpAddress,
    UserAgent,
    CreatedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AuditLog {
    pub id: i64,
    pub user_id: Option<i64>,
    pub action: String,
    pub resource_type: Option<String>,
    pub resource_id: Option<i64>,
    pub status: String,
    pub details: Option<String>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Iden)]
#[sea_query(rename = "gif_assets")]
pub enum GifAssetIden {
    Table,
    Id,
    Scope,
    OwnerId,
    SourceFileId,
    Filename,
    OriginalName,
    FileSize,
    MimeType,
    StoragePath,
    CreatedByAdmin,
    CreatedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct GifAsset {
    pub id: i64,
    pub scope: String, // favorite, global
    pub owner_id: Option<i64>,
    pub source_file_id: Option<i64>,
    pub filename: String,
    pub original_name: String,
    pub file_size: i64,
    pub mime_type: String,
    pub storage_path: String,
    pub created_by_admin: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Iden)]
#[sea_query(rename = "app_downloads")]
pub enum AppDownloadIden {
    Table,
    Id,
    Platform,
    Version,
    OriginalName,
    MimeType,
    FileSize,
    StoragePath,
    UploadedAt,
    IsActive,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AppDownload {
    pub id: i64,
    pub platform: String,
    pub version: String,
    pub original_name: String,
    pub mime_type: String,
    pub file_size: i64,
    pub storage_path: String,
    pub uploaded_at: DateTime<Utc>,
    pub is_active: bool,
}

#[derive(Iden)]
#[sea_query(rename = "user_key_backups")]
pub enum UserKeyBackupIden {
    Table,
    UserId,
    BlobPassword,
    SaltPassword,
    BlobEmail,
    SaltEmail,
    UpdatedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UserKeyBackup {
    pub user_id: i64,
    pub blob_password: Option<String>,
    pub salt_password: Option<String>,
    pub blob_email: Option<String>,
    pub salt_email: Option<String>,
    pub updated_at: DateTime<Utc>,
}
