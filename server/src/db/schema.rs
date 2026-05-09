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


    // default AI user from env, if enabled
    let ai_enabled_env = std::env::var("LB_AI_ENABLED")
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(false);
    if ai_enabled_env {
        let ai_name = std::env::var("LB_AI_USER_NAME")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "Gemka III".to_string());
        let ai_label = "Тестовая функция".to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let _ = sqlx::query(
            r#"
            INSERT OR IGNORE INTO users(username, email, password_hash, is_banned, created_at, token_version, is_ai, ai_label)
            VALUES(?, NULL, 'AI_LOGIN_DISABLED', 0, ?, 1, 1, ?)
            "#,
        )
        .bind(&ai_name)
        .bind(&now)
        .bind(&ai_label)
        .execute(db)
        .await;
        let _ = sqlx::query("UPDATE users SET is_ai = 1, ai_label = ?, is_banned = 0 WHERE username = ?")
            .bind(&ai_label)
            .bind(&ai_name)
            .execute(db)
            .await;
    }

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

    // AI marker columns
    try_add_column(db, "users", "is_ai INTEGER NOT NULL DEFAULT 0", "is_ai").await?;
    try_add_column(db, "users", "ai_label TEXT", "ai_label").await?;

    // servers
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS servers (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            owner_id INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            is_public INTEGER NOT NULL DEFAULT 1,
            FOREIGN KEY(owner_id) REFERENCES users(id)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_server_owner_id ON servers(owner_id);"#)
        .execute(db)
        .await?;

    try_add_column(db, "servers", "is_public INTEGER NOT NULL DEFAULT 1", "is_public")
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


    // server_join_requests
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS server_join_requests (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            server_id INTEGER NOT NULL,
            requester_id INTEGER NOT NULL,
            from_server_id INTEGER,
            status TEXT NOT NULL DEFAULT 'pending',
            created_at TEXT NOT NULL,
            decided_at TEXT,
            decided_by INTEGER,
            FOREIGN KEY(server_id) REFERENCES servers(id),
            FOREIGN KEY(requester_id) REFERENCES users(id),
            FOREIGN KEY(from_server_id) REFERENCES servers(id),
            FOREIGN KEY(decided_by) REFERENCES users(id),
            UNIQUE(server_id, requester_id)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_server_join_requests_server_status ON server_join_requests(server_id, status);"#)
        .execute(db)
        .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_server_join_requests_requester ON server_join_requests(requester_id);"#)
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
            content_hash TEXT,
            normalized_hash TEXT,
            content_hash_algo TEXT,
            storage_kind TEXT NOT NULL DEFAULT 'temporary',
            expires_at TEXT,
            deleted_at TEXT,
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

    try_add_column(db, "files", "content_hash TEXT", "content_hash").await?;
    try_add_column(db, "files", "normalized_hash TEXT", "normalized_hash").await?;
    try_add_column(db, "files", "content_hash_algo TEXT", "content_hash_algo").await?;
    try_add_column(db, "files", "storage_kind TEXT NOT NULL DEFAULT 'temporary'", "storage_kind").await?;
    try_add_column(db, "files", "expires_at TEXT", "expires_at").await?;
    try_add_column(db, "files", "deleted_at TEXT", "deleted_at").await?;

    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_files_content_hash ON files(content_hash);"#)
        .execute(db)
        .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_files_normalized_hash ON files(normalized_hash);"#)
        .execute(db)
        .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_files_expires_at ON files(expires_at);"#)
        .execute(db)
        .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_files_storage_path ON files(storage_path);"#)
        .execute(db)
        .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_files_deleted_at ON files(deleted_at);"#)
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



    // user_reports: жалобы пользователей на аккаунты / аватары / ник / поведение
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS user_reports (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            reporter_id INTEGER NOT NULL,
            target_user_id INTEGER NOT NULL,
            message_id INTEGER,
            reason TEXT NOT NULL,
            message TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'open',
            created_at TEXT NOT NULL,
            resolved_at TEXT,
            resolved_by INTEGER,
            FOREIGN KEY(reporter_id) REFERENCES users(id),
            FOREIGN KEY(target_user_id) REFERENCES users(id),
            FOREIGN KEY(message_id) REFERENCES messages(id),
            FOREIGN KEY(resolved_by) REFERENCES users(id)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_user_reports_target_status ON user_reports(target_user_id, status);"#)
        .execute(db)
        .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_user_reports_reporter ON user_reports(reporter_id);"#)
        .execute(db)
        .await?;

    // moderation_events: история действий модерации по пользователю
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS moderation_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id INTEGER NOT NULL,
            admin_id INTEGER,
            kind TEXT NOT NULL,
            reason TEXT NOT NULL DEFAULT '',
            details TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL,
            FOREIGN KEY(user_id) REFERENCES users(id),
            FOREIGN KEY(admin_id) REFERENCES users(id)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_moderation_events_user_kind ON moderation_events(user_id, kind, id);"#)
        .execute(db)
        .await?;


    // ai_settings: настройки Gemka III / LM Studio
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS ai_settings (
            id INTEGER PRIMARY KEY CHECK(id = 1),
            enabled INTEGER NOT NULL DEFAULT 0,
            base_url TEXT NOT NULL DEFAULT 'http://127.0.0.1:1234/v1',
            model TEXT NOT NULL DEFAULT 'qwen_qwen3-4b-instruct-2507',
            user_name TEXT NOT NULL DEFAULT 'Gemka III',
            label TEXT NOT NULL DEFAULT 'Тестовая функция',
            mode TEXT NOT NULL DEFAULT 'moderate',
            dm_enabled INTEGER NOT NULL DEFAULT 1,
            channel_enabled INTEGER NOT NULL DEFAULT 0,
            accept_friend_requests INTEGER NOT NULL DEFAULT 1,
            accept_server_join_requests INTEGER NOT NULL DEFAULT 0,
            start_dm_enabled INTEGER NOT NULL DEFAULT 0,
            dm_cooldown_seconds INTEGER NOT NULL DEFAULT 20,
            channel_cooldown_seconds INTEGER NOT NULL DEFAULT 90,
            context_messages INTEGER NOT NULL DEFAULT 40,
            max_tokens INTEGER NOT NULL DEFAULT 180,
            temperature REAL NOT NULL DEFAULT 0.35,
            top_p REAL NOT NULL DEFAULT 0.75,
            system_prompt TEXT NOT NULL DEFAULT '',
            updated_at TEXT NOT NULL
        );
        "#,
    )
    .execute(db)
    .await?;

    try_add_column(
        db,
        "ai_settings",
        "accept_server_join_requests INTEGER NOT NULL DEFAULT 0",
        "accept_server_join_requests",
    )
    .await?;
    try_add_column(
        db,
        "ai_settings",
        "kindness_score INTEGER NOT NULL DEFAULT 100",
        "kindness_score",
    )
    .await?;
    try_add_column(
        db,
        "ai_settings",
        "no_reply_count INTEGER NOT NULL DEFAULT 0",
        "no_reply_count",
    )
    .await?;
    try_add_column(
        db,
        "ai_settings",
        "violation_count INTEGER NOT NULL DEFAULT 0",
        "violation_count",
    )
    .await?;
    try_add_column(
        db,
        "ai_settings",
        "last_event_at TEXT",
        "last_event_at",
    )
    .await?;

    // ai_chat_state: антиспам/cooldown по чатам
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS ai_chat_state (
            chat_id INTEGER PRIMARY KEY,
            last_reply_at TEXT,
            last_seen_message_id INTEGER NOT NULL DEFAULT 0,
            FOREIGN KEY(chat_id) REFERENCES chats(id)
        );
        "#,
    )
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


    // default AI user from env, if enabled
    let ai_enabled_env = std::env::var("LB_AI_ENABLED")
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(false);
    if ai_enabled_env {
        let ai_name = std::env::var("LB_AI_USER_NAME")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "Gemka III".to_string());
        let ai_label = "Тестовая функция".to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let _ = sqlx::query(
            r#"
            INSERT OR IGNORE INTO users(username, email, password_hash, is_banned, created_at, token_version, is_ai, ai_label)
            VALUES(?, NULL, 'AI_LOGIN_DISABLED', 0, ?, 1, 1, ?)
            "#,
        )
        .bind(&ai_name)
        .bind(&now)
        .bind(&ai_label)
        .execute(db)
        .await;
        let _ = sqlx::query("UPDATE users SET is_ai = 1, ai_label = ?, is_banned = 0 WHERE username = ?")
            .bind(&ai_label)
            .bind(&ai_name)
            .execute(db)
            .await;
    }

    Ok(())
}
