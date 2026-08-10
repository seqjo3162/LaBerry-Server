use sqlx::PgPool;
use chrono::Utc;
use sea_query::{ColumnDef, Table, PostgresQueryBuilder, Iden};
use crate::models::*;

async fn get_applied_versions(db: &PgPool) -> anyhow::Result<Vec<i64>> {
    let rows = sqlx::query_scalar::<_, i64>("SELECT version FROM _schema_version ORDER BY version")
        .fetch_all(db)
        .await
        .unwrap_or_default();
    Ok(rows)
}

async fn mark_applied(db: &PgPool, version: i64) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO _schema_version(version, applied_at) VALUES($1, $2) ON CONFLICT(version) DO NOTHING",
    )
    .bind(version)
    .bind(Utc::now())
    .execute(db)
    .await?;
    Ok(())
}

macro_rules! migration {
    ($db:expr, $versions:expr, $ver:expr, $label:expr, $body:block) => {{
        if !$versions.contains(&$ver) {
            tracing::info!("[DB] Migration {}: {}", $ver, $label);
            $body;
            mark_applied($db, $ver).await?;
        }
    }};
}

async fn exec_sql(db: &PgPool, sql: &str) -> anyhow::Result<()> {
    sqlx::query(sql).execute(db).await?;
    Ok(())
}

async fn column_exists(db: &PgPool, table: &str, column: &str) -> anyhow::Result<bool> {
    let row: Option<(bool,)> = sqlx::query_as(
        "SELECT EXISTS(
            SELECT 1 FROM information_schema.columns
            WHERE table_schema = 'public' AND table_name = $1 AND column_name = $2
        )"
    )
    .bind(table)
    .bind(column)
    .fetch_one(db)
    .await?;
    Ok(row.map(|r| r.0).unwrap_or(false))
}

async fn add_column_if_not_exists(
    db: &PgPool,
    table: &str,
    column_def: &str,
    column_name: &str,
) -> anyhow::Result<()> {
    if column_exists(db, table, column_name).await? {
        return Ok(());
    }
    let sql = format!("ALTER TABLE {} ADD COLUMN {}", table, column_def);
    exec_sql(db, &sql).await?;
    Ok(())
}

async fn create_index_if_not_exists(
    db: &PgPool,
    name: &str,
    table: &str,
    columns: &[&str],
    unique: bool,
    r#where: Option<&str>,
) -> anyhow::Result<()> {
    let mut sql = String::new();
    sql.push_str("CREATE ");
    if unique {
        sql.push_str("UNIQUE ");
    }
    sql.push_str("INDEX IF NOT EXISTS ");
    sql.push_str(name);
    sql.push_str(" ON ");
    sql.push_str(table);
    sql.push_str(" (");
    sql.push_str(&columns.join(", "));
    if let Some(cond) = r#where {
        sql.push_str(&format!(" WHERE {}", cond));
    }
    sql.push_str(");");
    exec_sql(db, &sql).await?;
    Ok(())
}

pub async fn init(db: &PgPool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS _schema_version (
            version BIGINT PRIMARY KEY,
            applied_at TIMESTAMPTZ NOT NULL
        );",
    )
    .execute(db)
    .await?;

    let applied = get_applied_versions(db).await?;

    migration!(db, applied, 1, "Initial schema", {
        let sql = Table::create()
            .table(UserIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(UserIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(UserIden::Username).string().not_null().unique_key())
            .col(ColumnDef::new(UserIden::Email).string().unique_key())
            .col(ColumnDef::new(UserIden::EmailVerified).boolean().not_null().default(false))
            .col(ColumnDef::new(UserIden::EmailPending).string())
            .col(ColumnDef::new(UserIden::PasswordHash).string().not_null())
            .col(ColumnDef::new(UserIden::IsBanned).boolean().not_null().default(false))
            .col(ColumnDef::new(UserIden::CreatedAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(UserIden::TokenVersion).integer().not_null().default(1))
            .col(ColumnDef::new(UserIden::Is2faEnabled).boolean().not_null().default(false))
            .col(ColumnDef::new(UserIden::TwoFactorSecretCodeHash).string())
            .col(ColumnDef::new(UserIden::TwoFactorCodeSentAt).timestamp_with_time_zone())
            .col(ColumnDef::new(UserIden::PublicEncryptionKey).string())
            .col(ColumnDef::new(UserIden::TermsAcceptedAt).timestamp_with_time_zone())
            .col(ColumnDef::new(UserIden::TermsAgreementVersion).string())
            .col(ColumnDef::new(UserIden::CookieConsentStatus).string().not_null().default("unknown"))
            .col(ColumnDef::new(UserIden::CookieConsentAt).timestamp_with_time_zone())
            .col(ColumnDef::new(UserIden::TrustFactor).integer().not_null().default(100))
            .col(ColumnDef::new(UserIden::TrustReviewStatus).string().not_null().default("clear"))
            .col(ColumnDef::new(UserIden::TrustReviewReason).string())
            .col(ColumnDef::new(UserIden::TrustReviewAt).timestamp_with_time_zone())
            .col(ColumnDef::new(UserIden::IsAi).boolean().not_null().default(false))
            .col(ColumnDef::new(UserIden::AiLabel).string())
            .col(ColumnDef::new(UserIden::TwoFactorCodeExpiresAt).timestamp_with_time_zone())
            .col(ColumnDef::new(UserIden::TwoFactorCodeAttempts).integer().not_null().default(0))
            .col(ColumnDef::new(UserIden::TwoFactorLockedUntil).timestamp_with_time_zone())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;

        let sql = Table::create()
            .table(ServerIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(ServerIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(ServerIden::Name).string().not_null())
            .col(ColumnDef::new(ServerIden::OwnerId).big_integer().not_null())
            .col(ColumnDef::new(ServerIden::CreatedAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(ServerIden::IsPublic).boolean().not_null().default(true))
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE servers ADD CONSTRAINT fk_servers_owner_id FOREIGN KEY (owner_id) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_server_owner_id", "servers", &["owner_id"], false, None).await?;

        let sql = Table::create()
            .table(ServerMemberIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(ServerMemberIden::ServerId).big_integer().not_null())
            .col(ColumnDef::new(ServerMemberIden::UserId).big_integer().not_null())
            .col(ColumnDef::new(ServerMemberIden::Role).string().not_null().default("member"))
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE server_members ADD CONSTRAINT uq_server_members UNIQUE(server_id, user_id);").await?;
        exec_sql(db, "ALTER TABLE server_members ADD CONSTRAINT fk_server_members_server_id FOREIGN KEY (server_id) REFERENCES servers(id);").await?;
        exec_sql(db, "ALTER TABLE server_members ADD CONSTRAINT fk_server_members_user_id FOREIGN KEY (user_id) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_server_members_user_id", "server_members", &["user_id"], false, None).await?;

        let sql = Table::create()
            .table(ServerJoinRequestIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(ServerJoinRequestIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(ServerJoinRequestIden::ServerId).big_integer().not_null())
            .col(ColumnDef::new(ServerJoinRequestIden::RequesterId).big_integer().not_null())
            .col(ColumnDef::new(ServerJoinRequestIden::FromServerId).big_integer())
            .col(ColumnDef::new(ServerJoinRequestIden::Status).string().not_null().default("pending"))
            .col(ColumnDef::new(ServerJoinRequestIden::CreatedAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(ServerJoinRequestIden::DecidedAt).timestamp_with_time_zone())
            .col(ColumnDef::new(ServerJoinRequestIden::DecidedBy).big_integer())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE server_join_requests ADD CONSTRAINT uq_server_join_requests UNIQUE(server_id, requester_id);").await?;
        exec_sql(db, "ALTER TABLE server_join_requests ADD CONSTRAINT fk_sjr_server_id FOREIGN KEY (server_id) REFERENCES servers(id);").await?;
        exec_sql(db, "ALTER TABLE server_join_requests ADD CONSTRAINT fk_sjr_requester_id FOREIGN KEY (requester_id) REFERENCES users(id);").await?;
        exec_sql(db, "ALTER TABLE server_join_requests ADD CONSTRAINT fk_sjr_from_server_id FOREIGN KEY (from_server_id) REFERENCES servers(id);").await?;
        exec_sql(db, "ALTER TABLE server_join_requests ADD CONSTRAINT fk_sjr_decided_by FOREIGN KEY (decided_by) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_server_join_requests_server_status", "server_join_requests", &["server_id", "status"], false, None).await?;
        create_index_if_not_exists(db, "ix_server_join_requests_requester", "server_join_requests", &["requester_id"], false, None).await?;

        let sql = Table::create()
            .table(ChatIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(ChatIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(ChatIden::Name).string())
            .col(ColumnDef::new(ChatIden::ServerId).big_integer())
            .col(ColumnDef::new(ChatIden::IsPrivate).boolean().not_null().default(false))
            .col(ColumnDef::new(ChatIden::CreatedAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(ChatIden::Kind).string().not_null().default("text"))
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE chats ADD CONSTRAINT fk_chats_server_id FOREIGN KEY (server_id) REFERENCES servers(id);").await?;
        create_index_if_not_exists(db, "ix_chat_server_id", "chats", &["server_id"], false, None).await?;

        let sql = Table::create()
            .table(ChatParticipantIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(ChatParticipantIden::ChatId).big_integer().not_null())
            .col(ColumnDef::new(ChatParticipantIden::UserId).big_integer().not_null())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE chat_participants ADD CONSTRAINT uq_chat_participants UNIQUE(chat_id, user_id);").await?;
        exec_sql(db, "ALTER TABLE chat_participants ADD CONSTRAINT fk_cp_chat_id FOREIGN KEY (chat_id) REFERENCES chats(id);").await?;
        exec_sql(db, "ALTER TABLE chat_participants ADD CONSTRAINT fk_cp_user_id FOREIGN KEY (user_id) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_chat_participants_user_id", "chat_participants", &["user_id"], false, None).await?;

        let sql = Table::create()
            .table(MessageIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(MessageIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(MessageIden::ChatId).big_integer().not_null())
            .col(ColumnDef::new(MessageIden::SenderId).big_integer().not_null())
            .col(ColumnDef::new(MessageIden::Content).string().not_null())
            .col(ColumnDef::new(MessageIden::Timestamp).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(MessageIden::ReplyToMessageId).big_integer())
            .col(ColumnDef::new(MessageIden::EditedAt).timestamp_with_time_zone())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE messages ADD CONSTRAINT fk_messages_chat_id FOREIGN KEY (chat_id) REFERENCES chats(id);").await?;
        exec_sql(db, "ALTER TABLE messages ADD CONSTRAINT fk_messages_sender_id FOREIGN KEY (sender_id) REFERENCES users(id);").await?;
        exec_sql(db, "ALTER TABLE messages ADD CONSTRAINT fk_messages_reply_to FOREIGN KEY (reply_to_message_id) REFERENCES messages(id);").await?;
        create_index_if_not_exists(db, "ix_messages_chat_id", "messages", &["chat_id"], false, None).await?;
        create_index_if_not_exists(db, "ix_messages_sender_id", "messages", &["sender_id"], false, None).await?;
        create_index_if_not_exists(db, "ix_messages_created_at", "messages", &["timestamp DESC"], false, None).await?;

        let sql = Table::create()
            .table(FileIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(FileIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(FileIden::Filename).string().not_null())
            .col(ColumnDef::new(FileIden::OriginalName).string().not_null())
            .col(ColumnDef::new(FileIden::FileSize).big_integer().not_null())
            .col(ColumnDef::new(FileIden::MimeType).string().not_null())
            .col(ColumnDef::new(FileIden::StoragePath).string().not_null())
            .col(ColumnDef::new(FileIden::UploadedBy).big_integer().not_null())
            .col(ColumnDef::new(FileIden::ChatId).big_integer().not_null())
            .col(ColumnDef::new(FileIden::MessageId).big_integer())
            .col(ColumnDef::new(FileIden::CreatedAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(FileIden::ContentHash).string())
            .col(ColumnDef::new(FileIden::NormalizedHash).string())
            .col(ColumnDef::new(FileIden::ContentHashAlgo).string())
            .col(ColumnDef::new(FileIden::StorageKind).string().not_null().default("temporary"))
            .col(ColumnDef::new(FileIden::ExpiresAt).timestamp_with_time_zone())
            .col(ColumnDef::new(FileIden::DeletedAt).timestamp_with_time_zone())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE files ADD CONSTRAINT fk_files_uploaded_by FOREIGN KEY (uploaded_by) REFERENCES users(id);").await?;
        exec_sql(db, "ALTER TABLE files ADD CONSTRAINT fk_files_chat_id FOREIGN KEY (chat_id) REFERENCES chats(id);").await?;
        exec_sql(db, "ALTER TABLE files ADD CONSTRAINT fk_files_message_id FOREIGN KEY (message_id) REFERENCES messages(id);").await?;
        create_index_if_not_exists(db, "ix_files_chat_id", "files", &["chat_id"], false, None).await?;
        create_index_if_not_exists(db, "ix_files_content_hash", "files", &["content_hash"], false, None).await?;
        create_index_if_not_exists(db, "ix_files_normalized_hash", "files", &["normalized_hash"], false, None).await?;
        create_index_if_not_exists(db, "ix_files_expires_at", "files", &["expires_at"], false, None).await?;
        create_index_if_not_exists(db, "ix_files_storage_path", "files", &["storage_path"], false, None).await?;
        create_index_if_not_exists(db, "ix_files_deleted_at", "files", &["deleted_at"], false, None).await?;

        let sql = Table::create()
            .table(FriendshipIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(FriendshipIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(FriendshipIden::UserId).big_integer().not_null())
            .col(ColumnDef::new(FriendshipIden::FriendId).big_integer().not_null())
            .col(ColumnDef::new(FriendshipIden::CreatedAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(FriendshipIden::IsFavorite).boolean().not_null().default(false))
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE friendships ADD CONSTRAINT uq_friendships UNIQUE(user_id, friend_id);").await?;
        exec_sql(db, "ALTER TABLE friendships ADD CONSTRAINT fk_friendships_user_id FOREIGN KEY (user_id) REFERENCES users(id);").await?;
        exec_sql(db, "ALTER TABLE friendships ADD CONSTRAINT fk_friendships_friend_id FOREIGN KEY (friend_id) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_friendships_user_id", "friendships", &["user_id"], false, None).await?;

        let sql = Table::create()
            .table(FriendRequestIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(FriendRequestIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(FriendRequestIden::SenderId).big_integer().not_null())
            .col(ColumnDef::new(FriendRequestIden::ReceiverId).big_integer().not_null())
            .col(ColumnDef::new(FriendRequestIden::Status).string().not_null().default("pending"))
            .col(ColumnDef::new(FriendRequestIden::CreatedAt).timestamp_with_time_zone().not_null())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE friend_requests ADD CONSTRAINT fk_fr_sender_id FOREIGN KEY (sender_id) REFERENCES users(id);").await?;
        exec_sql(db, "ALTER TABLE friend_requests ADD CONSTRAINT fk_fr_receiver_id FOREIGN KEY (receiver_id) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_friend_requests_receiver_id", "friend_requests", &["receiver_id"], false, None).await?;
        create_index_if_not_exists(db, "ix_friend_requests_sender_id", "friend_requests", &["sender_id"], false, None).await?;

        let sql = Table::create()
            .table(UserPresenceIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(UserPresenceIden::UserId).big_integer().not_null().primary_key())
            .col(ColumnDef::new(UserPresenceIden::IsOnline).boolean().not_null().default(false))
            .col(ColumnDef::new(UserPresenceIden::Status).string().not_null().default("online"))
            .col(ColumnDef::new(UserPresenceIden::UpdatedAt).timestamp_with_time_zone())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;

        let sql = Table::create()
            .table(UserSettingsIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(UserSettingsIden::UserId).big_integer().not_null().primary_key())
            .col(ColumnDef::new(UserSettingsIden::SettingsJson).string().not_null())
            .col(ColumnDef::new(UserSettingsIden::UpdatedAt).timestamp_with_time_zone().not_null())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE user_settings ADD CONSTRAINT fk_us_user_id FOREIGN KEY (user_id) REFERENCES users(id);").await?;

        let sql = Table::create()
            .table(DmChatIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(DmChatIden::ChatId).big_integer().not_null())
            .col(ColumnDef::new(DmChatIden::UserA).big_integer().not_null())
            .col(ColumnDef::new(DmChatIden::UserB).big_integer().not_null())
            .col(ColumnDef::new(DmChatIden::CreatedAt).timestamp_with_time_zone().not_null())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE dm_chats ADD CONSTRAINT uq_dm_chats UNIQUE(user_a, user_b);").await?;
        exec_sql(db, "ALTER TABLE dm_chats ADD CONSTRAINT fk_dm_chat_id FOREIGN KEY (chat_id) REFERENCES chats(id);").await?;
        exec_sql(db, "ALTER TABLE dm_chats ADD CONSTRAINT fk_dm_user_a FOREIGN KEY (user_a) REFERENCES users(id);").await?;
        exec_sql(db, "ALTER TABLE dm_chats ADD CONSTRAINT fk_dm_user_b FOREIGN KEY (user_b) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_dm_chats_user_a", "dm_chats", &["user_a"], false, None).await?;
        create_index_if_not_exists(db, "ix_dm_chats_user_b", "dm_chats", &["user_b"], false, None).await?;

        let sql = Table::create()
            .table(UserBlockIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(UserBlockIden::BlockerId).big_integer().not_null())
            .col(ColumnDef::new(UserBlockIden::BlockedId).big_integer().not_null())
            .col(ColumnDef::new(UserBlockIden::CreatedAt).timestamp_with_time_zone().not_null())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE user_blocks ADD CONSTRAINT uq_user_blocks UNIQUE(blocker_id, blocked_id);").await?;
        exec_sql(db, "ALTER TABLE user_blocks ADD CONSTRAINT fk_ub_blocker_id FOREIGN KEY (blocker_id) REFERENCES users(id);").await?;
        exec_sql(db, "ALTER TABLE user_blocks ADD CONSTRAINT fk_ub_blocked_id FOREIGN KEY (blocked_id) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_user_blocks_blocker", "user_blocks", &["blocker_id"], false, None).await?;

        let sql = Table::create()
            .table(MessageReactionIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(MessageReactionIden::MessageId).big_integer().not_null())
            .col(ColumnDef::new(MessageReactionIden::UserId).big_integer().not_null())
            .col(ColumnDef::new(MessageReactionIden::Emoji).string().not_null())
            .col(ColumnDef::new(MessageReactionIden::CreatedAt).timestamp_with_time_zone().not_null())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE message_reactions ADD CONSTRAINT uq_message_reactions UNIQUE(message_id, user_id, emoji);").await?;
        exec_sql(db, "ALTER TABLE message_reactions ADD CONSTRAINT fk_mr_message_id FOREIGN KEY (message_id) REFERENCES messages(id);").await?;
        exec_sql(db, "ALTER TABLE message_reactions ADD CONSTRAINT fk_mr_user_id FOREIGN KEY (user_id) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_message_reactions_message", "message_reactions", &["message_id"], false, None).await?;

        let sql = Table::create()
            .table(UserProfileIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(UserProfileIden::UserId).big_integer().not_null().primary_key())
            .col(ColumnDef::new(UserProfileIden::AvatarFileId).big_integer())
            .col(ColumnDef::new(UserProfileIden::BannerFileId).big_integer())
            .col(ColumnDef::new(UserProfileIden::AccentColor).string())
            .col(ColumnDef::new(UserProfileIden::About).string())
            .col(ColumnDef::new(UserProfileIden::StatusText).string())
            .col(ColumnDef::new(UserProfileIden::IntegrationsJson).string().not_null().default("{}"))
            .col(ColumnDef::new(UserProfileIden::UpdatedAt).timestamp_with_time_zone().not_null())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE user_profile ADD CONSTRAINT fk_up_user_id FOREIGN KEY (user_id) REFERENCES users(id);").await?;

        let sql = Table::create()
            .table(ChatReadIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(ChatReadIden::ChatId).big_integer().not_null())
            .col(ColumnDef::new(ChatReadIden::UserId).big_integer().not_null())
            .col(ColumnDef::new(ChatReadIden::LastReadMessageId).big_integer().not_null().default(0))
            .col(ColumnDef::new(ChatReadIden::UpdatedAt).timestamp_with_time_zone().not_null())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE chat_reads ADD CONSTRAINT uq_chat_reads UNIQUE(chat_id, user_id);").await?;
        exec_sql(db, "ALTER TABLE chat_reads ADD CONSTRAINT fk_cr_chat_id FOREIGN KEY (chat_id) REFERENCES chats(id);").await?;
        exec_sql(db, "ALTER TABLE chat_reads ADD CONSTRAINT fk_cr_user_id FOREIGN KEY (user_id) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_chat_reads_user", "chat_reads", &["user_id"], false, None).await?;

        let sql = Table::create()
            .table(PinnedMessageIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(PinnedMessageIden::ChatId).big_integer().not_null())
            .col(ColumnDef::new(PinnedMessageIden::MessageId).big_integer().not_null())
            .col(ColumnDef::new(PinnedMessageIden::PinnedBy).big_integer().not_null())
            .col(ColumnDef::new(PinnedMessageIden::PinnedAt).timestamp_with_time_zone().not_null())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE pinned_messages ADD CONSTRAINT uq_pinned_messages UNIQUE(chat_id, message_id);").await?;
        exec_sql(db, "ALTER TABLE pinned_messages ADD CONSTRAINT fk_pm_chat_id FOREIGN KEY (chat_id) REFERENCES chats(id);").await?;
        exec_sql(db, "ALTER TABLE pinned_messages ADD CONSTRAINT fk_pm_message_id FOREIGN KEY (message_id) REFERENCES messages(id);").await?;
        exec_sql(db, "ALTER TABLE pinned_messages ADD CONSTRAINT fk_pm_pinned_by FOREIGN KEY (pinned_by) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_pins_chat", "pinned_messages", &["chat_id"], false, None).await?;

        let sql = Table::create()
            .table(ProfileFileIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(ProfileFileIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(ProfileFileIden::Filename).string().not_null())
            .col(ColumnDef::new(ProfileFileIden::OriginalName).string().not_null())
            .col(ColumnDef::new(ProfileFileIden::FileSize).big_integer().not_null())
            .col(ColumnDef::new(ProfileFileIden::MimeType).string().not_null())
            .col(ColumnDef::new(ProfileFileIden::StoragePath).string().not_null())
            .col(ColumnDef::new(ProfileFileIden::UploadedBy).big_integer().not_null())
            .col(ColumnDef::new(ProfileFileIden::CreatedAt).timestamp_with_time_zone().not_null())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE profile_files ADD CONSTRAINT fk_pf_uploaded_by FOREIGN KEY (uploaded_by) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_profile_files_uploader", "profile_files", &["uploaded_by"], false, None).await?;

        let sql = Table::create()
            .table(UserSessionIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(UserSessionIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(UserSessionIden::UserId).big_integer().not_null())
            .col(ColumnDef::new(UserSessionIden::TokenHash).string().not_null().unique_key())
            .col(ColumnDef::new(UserSessionIden::UserAgent).string())
            .col(ColumnDef::new(UserSessionIden::Ip).string())
            .col(ColumnDef::new(UserSessionIden::CreatedAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(UserSessionIden::LastSeenAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(UserSessionIden::RevokedAt).timestamp_with_time_zone())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE user_sessions ADD CONSTRAINT fk_us_user_id FOREIGN KEY (user_id) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_user_sessions_user_id", "user_sessions", &["user_id"], false, None).await?;
    });

    migration!(db, applied, 2, "Sessions and security tables", {
        let sql = Table::create()
            .table(RefreshSessionIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(RefreshSessionIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(RefreshSessionIden::UserId).big_integer().not_null())
            .col(ColumnDef::new(RefreshSessionIden::RefreshTokenHash).string().not_null().unique_key())
            .col(ColumnDef::new(RefreshSessionIden::UserAgent).string())
            .col(ColumnDef::new(RefreshSessionIden::Ip).string())
            .col(ColumnDef::new(RefreshSessionIden::CreatedAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(RefreshSessionIden::LastUsedAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(RefreshSessionIden::ExpiresAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(RefreshSessionIden::RevokedAt).timestamp_with_time_zone())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE refresh_sessions ADD CONSTRAINT fk_rs_user_id FOREIGN KEY (user_id) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_refresh_sessions_user_id", "refresh_sessions", &["user_id"], false, None).await?;

        let sql = Table::create()
            .table(EmailCodeIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(EmailCodeIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(EmailCodeIden::UserId).big_integer().not_null())
            .col(ColumnDef::new(EmailCodeIden::Purpose).string().not_null())
            .col(ColumnDef::new(EmailCodeIden::CodeHash).string().not_null())
            .col(ColumnDef::new(EmailCodeIden::SentToEmail).string())
            .col(ColumnDef::new(EmailCodeIden::CreatedAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(EmailCodeIden::ExpiresAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(EmailCodeIden::ConsumedAt).timestamp_with_time_zone())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE email_codes ADD CONSTRAINT fk_ec_user_id FOREIGN KEY (user_id) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_email_codes_user_purpose", "email_codes", &["user_id", "purpose"], false, None).await?;

        let sql = Table::create()
            .table(UserDeviceKeyIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(UserDeviceKeyIden::DeviceId).string().not_null().primary_key())
            .col(ColumnDef::new(UserDeviceKeyIden::UserId).big_integer().not_null())
            .col(ColumnDef::new(UserDeviceKeyIden::PublicJwk).string().not_null())
            .col(ColumnDef::new(UserDeviceKeyIden::Label).string())
            .col(ColumnDef::new(UserDeviceKeyIden::CreatedAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(UserDeviceKeyIden::LastSeen).timestamp_with_time_zone())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE user_device_keys ADD CONSTRAINT fk_udk_user_id FOREIGN KEY (user_id) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_user_device_keys_user_id", "user_device_keys", &["user_id"], false, None).await?;

        let sql = Table::create()
            .table(E2eeKeyPinIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(E2eeKeyPinIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(E2eeKeyPinIden::UserId).big_integer().not_null())
            .col(ColumnDef::new(E2eeKeyPinIden::DeviceId).string().not_null())
            .col(ColumnDef::new(E2eeKeyPinIden::Fingerprint).string().not_null())
            .col(ColumnDef::new(E2eeKeyPinIden::CreatedAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(E2eeKeyPinIden::LastVerifiedAt).timestamp_with_time_zone().not_null())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE e2ee_key_pins ADD CONSTRAINT uq_e2ee_key_pins UNIQUE(user_id, device_id);").await?;
        exec_sql(db, "ALTER TABLE e2ee_key_pins ADD CONSTRAINT fk_ekp_user_id FOREIGN KEY (user_id) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_e2ee_key_pins_user_id", "e2ee_key_pins", &["user_id"], false, None).await?;

        let sql = Table::create()
            .table(RateLimitLogIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(RateLimitLogIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(RateLimitLogIden::Key).string().not_null())
            .col(ColumnDef::new(RateLimitLogIden::Timestamp).big_integer().not_null())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        create_index_if_not_exists(db, "ix_rate_limit_logs_key_ts", "rate_limit_logs", &["key", "timestamp"], false, None).await?;

        let sql = Table::create()
            .table(CsrfTokenIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(CsrfTokenIden::TokenHash).string().not_null().primary_key())
            .col(ColumnDef::new(CsrfTokenIden::UserId).big_integer().not_null())
            .col(ColumnDef::new(CsrfTokenIden::CreatedAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(CsrfTokenIden::ExpiresAt).timestamp_with_time_zone().not_null())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE csrf_tokens ADD CONSTRAINT fk_ct_user_id FOREIGN KEY (user_id) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_csrf_tokens_user_id", "csrf_tokens", &["user_id"], false, None).await?;

        let sql = Table::create()
            .table(UserReportIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(UserReportIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(UserReportIden::ReporterId).big_integer().not_null())
            .col(ColumnDef::new(UserReportIden::TargetUserId).big_integer().not_null())
            .col(ColumnDef::new(UserReportIden::MessageId).big_integer())
            .col(ColumnDef::new(UserReportIden::Reason).string().not_null())
            .col(ColumnDef::new(UserReportIden::Message).string().not_null().default(""))
            .col(ColumnDef::new(UserReportIden::Status).string().not_null().default("open"))
            .col(ColumnDef::new(UserReportIden::CreatedAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(UserReportIden::ResolvedAt).timestamp_with_time_zone())
            .col(ColumnDef::new(UserReportIden::ResolvedBy).big_integer())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE user_reports ADD CONSTRAINT fk_ur_reporter_id FOREIGN KEY (reporter_id) REFERENCES users(id);").await?;
        exec_sql(db, "ALTER TABLE user_reports ADD CONSTRAINT fk_ur_target_user_id FOREIGN KEY (target_user_id) REFERENCES users(id);").await?;
        exec_sql(db, "ALTER TABLE user_reports ADD CONSTRAINT fk_ur_message_id FOREIGN KEY (message_id) REFERENCES messages(id);").await?;
        exec_sql(db, "ALTER TABLE user_reports ADD CONSTRAINT fk_ur_resolved_by FOREIGN KEY (resolved_by) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_user_reports_target_status", "user_reports", &["target_user_id", "status"], false, None).await?;
        create_index_if_not_exists(db, "ix_user_reports_reporter", "user_reports", &["reporter_id"], false, None).await?;

        let sql = Table::create()
            .table(UserSuggestionIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(UserSuggestionIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(UserSuggestionIden::UserId).big_integer().not_null())
            .col(ColumnDef::new(UserSuggestionIden::Title).string().not_null().default(""))
            .col(ColumnDef::new(UserSuggestionIden::Message).string().not_null())
            .col(ColumnDef::new(UserSuggestionIden::Status).string().not_null().default("open"))
            .col(ColumnDef::new(UserSuggestionIden::CreatedAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(UserSuggestionIden::ReviewedAt).timestamp_with_time_zone())
            .col(ColumnDef::new(UserSuggestionIden::ReviewedBy).big_integer())
            .col(ColumnDef::new(UserSuggestionIden::AdminNote).string().not_null().default(""))
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE user_suggestions ADD CONSTRAINT fk_us_user_id FOREIGN KEY (user_id) REFERENCES users(id);").await?;
        exec_sql(db, "ALTER TABLE user_suggestions ADD CONSTRAINT fk_us_reviewed_by FOREIGN KEY (reviewed_by) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_user_suggestions_status_created", "user_suggestions", &["status", "created_at"], false, None).await?;
        create_index_if_not_exists(db, "ix_user_suggestions_user", "user_suggestions", &["user_id", "created_at"], false, None).await?;

        let sql = Table::create()
            .table(ModerationEventIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(ModerationEventIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(ModerationEventIden::UserId).big_integer().not_null())
            .col(ColumnDef::new(ModerationEventIden::AdminId).big_integer())
            .col(ColumnDef::new(ModerationEventIden::Kind).string().not_null())
            .col(ColumnDef::new(ModerationEventIden::Reason).string().not_null().default(""))
            .col(ColumnDef::new(ModerationEventIden::Details).string().not_null().default(""))
            .col(ColumnDef::new(ModerationEventIden::CreatedAt).timestamp_with_time_zone().not_null())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE moderation_events ADD CONSTRAINT fk_me_user_id FOREIGN KEY (user_id) REFERENCES users(id);").await?;
        exec_sql(db, "ALTER TABLE moderation_events ADD CONSTRAINT fk_me_admin_id FOREIGN KEY (admin_id) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_moderation_events_user_kind", "moderation_events", &["user_id", "kind", "id"], false, None).await?;

        let sql = Table::create()
            .table(TwoFactorBackupCodeIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(TwoFactorBackupCodeIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(TwoFactorBackupCodeIden::UserId).big_integer().not_null())
            .col(ColumnDef::new(TwoFactorBackupCodeIden::CodeHash).string().not_null())
            .col(ColumnDef::new(TwoFactorBackupCodeIden::IsUsed).boolean().not_null().default(false))
            .col(ColumnDef::new(TwoFactorBackupCodeIden::UsedAt).timestamp_with_time_zone())
            .col(ColumnDef::new(TwoFactorBackupCodeIden::CreatedAt).timestamp_with_time_zone().not_null())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE two_factor_backup_codes ADD CONSTRAINT fk_tfbc_user_id FOREIGN KEY (user_id) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_two_factor_backup_codes_user_id", "two_factor_backup_codes", &["user_id"], false, None).await?;
    });

    migration!(db, applied, 3, "Extended sessions and audit", {
        let sql = Table::create()
            .table(AuditLogIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(AuditLogIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(AuditLogIden::UserId).big_integer())
            .col(ColumnDef::new(AuditLogIden::Action).string().not_null())
            .col(ColumnDef::new(AuditLogIden::ResourceType).string())
            .col(ColumnDef::new(AuditLogIden::ResourceId).big_integer())
            .col(ColumnDef::new(AuditLogIden::Status).string().not_null().default("success"))
            .col(ColumnDef::new(AuditLogIden::Details).string())
            .col(ColumnDef::new(AuditLogIden::IpAddress).string())
            .col(ColumnDef::new(AuditLogIden::UserAgent).string())
            .col(ColumnDef::new(AuditLogIden::CreatedAt).timestamp_with_time_zone().not_null())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE audit_logs ADD CONSTRAINT fk_al_user_id FOREIGN KEY (user_id) REFERENCES users(id);").await?;
        create_index_if_not_exists(db, "ix_audit_logs_user_id", "audit_logs", &["user_id"], false, None).await?;
        create_index_if_not_exists(db, "ix_audit_logs_created_at", "audit_logs", &["created_at DESC"], false, None).await?;

        let sql = Table::create()
            .table(GifAssetIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(GifAssetIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(GifAssetIden::Scope).string().not_null())
            .col(ColumnDef::new(GifAssetIden::OwnerId).big_integer())
            .col(ColumnDef::new(GifAssetIden::SourceFileId).big_integer())
            .col(ColumnDef::new(GifAssetIden::Filename).string().not_null())
            .col(ColumnDef::new(GifAssetIden::OriginalName).string().not_null())
            .col(ColumnDef::new(GifAssetIden::FileSize).big_integer().not_null())
            .col(ColumnDef::new(GifAssetIden::MimeType).string().not_null().default("image/gif"))
            .col(ColumnDef::new(GifAssetIden::StoragePath).string().not_null())
            .col(ColumnDef::new(GifAssetIden::CreatedByAdmin).boolean().not_null().default(false))
            .col(ColumnDef::new(GifAssetIden::CreatedAt).timestamp_with_time_zone().not_null())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE gif_assets ADD CONSTRAINT fk_ga_owner_id FOREIGN KEY (owner_id) REFERENCES users(id);").await?;
        exec_sql(db, "ALTER TABLE gif_assets ADD CONSTRAINT fk_ga_source_file_id FOREIGN KEY (source_file_id) REFERENCES files(id);").await?;
        create_index_if_not_exists(db, "ix_gif_assets_scope", "gif_assets", &["scope", "id"], false, None).await?;
        create_index_if_not_exists(db, "ix_gif_assets_owner", "gif_assets", &["owner_id", "id"], false, None).await?;
        create_index_if_not_exists(db, "ix_gif_assets_storage_path", "gif_assets", &["storage_path"], false, None).await?;

        let sql = Table::create()
            .table(AppDownloadIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(AppDownloadIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(AppDownloadIden::Platform).string().not_null())
            .col(ColumnDef::new(AppDownloadIden::Version).string().not_null().default(""))
            .col(ColumnDef::new(AppDownloadIden::OriginalName).string().not_null())
            .col(ColumnDef::new(AppDownloadIden::MimeType).string().not_null())
            .col(ColumnDef::new(AppDownloadIden::FileSize).big_integer().not_null())
            .col(ColumnDef::new(AppDownloadIden::StoragePath).string().not_null())
            .col(ColumnDef::new(AppDownloadIden::UploadedAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(AppDownloadIden::IsActive).boolean().not_null().default(true))
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        create_index_if_not_exists(db, "ix_app_downloads_platform_active", "app_downloads", &["platform", "is_active", "id"], false, None).await?;
    });

    migration!(db, applied, 4, "AI settings and chat state", {
        let sql = Table::create()
            .table(AiSettingIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(AiSettingIden::Id).big_integer().not_null().primary_key())
            .col(ColumnDef::new(AiSettingIden::Enabled).boolean().not_null().default(false))
            .col(ColumnDef::new(AiSettingIden::BaseUrl).string().not_null().default("http://127.0.0.1:1234/v1"))
            .col(ColumnDef::new(AiSettingIden::Model).string().not_null().default("qwen_qwen3-4b-instruct-2507"))
            .col(ColumnDef::new(AiSettingIden::UserName).string().not_null().default("Gemka III"))
            .col(ColumnDef::new(AiSettingIden::Label).string().not_null().default("Тестовая функция"))
            .col(ColumnDef::new(AiSettingIden::Mode).string().not_null().default("moderate"))
            .col(ColumnDef::new(AiSettingIden::DmEnabled).boolean().not_null().default(true))
            .col(ColumnDef::new(AiSettingIden::ChannelEnabled).boolean().not_null().default(false))
            .col(ColumnDef::new(AiSettingIden::AcceptFriendRequests).boolean().not_null().default(true))
            .col(ColumnDef::new(AiSettingIden::AcceptServerJoinRequests).boolean().not_null().default(false))
            .col(ColumnDef::new(AiSettingIden::StartDmEnabled).boolean().not_null().default(false))
            .col(ColumnDef::new(AiSettingIden::DmCooldownSeconds).integer().not_null().default(20))
            .col(ColumnDef::new(AiSettingIden::ChannelCooldownSeconds).integer().not_null().default(90))
            .col(ColumnDef::new(AiSettingIden::ContextMessages).integer().not_null().default(40))
            .col(ColumnDef::new(AiSettingIden::MaxTokens).integer().not_null().default(180))
            .col(ColumnDef::new(AiSettingIden::Temperature).float().not_null().default(0.35))
            .col(ColumnDef::new(AiSettingIden::TopP).float().not_null().default(0.75))
            .col(ColumnDef::new(AiSettingIden::SystemPrompt).string().not_null().default(""))
            .col(ColumnDef::new(AiSettingIden::UpdatedAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(AiSettingIden::KindnessScore).integer().not_null().default(100))
            .col(ColumnDef::new(AiSettingIden::NoReplyCount).integer().not_null().default(0))
            .col(ColumnDef::new(AiSettingIden::ViolationCount).integer().not_null().default(0))
            .col(ColumnDef::new(AiSettingIden::LastEventAt).timestamp_with_time_zone())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE ai_settings ADD CONSTRAINT chk_ai_settings_id CHECK (id = 1);").await?;

        let sql = Table::create()
            .table(AiChatStateIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(AiChatStateIden::ChatId).big_integer().not_null().primary_key())
            .col(ColumnDef::new(AiChatStateIden::LastReplyAt).timestamp_with_time_zone())
            .col(ColumnDef::new(AiChatStateIden::LastSeenMessageId).big_integer().not_null().default(0))
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE ai_chat_state ADD CONSTRAINT fk_acs_chat_id FOREIGN KEY (chat_id) REFERENCES chats(id);").await?;
    });

    migration!(db, applied, 5, "Column additions and indexes", {
        add_column_if_not_exists(db, "users", "is_ai BOOLEAN NOT NULL DEFAULT false", "is_ai").await?;
        add_column_if_not_exists(db, "users", "ai_label TEXT", "ai_label").await?;
        add_column_if_not_exists(db, "users", "email_verified BOOLEAN NOT NULL DEFAULT false", "email_verified").await?;
        add_column_if_not_exists(db, "users", "email_pending TEXT", "email_pending").await?;
        add_column_if_not_exists(db, "users", "public_encryption_key TEXT", "public_encryption_key").await?;
        add_column_if_not_exists(db, "users", "terms_accepted_at TIMESTAMPTZ", "terms_accepted_at").await?;
        add_column_if_not_exists(db, "users", "terms_agreement_version TEXT", "terms_agreement_version").await?;
        add_column_if_not_exists(db, "users", "cookie_consent_status TEXT NOT NULL DEFAULT 'unknown'", "cookie_consent_status").await?;
        add_column_if_not_exists(db, "users", "cookie_consent_at TIMESTAMPTZ", "cookie_consent_at").await?;
        add_column_if_not_exists(db, "users", "trust_factor INTEGER NOT NULL DEFAULT 100", "trust_factor").await?;
        add_column_if_not_exists(db, "users", "trust_review_status TEXT NOT NULL DEFAULT 'clear'", "trust_review_status").await?;
        add_column_if_not_exists(db, "users", "trust_review_reason TEXT", "trust_review_reason").await?;
        add_column_if_not_exists(db, "users", "trust_review_at TIMESTAMPTZ", "trust_review_at").await?;
        add_column_if_not_exists(db, "users", "two_factor_code_expires_at TIMESTAMPTZ", "two_factor_code_expires_at").await?;
        add_column_if_not_exists(db, "users", "two_factor_code_attempts INTEGER NOT NULL DEFAULT 0", "two_factor_code_attempts").await?;
        add_column_if_not_exists(db, "users", "two_factor_locked_until TIMESTAMPTZ", "two_factor_locked_until").await?;

        add_column_if_not_exists(db, "servers", "is_public BOOLEAN NOT NULL DEFAULT true", "is_public").await?;
        add_column_if_not_exists(db, "messages", "reply_to_message_id BIGINT", "reply_to_message_id").await?;
        add_column_if_not_exists(db, "messages", "edited_at TIMESTAMPTZ", "edited_at").await?;
        add_column_if_not_exists(db, "friendships", "is_favorite BOOLEAN NOT NULL DEFAULT false", "is_favorite").await?;

        add_column_if_not_exists(db, "files", "content_hash TEXT", "content_hash").await?;
        add_column_if_not_exists(db, "files", "normalized_hash TEXT", "normalized_hash").await?;
        add_column_if_not_exists(db, "files", "content_hash_algo TEXT", "content_hash_algo").await?;
        add_column_if_not_exists(db, "files", "storage_kind TEXT NOT NULL DEFAULT 'temporary'", "storage_kind").await?;
        add_column_if_not_exists(db, "files", "expires_at TIMESTAMPTZ", "expires_at").await?;
        add_column_if_not_exists(db, "files", "deleted_at TIMESTAMPTZ", "deleted_at").await?;

        add_column_if_not_exists(db, "ai_settings", "accept_server_join_requests BOOLEAN NOT NULL DEFAULT false", "accept_server_join_requests").await?;
        add_column_if_not_exists(db, "ai_settings", "kindness_score INTEGER NOT NULL DEFAULT 100", "kindness_score").await?;
        add_column_if_not_exists(db, "ai_settings", "no_reply_count INTEGER NOT NULL DEFAULT 0", "no_reply_count").await?;
        add_column_if_not_exists(db, "ai_settings", "violation_count INTEGER NOT NULL DEFAULT 0", "violation_count").await?;
        add_column_if_not_exists(db, "ai_settings", "last_event_at TIMESTAMPTZ", "last_event_at").await?;

        create_index_if_not_exists(db, "ix_user_device_keys_user_id", "user_device_keys", &["user_id"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_user_sessions_user_id", "user_sessions", &["user_id"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_refresh_sessions_user_id", "refresh_sessions", &["user_id"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_email_codes_user_purpose", "email_codes", &["user_id", "purpose"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_e2ee_key_pins_user_id", "e2ee_key_pins", &["user_id"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_two_factor_backup_codes_user_id", "two_factor_backup_codes", &["user_id"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_rate_limit_logs_key_ts", "rate_limit_logs", &["key", "timestamp"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_csrf_tokens_user_id", "csrf_tokens", &["user_id"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_audit_logs_user_id", "audit_logs", &["user_id"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_audit_logs_created_at", "audit_logs", &["created_at DESC"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_user_reports_target_status", "user_reports", &["target_user_id", "status"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_user_reports_reporter", "user_reports", &["reporter_id"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_user_suggestions_status_created", "user_suggestions", &["status", "created_at"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_user_suggestions_user", "user_suggestions", &["user_id", "created_at"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_moderation_events_user_kind", "moderation_events", &["user_id", "kind", "id"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_files_content_hash", "files", &["content_hash"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_files_normalized_hash", "files", &["normalized_hash"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_files_expires_at", "files", &["expires_at"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_files_storage_path", "files", &["storage_path"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_files_deleted_at", "files", &["deleted_at"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_gif_assets_scope", "gif_assets", &["scope", "id"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_gif_assets_owner", "gif_assets", &["owner_id", "id"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_gif_assets_storage_path", "gif_assets", &["storage_path"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_app_downloads_platform_active", "app_downloads", &["platform", "is_active", "id"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_messages_sender_id", "messages", &["sender_id"], false, None).await.ok();
        create_index_if_not_exists(db, "ix_messages_created_at", "messages", &["timestamp DESC"], false, None).await.ok();
    });

    migration!(db, applied, 6, "Friend request dedup", {
        exec_sql(db, "DELETE FROM friend_requests WHERE id NOT IN (SELECT MIN(id) FROM friend_requests GROUP BY sender_id, receiver_id, status);").await.ok();
        create_index_if_not_exists(db, "ux_friend_requests_pending_pair", "friend_requests", &["sender_id", "receiver_id"], true, Some("status = 'pending'")).await.ok();
        create_index_if_not_exists(db, "ux_gif_favorites_owner_storage", "gif_assets", &["owner_id", "storage_path"], true, Some("scope = 'favorite' AND owner_id IS NOT NULL")).await.ok();
        exec_sql(db, "UPDATE user_presence SET is_online = false;").await.ok();
    });

    {
        let ai_enabled_env = std::env::var("LB_AI_ENABLED")
            .ok()
            .map(|v| {
                let v = v.trim().to_ascii_lowercase();
                v == "1" || v == "true" || v == "yes" || v == "on"
            })
            .unwrap_or(false);
        if ai_enabled_env && !applied.contains(&7) {
            tracing::info!("[DB] Migration 7: AI user setup");
            let ai_name = std::env::var("LB_AI_USER_NAME")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "Gemka III".to_string());
            let ai_label = "Тестовая функция".to_string();
            let now = Utc::now();
            let _ = sqlx::query(
                "INSERT INTO users (username, email, password_hash, is_banned, created_at, token_version, is_ai, ai_label)
                 VALUES ($1, NULL, 'AI_LOGIN_DISABLED', false, $2, 1, true, $3)
                 ON CONFLICT(username) DO UPDATE SET is_ai = true, ai_label = EXCLUDED.ai_label, is_banned = false"
            )
            .bind(&ai_name)
            .bind(now)
            .bind(&ai_label)
            .execute(db)
            .await;

            let _ = sqlx::query("UPDATE users SET is_ai = true, ai_label = $1, is_banned = false WHERE username = $2")
                .bind(&ai_label)
                .bind(&ai_name)
                .execute(db)
                .await;
            mark_applied(db, 7).await?;
        }
    }

    migration!(db, applied, 8, "E2EE Room Keys Backup", {
        let sql = Table::create()
            .table(E2eeRoomKeyIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(E2eeRoomKeyIden::UserId).big_integer().not_null())
            .col(ColumnDef::new(E2eeRoomKeyIden::ChatId).big_integer().not_null())
            .col(ColumnDef::new(E2eeRoomKeyIden::EncryptedKey).string().not_null())
            .col(ColumnDef::new(E2eeRoomKeyIden::Nonce).string().not_null())
            .col(ColumnDef::new(E2eeRoomKeyIden::CreatedAt).timestamp_with_time_zone().not_null())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE e2ee_room_keys ADD CONSTRAINT uq_e2ee_room_keys UNIQUE(user_id, chat_id);").await?;
        exec_sql(db, "ALTER TABLE e2ee_room_keys ADD CONSTRAINT fk_erk_user_id FOREIGN KEY (user_id) REFERENCES users(id);").await?;
        exec_sql(db, "ALTER TABLE e2ee_room_keys ADD CONSTRAINT fk_erk_chat_id FOREIGN KEY (chat_id) REFERENCES chats(id);").await?;
    });

    migration!(db, applied, 9, "E2EE Master Key Backup", {
        let sql = Table::create()
            .table(UserKeyBackupIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(UserKeyBackupIden::UserId).big_integer().not_null().primary_key())
            .col(ColumnDef::new(UserKeyBackupIden::BlobPassword).string())
            .col(ColumnDef::new(UserKeyBackupIden::SaltPassword).string())
            .col(ColumnDef::new(UserKeyBackupIden::BlobEmail).string())
            .col(ColumnDef::new(UserKeyBackupIden::SaltEmail).string())
            .col(ColumnDef::new(UserKeyBackupIden::UpdatedAt).timestamp_with_time_zone().not_null())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
        exec_sql(db, "ALTER TABLE user_key_backups ADD CONSTRAINT fk_ukb_user_id FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE;").await?;
    });

    migration!(db, applied, 10, "Payment orders", {
        let sql = Table::create()
            .table(PaymentOrderIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(PaymentOrderIden::Id).string().not_null().primary_key())
            .col(ColumnDef::new(PaymentOrderIden::UserId).big_integer().not_null())
            .col(ColumnDef::new(PaymentOrderIden::PlanId).string().not_null())
            .col(ColumnDef::new(PaymentOrderIden::Amount).integer().not_null())
            .col(ColumnDef::new(PaymentOrderIden::Status).string().not_null().default("pending"))
            .col(ColumnDef::new(PaymentOrderIden::CreatedAt).timestamp_with_time_zone().not_null().default("NOW()"))
            .col(ColumnDef::new(PaymentOrderIden::PaidAt).timestamp_with_time_zone())
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
    });

    migration!(db, applied, 11, "Subscriptions", {
        let sql = Table::create()
            .table(SubscriptionIden::Table)
            .if_not_exists()
            .col(ColumnDef::new(SubscriptionIden::Id).big_integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(SubscriptionIden::UserId).big_integer().not_null())
            .col(ColumnDef::new(SubscriptionIden::PlanId).string().not_null())
            .col(ColumnDef::new(SubscriptionIden::ExpiresAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(SubscriptionIden::CreatedAt).timestamp_with_time_zone().not_null().default("NOW()"))
            .to_string(PostgresQueryBuilder);
        exec_sql(db, &sql).await?;
    });

    Ok(())
}