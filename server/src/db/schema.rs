use sqlx::SqlitePool;

pub async fn init(db: &SqlitePool) -> anyhow::Result<()> {
    // perf + надёжность для телефона
    sqlx::query("PRAGMA journal_mode=WAL;").execute(db).await?;
    sqlx::query("PRAGMA synchronous=NORMAL;").execute(db).await?;
    sqlx::query("PRAGMA foreign_keys=ON;").execute(db).await?;

    // users
    sqlx::query(r#"
    CREATE TABLE IF NOT EXISTS users (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        username TEXT NOT NULL UNIQUE,
        email TEXT UNIQUE,
        password_hash TEXT NOT NULL,
        is_banned INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        token_version INTEGER NOT NULL DEFAULT 1,
        is_2fa_enabled INTEGER NOT NULL DEFAULT 0,
        two_factor_secret_code_hash TEXT,
        two_factor_code_sent_at TEXT,
        public_encryption_key TEXT
    );
    "#).execute(db).await?;

    // servers
    sqlx::query(r#"
    CREATE TABLE IF NOT EXISTS servers (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL,
        owner_id INTEGER NOT NULL,
        created_at TEXT NOT NULL,
        FOREIGN KEY(owner_id) REFERENCES users(id)
    );
    "#).execute(db).await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_server_owner_id ON servers(owner_id);"#)
        .execute(db).await?;

    // server_members (m2m)
    sqlx::query(r#"
    CREATE TABLE IF NOT EXISTS server_members (
        server_id INTEGER NOT NULL,
        user_id INTEGER NOT NULL,
        role TEXT NOT NULL DEFAULT 'member',
        FOREIGN KEY(server_id) REFERENCES servers(id),
        FOREIGN KEY(user_id) REFERENCES users(id),
        UNIQUE(server_id, user_id)
    );
    "#).execute(db).await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_server_members_user_id ON server_members(user_id);"#)
        .execute(db).await?;

    // chats
    sqlx::query(r#"
    CREATE TABLE IF NOT EXISTS chats (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT,
        server_id INTEGER,
        is_private INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        FOREIGN KEY(server_id) REFERENCES servers(id)
    );
    "#).execute(db).await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_chat_server_id ON chats(server_id);"#)
        .execute(db).await?;

    // chat_participants (m2m)
    sqlx::query(r#"
    CREATE TABLE IF NOT EXISTS chat_participants (
        chat_id INTEGER NOT NULL,
        user_id INTEGER NOT NULL,
        FOREIGN KEY(chat_id) REFERENCES chats(id),
        FOREIGN KEY(user_id) REFERENCES users(id),
        UNIQUE(chat_id, user_id)
    );
    "#).execute(db).await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_chat_participants_user_id ON chat_participants(user_id);"#)
        .execute(db).await?;

    // messages
    sqlx::query(r#"
    CREATE TABLE IF NOT EXISTS messages (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        chat_id INTEGER NOT NULL,
        sender_id INTEGER NOT NULL,
        content TEXT NOT NULL,
        timestamp TEXT NOT NULL,
        FOREIGN KEY(chat_id) REFERENCES chats(id),
        FOREIGN KEY(sender_id) REFERENCES users(id)
    );
    "#).execute(db).await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_messages_chat_id ON messages(chat_id);"#)
        .execute(db).await?;

    // files
    sqlx::query(r#"
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
    "#).execute(db).await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_files_chat_id ON files(chat_id);"#)
        .execute(db).await?;

    // friendships
    sqlx::query(r#"
    CREATE TABLE IF NOT EXISTS friendships (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        user_id INTEGER NOT NULL,
        friend_id INTEGER NOT NULL,
        created_at TEXT NOT NULL,
        FOREIGN KEY(user_id) REFERENCES users(id),
        FOREIGN KEY(friend_id) REFERENCES users(id),
        UNIQUE(user_id, friend_id)
    );
    "#).execute(db).await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_friendships_user_id ON friendships(user_id);"#)
        .execute(db).await?;

    // friend_requests
    sqlx::query(r#"
    CREATE TABLE IF NOT EXISTS friend_requests (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        sender_id INTEGER NOT NULL,
        receiver_id INTEGER NOT NULL,
        status TEXT NOT NULL DEFAULT 'pending',
        created_at TEXT NOT NULL,
        FOREIGN KEY(sender_id) REFERENCES users(id),
        FOREIGN KEY(receiver_id) REFERENCES users(id)
    );
    "#).execute(db).await?;
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS ix_friend_requests_receiver_id ON friend_requests(receiver_id);"#)
        .execute(db).await?;

    // user_presence
    sqlx::query(r#"
    CREATE TABLE IF NOT EXISTS user_presence (
        user_id INTEGER PRIMARY KEY,
        is_online INTEGER NOT NULL DEFAULT 0,
        FOREIGN KEY(user_id) REFERENCES users(id)
    );
    "#).execute(db).await?;

    Ok(())
}
