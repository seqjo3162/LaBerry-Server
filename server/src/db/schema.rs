use sqlx::{Row, SqlitePool};

async fn table_has_column(db: &SqlitePool, table: &str, column: &str) -> anyhow::Result<bool> {
    let rows = sqlx::query(&format!("PRAGMA table_info({})", table))
        .fetch_all(db)
        .await?;
    Ok(rows
        .into_iter()
        .any(|r| r.get::<String, _>("name") == column))
}

async fn try_add_column(
    db: &SqlitePool,
    table: &str,
    column_def: &str,
    column_name: &str,
) -> anyhow::Result<()> {
    if table_has_column(db, table, column_name).await? {
        return Ok(());
    }

    sqlx::query(&format!("ALTER TABLE {} ADD COLUMN {}", table, column_def))
        .execute(db)
        .await?;

    // migrate: chats.kind (text/voice)
    let _ = sqlx::query("ALTER TABLE chats ADD COLUMN kind TEXT NOT NULL DEFAULT 'text'")
        .execute(db)
        .await;

    Ok(())
}

pub async fn init(db: &SqlitePool) -> anyhow::Result<()> {
    // perf + надёжность
    sqlx::query("PRAGMA journal_mode=WAL;").execute(db).await?;
    sqlx::query("PRAGMA synchronous=NORMAL;").execute(db).await?;
    sqlx::query("PRAGMA foreign_keys=ON;").execute(db).await?;

    // users
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL UNIQUE,
            email TEXT UNIQUE,
            email_verified INTEGER NOT NULL DEFAULT 0,
            email_pending TEXT,
            password_hash TEXT NOT NULL,
            is_banned INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            token_version INTEGER NOT NULL DEFAULT 1,
            is_2fa_enabled INTEGER NOT NULL DEFAULT 0,
            two_factor_secret_code_hash TEXT,
            two_factor_code_sent_at TEXT,
            public_encryption_key TEXT
        );
        "#,
    )
    .execute(db)
    .await?;

    // migrate legacy dbs
    try_add_column(db, "users", "email_verified INTEGER NOT NULL DEFAULT 0", "email_verified")
        .await?;
    try_add_column(db, "users", "email_pending TEXT", "email_pending").await?;

    // servers
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS servers (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            owner_id INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            FOREIGN KEY(owner_id) REFERENCES users(id)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_server_owner_id ON servers(owner_id);"#)
        .execute(db)
        .await?;

    // server_members
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS server_members (
            server_id INTEGER NOT NULL,
            user_id INTEGER NOT NULL,
            role TEXT NOT NULL DEFAULT 'member',
            FOREIGN KEY(server_id) REFERENCES servers(id),
            FOREIGN KEY(user_id) REFERENCES users(id),
            UNIQUE(server_id, user_id)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_server_members_user_id ON server_members(user_id);"#)
        .execute(db)
        .await?;

    // chats
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS chats (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT,
            server_id INTEGER,
            is_private INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            kind TEXT NOT NULL DEFAULT 'text',
            FOREIGN KEY(server_id) REFERENCES servers(id)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_chat_server_id ON chats(server_id);"#)
        .execute(db)
        .await?;

    // chat_participants
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS chat_participants (
            chat_id INTEGER NOT NULL,
            user_id INTEGER NOT NULL,
            FOREIGN KEY(chat_id) REFERENCES chats(id),
            FOREIGN KEY(user_id) REFERENCES users(id),
            UNIQUE(chat_id, user_id)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_chat_participants_user_id ON chat_participants(user_id);"#)
        .execute(db)
        .await?;

    // messages
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            chat_id INTEGER NOT NULL,
            sender_id INTEGER NOT NULL,
            content TEXT NOT NULL,
            timestamp TEXT NOT NULL,
            FOREIGN KEY(chat_id) REFERENCES chats(id),
            FOREIGN KEY(sender_id) REFERENCES users(id)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_messages_chat_id ON messages(chat_id);"#)
        .execute(db)
        .await?;

    // reply + edit columns
    try_add_column(db, "messages", "reply_to_message_id INTEGER", "reply_to_message_id").await?;
    try_add_column(db, "messages", "edited_at TEXT", "edited_at").await?;

    // files
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            filename TEXT NOT NULL,
            original_name TEXT NOT NULL,
            file_size INTEGER NOT NULL,
            mime_type TEXT NOT NULL,
            storage_path TEXT NOT NULL,
            uploaded_by INTEGER NOT NULL,
            chat_id INTEGER NOT NULL,
            message_id INTEGER,
            created_at TEXT NOT NULL,
            FOREIGN KEY(uploaded_by) REFERENCES users(id),
            FOREIGN KEY(chat_id) REFERENCES chats(id),
            FOREIGN KEY(message_id) REFERENCES messages(id)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_files_chat_id ON files(chat_id);"#)
        .execute(db)
        .await?;

    // friendships
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS friendships (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id INTEGER NOT NULL,
            friend_id INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            FOREIGN KEY(user_id) REFERENCES users(id),
            FOREIGN KEY(friend_id) REFERENCES users(id),
            UNIQUE(user_id, friend_id)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_friendships_user_id ON friendships(user_id);"#)
        .execute(db)
        .await?;

    // friend_requests
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS friend_requests (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            sender_id INTEGER NOT NULL,
            receiver_id INTEGER NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            created_at TEXT NOT NULL,
            FOREIGN KEY(sender_id) REFERENCES users(id),
            FOREIGN KEY(receiver_id) REFERENCES users(id)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_friend_requests_receiver_id ON friend_requests(receiver_id);"#)
        .execute(db)
        .await?;

    // cleanup duplicates + unique pending index
    sqlx::query(
        r#"
        DELETE FROM friend_requests
        WHERE id NOT IN (
            SELECT MIN(id)
            FROM friend_requests
            GROUP BY sender_id, receiver_id, status
        );
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        CREATE UNIQUE INDEX IF NOT EXISTS ux_friend_requests_pending_pair
        ON friend_requests(sender_id, receiver_id)
        WHERE status = 'pending';
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_friend_requests_sender_id ON friend_requests(sender_id);"#)
        .execute(db)
        .await?;

    // user_presence
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS user_presence (
            user_id INTEGER PRIMARY KEY,
            is_online INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'online',
            updated_at TEXT
        );
        "#,
    )
    .execute(db)
    .await?;

    // user_settings
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS user_settings (
            user_id INTEGER PRIMARY KEY,
            settings_json TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            FOREIGN KEY(user_id) REFERENCES users(id)
        );
        "#,
    )
    .execute(db)
    .await?;

    // reset presence on startup
    sqlx::query(r#"UPDATE user_presence SET is_online = 0;"#)
        .execute(db)
        .await?;

    // dm_chats
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS dm_chats (
            chat_id INTEGER NOT NULL,
            user_a INTEGER NOT NULL,
            user_b INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            FOREIGN KEY(chat_id) REFERENCES chats(id),
            FOREIGN KEY(user_a) REFERENCES users(id),
            FOREIGN KEY(user_b) REFERENCES users(id),
            UNIQUE(user_a, user_b)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_dm_chats_user_a ON dm_chats(user_a);"#)
        .execute(db)
        .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_dm_chats_user_b ON dm_chats(user_b);"#)
        .execute(db)
        .await?;

    // user_blocks
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS user_blocks (
            blocker_id INTEGER NOT NULL,
            blocked_id INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            FOREIGN KEY(blocker_id) REFERENCES users(id),
            FOREIGN KEY(blocked_id) REFERENCES users(id),
            UNIQUE(blocker_id, blocked_id)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_user_blocks_blocker ON user_blocks(blocker_id);"#)
        .execute(db)
        .await?;

    // message_reactions
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS message_reactions (
            message_id INTEGER NOT NULL,
            user_id INTEGER NOT NULL,
            emoji TEXT NOT NULL,
            created_at TEXT NOT NULL,
            FOREIGN KEY(message_id) REFERENCES messages(id),
            FOREIGN KEY(user_id) REFERENCES users(id),
            UNIQUE(message_id, user_id, emoji)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_message_reactions_message ON message_reactions(message_id);"#)
        .execute(db)
        .await?;

    // user_sessions
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS user_sessions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id INTEGER NOT NULL,
            token_hash TEXT NOT NULL,
            user_agent TEXT,
            ip TEXT,
            created_at TEXT NOT NULL,
            last_seen_at TEXT NOT NULL,
            revoked_at TEXT,
            FOREIGN KEY(user_id) REFERENCES users(id),
            UNIQUE(token_hash)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_user_sessions_user_id ON user_sessions(user_id);"#)
        .execute(db)
        .await?;


// refresh_sessions (long-lived refresh tokens, rotated)
sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS refresh_sessions (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        user_id INTEGER NOT NULL,
        refresh_token_hash TEXT NOT NULL,
        user_agent TEXT,
        ip TEXT,
        created_at TEXT NOT NULL,
        last_used_at TEXT NOT NULL,
        expires_at TEXT NOT NULL,
        revoked_at TEXT,
        FOREIGN KEY(user_id) REFERENCES users(id),
        UNIQUE(refresh_token_hash)
    );
    "#,
)
.execute(db)
.await?;
sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_refresh_sessions_user_id ON refresh_sessions(user_id);"#)
    .execute(db)
    .await?;


    // email_codes (stubs)
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS email_codes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id INTEGER NOT NULL,
            purpose TEXT NOT NULL,
            code_hash TEXT NOT NULL,
            sent_to_email TEXT,
            created_at TEXT NOT NULL,
            expires_at TEXT NOT NULL,
            consumed_at TEXT,
            FOREIGN KEY(user_id) REFERENCES users(id)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_email_codes_user_purpose ON email_codes(user_id, purpose);"#)
        .execute(db)
        .await?;

    // user_profile
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS user_profile (
            user_id INTEGER PRIMARY KEY,
            avatar_file_id INTEGER,
            banner_file_id INTEGER,
            accent_color TEXT,
            about TEXT,
            status_text TEXT,
            integrations_json TEXT NOT NULL DEFAULT '{}',
            updated_at TEXT NOT NULL,
            FOREIGN KEY(user_id) REFERENCES users(id)
        );
        "#,
    )
    .execute(db)
    .await?;


    

    // chat_reads (unread counters)
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS chat_reads (
            chat_id INTEGER NOT NULL,
            user_id INTEGER NOT NULL,
            last_read_message_id INTEGER NOT NULL DEFAULT 0,
            updated_at TEXT NOT NULL,
            FOREIGN KEY(chat_id) REFERENCES chats(id),
            FOREIGN KEY(user_id) REFERENCES users(id),
            UNIQUE(chat_id, user_id)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_chat_reads_user ON chat_reads(user_id);"#)
        .execute(db)
        .await?;

    // pinned_messages
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS pinned_messages (
            chat_id INTEGER NOT NULL,
            message_id INTEGER NOT NULL,
            pinned_by INTEGER NOT NULL,
            pinned_at TEXT NOT NULL,
            FOREIGN KEY(chat_id) REFERENCES chats(id),
            FOREIGN KEY(message_id) REFERENCES messages(id),
            FOREIGN KEY(pinned_by) REFERENCES users(id),
            UNIQUE(chat_id, message_id)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_pins_chat ON pinned_messages(chat_id);"#)
        .execute(db)
        .await?;

    // friendships: favorites
    try_add_column(db, "friendships", "is_favorite INTEGER NOT NULL DEFAULT 0", "is_favorite").await?;

    // profile_files (avatars/banners)
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS profile_files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            filename TEXT NOT NULL,
            original_name TEXT NOT NULL,
            file_size INTEGER NOT NULL,
            mime_type TEXT NOT NULL,
            storage_path TEXT NOT NULL,
            uploaded_by INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            FOREIGN KEY(uploaded_by) REFERENCES users(id)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_profile_files_uploader ON profile_files(uploaded_by);"#)
        .execute(db)
        .await?;

    // migrate: chats.kind (text/voice)
    let _ = sqlx::query("ALTER TABLE chats ADD COLUMN kind TEXT NOT NULL DEFAULT 'text'")
        .execute(db)
        .await;

    Ok(())
}
