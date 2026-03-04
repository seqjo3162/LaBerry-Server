use sqlx::SqlitePool;

pub const GLOBAL_SERVER_ID: i64 = 1;

pub async fn ensure_global_server(db: &SqlitePool) -> anyhow::Result<()> {
    let exists: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM servers WHERE id = ?"
    )
    .bind(GLOBAL_SERVER_ID)
    .fetch_optional(db)
    .await?;

    if exists.is_some() {
        return Ok(());
    }

    let system_user_id: i64 = match sqlx::query_scalar(
        "SELECT id FROM users WHERE username = '__system__' LIMIT 1"
    )
    .fetch_optional(db)
    .await? {
        Some(id) => id,
        None => {
            let res = sqlx::query(
                r#"
                INSERT INTO users (username, password_hash, is_banned, created_at)
                VALUES ('__system__', '', 1, datetime('now'))
                "#
            )
            .execute(db)
            .await?;

            res.last_insert_rowid()
        }
    };

    sqlx::query(
        r#"
        INSERT INTO servers (id, name, owner_id, created_at)
        VALUES (?, 'Global', ?, datetime('now'))
        "#
    )
    .bind(GLOBAL_SERVER_ID)
    .bind(system_user_id)
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO chats (server_id, name, kind, created_at)
        VALUES (?, 'general', 'text', datetime('now'))"#
    )
    .bind(GLOBAL_SERVER_ID)
    .execute(db)
    .await?;

    let _ = sqlx::query(
        r#"
        INSERT INTO chats (server_id, name, kind, created_at)
        SELECT ?, 'General chat', 'voice', datetime('now')
        WHERE NOT EXISTS (
            SELECT 1 FROM chats
            WHERE server_id = ? AND COALESCE(kind,'text') = 'voice' AND COALESCE(name,'') = 'General chat'
        )
        "#
    )
    .bind(GLOBAL_SERVER_ID)
    .bind(GLOBAL_SERVER_ID)
    .execute(db)
    .await;

    Ok(())
}

pub async fn add_user_to_global_server(
    db: &SqlitePool,
    user_id: i64,
) -> anyhow::Result<()> {
    let exists: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM servers WHERE id = ?"
    )
    .bind(GLOBAL_SERVER_ID)
    .fetch_optional(db)
    .await?;

    if exists.is_none() {
        anyhow::bail!("Global server does not exist");
    }

    let user_exists: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM users WHERE id = ?"
    )
    .bind(user_id)
    .fetch_optional(db)
    .await?;

    if user_exists.is_none() {
        anyhow::bail!("User does not exist");
    }

    sqlx::query(
        r#"
        INSERT INTO server_members (server_id, user_id)
        VALUES (?, ?)
        ON CONFLICT(server_id, user_id) DO NOTHING
        "#
    )
    .bind(GLOBAL_SERVER_ID)
    .bind(user_id)
    .execute(db)
    .await?;
 
    let _ = sqlx::query(
        r#"
        INSERT INTO chats (server_id, name, kind, created_at)
        SELECT ?, 'General chat', 'voice', datetime('now')
        WHERE NOT EXISTS (
            SELECT 1 FROM chats
            WHERE server_id = ? AND COALESCE(kind,'text') = 'voice' AND COALESCE(name,'') = 'General chat'
        )
        "#
    )
    .bind(GLOBAL_SERVER_ID)
    .bind(GLOBAL_SERVER_ID)
    .execute(db)
    .await;

    Ok(())
}
