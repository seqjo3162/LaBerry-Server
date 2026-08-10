use sqlx::PgPool;
use chrono::Utc;

use crate::models::{ServerIden, UserIden, ChatIden, ServerMemberIden};

pub const GLOBAL_SERVER_ID: i64 = 1;

async fn ensure_voice_channel(db: &PgPool, server_id: i64) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO chats (server_id, name, kind, created_at)
        SELECT $1, 'General chat', 'voice', $2
        WHERE NOT EXISTS (
            SELECT 1 FROM chats
            WHERE server_id = $1 AND kind = 'voice' AND name = 'General chat'
        )
        "#
    )
    .bind(server_id)
    .bind(Utc::now())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn ensure_global_server(db: &PgPool) -> anyhow::Result<()> {
    let exists: Option<i64> = sqlx::query_scalar(
        &format!("SELECT id FROM {} WHERE id = $1", ServerIden::Table.to_string())
    )
    .bind(GLOBAL_SERVER_ID)
    .fetch_optional(db)
    .await?;

    if exists.is_some() {
        return Ok(());
    }

    let system_user_id: i64 = match sqlx::query_scalar(
        &format!("SELECT id FROM {} WHERE username = '__system__' LIMIT 1", UserIden::Table.to_string())
    )
    .fetch_optional(db)
    .await? {
        Some(id) => id,
        None => {
            let res = sqlx::query(
                &format!(
                    "INSERT INTO {} (username, password_hash, is_banned, created_at)
                     VALUES ('__system__', '', false, $1)
                     RETURNING id",
                    UserIden::Table.to_string()
                )
            )
            .bind(Utc::now())
            .execute(db)
            .await?;
            res.last_insert_rowid()
        }
    };

    sqlx::query(
        &format!(
            "INSERT INTO {} (id, name, owner_id, created_at)
             VALUES ($1, 'Global', $2, $3)",
            ServerIden::Table.to_string()
        )
    )
    .bind(GLOBAL_SERVER_ID)
    .bind(system_user_id)
    .bind(Utc::now())
    .execute(db)
    .await?;

    sqlx::query(
        &format!(
            "INSERT INTO {} (server_id, name, kind, created_at)
             VALUES ($1, 'general', 'text', $2)",
            ChatIden::Table.to_string()
        )
    )
    .bind(GLOBAL_SERVER_ID)
    .bind(Utc::now())
    .execute(db)
    .await?;

    ensure_voice_channel(db, GLOBAL_SERVER_ID).await?;

    Ok(())
}

pub async fn add_user_to_global_server(db: &PgPool, user_id: i64) -> anyhow::Result<()> {
    let server_exists: Option<i64> = sqlx::query_scalar(
        &format!("SELECT id FROM {} WHERE id = $1", ServerIden::Table.to_string())
    )
    .bind(GLOBAL_SERVER_ID)
    .fetch_optional(db)
    .await?;

    if server_exists.is_none() {
        anyhow::bail!("Global server does not exist");
    }

    let user_exists: Option<i64> = sqlx::query_scalar(
        &format!("SELECT id FROM {} WHERE id = $1", UserIden::Table.to_string())
    )
    .bind(user_id)
    .fetch_optional(db)
    .await?;

    if user_exists.is_none() {
        anyhow::bail!("User does not exist");
    }

    sqlx::query(
        &format!(
            "INSERT INTO {} (server_id, user_id)
             VALUES ($1, $2)
             ON CONFLICT(server_id, user_id) DO NOTHING",
            ServerMemberIden::Table.to_string()
        )
    )
    .bind(GLOBAL_SERVER_ID)
    .bind(user_id)
    .execute(db)
    .await?;
    ensure_voice_channel(db, GLOBAL_SERVER_ID).await?;

    Ok(())
}