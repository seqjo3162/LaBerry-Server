use std::{env, path::PathBuf};

use anyhow::Context;
use clap::{Parser, Subcommand};
use dialoguer::{theme::ColorfulTheme, Confirm, FuzzySelect, Input, MultiSelect, Password, Select};
use password_hash::{PasswordHash, PasswordVerifier, SaltString};
use regex::Regex;
use sqlx::{Row, SqlitePool};

use laberry_server::{auth, db};

use argon2::PasswordHasher;

#[derive(Parser, Debug)]
#[command(name = "laberry_admin", version, about = "LaBerry admin CLI (destructive ops)")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    #[arg(long)]
    db: Option<String>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    Interactive,
    HashAdminPassword {
        #[arg(long)]
        password: Option<String>,
    },

    ListUsers {
        #[arg(long, default_value_t = 200)]
        limit: u32,
    },

    ListServers {
        #[arg(long, default_value_t = 200)]
        limit: u32,
    },

    BanUser {
        user_id: i64,
    },

    BanUserForever {
        user_id: i64,
    },

    PurgeUserContent {
        user_id: i64,
    },

    PurgeServer {
        server_id: i64,
    },

    PurgeTestUsers {
        #[arg(long)]
        regex: Option<String>,
    },
}

struct Settings {
    db_path: String,
    test_user_re: Regex,
    test_server_re: Regex,
    admin_password_hash: Option<String>,
    admin_password_plain: Option<String>,
}

impl Settings {
    fn from_env(override_db: Option<String>) -> anyhow::Result<Self> {
        let db_path = override_db
            .or_else(|| env::var("LB_DB_PATH").ok())
            .unwrap_or_else(|| "./laberry.db".to_string());

        let test_user_re = Regex::new(
            &env::var("LB_TEST_USER_REGEX").unwrap_or_else(|_| "^test_".to_string()),
        )
        .context("Invalid LB_TEST_USER_REGEX")?;

        let test_server_re = Regex::new(
            &env::var("LB_TEST_SERVER_REGEX").unwrap_or_else(|_| "^test_".to_string()),
        )
        .context("Invalid LB_TEST_SERVER_REGEX")?;

        let admin_password_hash = env::var("LB_ADMIN_PASSWORD_HASH").ok();
        let admin_password_plain = env::var("LB_ADMIN_PASSWORD").ok();

        Ok(Self {
            db_path,
            test_user_re,
            test_server_re,
            admin_password_hash,
            admin_password_plain,
        })
    }

    fn has_admin_password(&self) -> bool {
        self.admin_password_hash.is_some() || self.admin_password_plain.is_some()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let st = Settings::from_env(cli.db)?;

    match cli.cmd.unwrap_or(Cmd::Interactive) {
        Cmd::Interactive => interactive(st).await,
        Cmd::HashAdminPassword { password } => hash_admin_password(password),
        Cmd::ListUsers { limit } => {
            let db = connect_db(&st.db_path).await?;
            list_users(&db, limit).await
        }
        Cmd::ListServers { limit } => {
            let db = connect_db(&st.db_path).await?;
            list_servers(&db, limit).await
        }
        Cmd::BanUser { user_id } => {
            let db = connect_db(&st.db_path).await?;
            ban_user_checked(&st, &db, user_id).await
        }
        Cmd::BanUserForever { user_id } => {
            let db = connect_db(&st.db_path).await?;
            purge_user(&st, &db, user_id, true).await
        }
        Cmd::PurgeUserContent { user_id } => {
            let db = connect_db(&st.db_path).await?;
            purge_user_content(&st, &db, user_id).await
        }
        Cmd::PurgeServer { server_id } => {
            let db = connect_db(&st.db_path).await?;
            purge_server(&st, &db, server_id).await
        }
        Cmd::PurgeTestUsers { regex } => {
            let db = connect_db(&st.db_path).await?;
            purge_test_users(&st, &db, regex).await
        }
    }
}

async fn connect_db(db_path: &str) -> anyhow::Result<SqlitePool> {
    let url = if db_path.starts_with("sqlite:") {
        db_path.to_string()
    } else {
        format!("sqlite:{}?mode=rwc", db_path)
    };

    let db = SqlitePool::connect(&url)
        .await
        .with_context(|| format!("Failed to connect DB: {}", db_path))?;

    db::init(&db).await.context("DB init failed")?;
    db::bootstrap::ensure_global_server(&db)
        .await
        .context("ensure_global_server failed")?;

    Ok(db)
}

fn hash_admin_password(password: Option<String>) -> anyhow::Result<()> {
    let theme = ColorfulTheme::default();
    let pw = match password {
        Some(p) => p,
        None => Password::with_theme(&theme)
            .with_prompt("Admin password")
            .with_confirmation("Repeat password", "Passwords do not match")
            .interact()?,
    };

    let salt = SaltString::generate(&mut rand::thread_rng());
    let argon2 = argon2::Argon2::default();
    let hash = argon2
        .hash_password(pw.as_bytes(), &salt)
        .context("hash_password failed")?
        .to_string();
    println!("LB_ADMIN_PASSWORD_HASH={}", hash);
    Ok(())
}

async fn interactive(st: Settings) -> anyhow::Result<()> {
    let theme = ColorfulTheme::default();
    let db = connect_db(&st.db_path).await?;

    loop {
        let actions = vec![
            "Users: ban",
            "Users: ban forever (delete)",
            "Users: purge content only",
            "Users: delete multiple test users",
            "Servers: delete server",
            "List: users",
            "List: servers",
            "Exit",
        ];

        let idx = Select::with_theme(&theme)
            .with_prompt("Choose action")
            .items(&actions)
            .default(0)
            .interact()?;

        match idx {
            0 => {
                let user_id = pick_user(&theme, &db).await?;
                ban_user_checked(&st, &db, user_id).await?;
            }
            1 => {
                let user_id = pick_user(&theme, &db).await?;
                purge_user(&st, &db, user_id, true).await?;
            }
            2 => {
                let user_id = pick_user(&theme, &db).await?;
                purge_user_content(&st, &db, user_id).await?;
            }
            3 => {
                purge_test_users(&st, &db, None).await?;
            }
            4 => {
                let server_id = pick_server(&theme, &db).await?;
                purge_server(&st, &db, server_id).await?;
            }
            5 => {
                list_users(&db, 200).await?;
            }
            6 => {
                list_servers(&db, 200).await?;
            }
            _ => break,
        }
    }

    Ok(())
}

async fn list_users(db: &SqlitePool, limit: u32) -> anyhow::Result<()> {
    let rows = sqlx::query(
        r#"SELECT id, username, COALESCE(email,'') AS email, is_banned, created_at
           FROM users
           ORDER BY id DESC
           LIMIT ?"#,
    )
    .bind(limit as i64)
    .fetch_all(db)
    .await?;

    for r in rows {
        let id: i64 = r.get("id");
        let username: String = r.get("username");
        let email: String = r.get("email");
        let is_banned: i64 = r.get("is_banned");
        let created_at: String = r.get("created_at");
        let banned = if is_banned != 0 { "banned" } else { "" };
        if email.is_empty() {
            println!("#{} {} {} {}", id, username, banned, created_at);
        } else {
            println!("#{} {} <{}> {} {}", id, username, email, banned, created_at);
        }
    }

    Ok(())
}

async fn list_servers(db: &SqlitePool, limit: u32) -> anyhow::Result<()> {
    let rows = sqlx::query(
        r#"SELECT s.id, s.name, s.owner_id,
                  (SELECT COUNT(*) FROM server_members sm WHERE sm.server_id = s.id) AS members
           FROM servers s
           ORDER BY s.id DESC
           LIMIT ?"#,
    )
    .bind(limit as i64)
    .fetch_all(db)
    .await?;

    for r in rows {
        let id: i64 = r.get("id");
        let name: String = r.get("name");
        let owner_id: i64 = r.get("owner_id");
        let members: i64 = r.get("members");
        println!("#{} {} (owner #{}, members {})", id, name, owner_id, members);
    }
    Ok(())
}

async fn pick_user(theme: &ColorfulTheme, db: &SqlitePool) -> anyhow::Result<i64> {
    let q: String = Input::with_theme(theme)
        .with_prompt("Search (username/email/id), empty for latest")
        .allow_empty(true)
        .interact_text()?;

    let rows = if q.trim().is_empty() {
        sqlx::query(
            r#"SELECT id, username, COALESCE(email,'') AS email, is_banned, created_at
               FROM users
               ORDER BY id DESC
               LIMIT 200"#,
        )
        .fetch_all(db)
        .await?
    } else {
        let like = format!("%{}%", q.trim());
        sqlx::query(
            r#"SELECT id, username, COALESCE(email,'') AS email, is_banned, created_at
               FROM users
               WHERE username LIKE ? OR email LIKE ? OR CAST(id AS TEXT) LIKE ?
               ORDER BY id DESC
               LIMIT 200"#,
        )
        .bind(&like)
        .bind(&like)
        .bind(&like)
        .fetch_all(db)
        .await?
    };

    if rows.is_empty() {
        anyhow::bail!("No users found");
    }

    let items: Vec<String> = rows
        .iter()
        .map(|r| {
            let id: i64 = r.get("id");
            let username: String = r.get("username");
            let email: String = r.get("email");
            let is_banned: i64 = r.get("is_banned");
            let created_at: String = r.get("created_at");
            let banned = if is_banned != 0 { "banned" } else { "" };
            if email.is_empty() {
                format!("#{} {} {} {}", id, username, banned, created_at)
            } else {
                format!("#{} {} <{}> {} {}", id, username, email, banned, created_at)
            }
        })
        .collect();

    let idx = FuzzySelect::with_theme(theme)
        .with_prompt("Pick user")
        .items(&items)
        .default(0)
        .interact()?;

    let id: i64 = rows[idx].get("id");
    Ok(id)
}

async fn pick_server(theme: &ColorfulTheme, db: &SqlitePool) -> anyhow::Result<i64> {
    let q: String = Input::with_theme(theme)
        .with_prompt("Search (name/id), empty for latest")
        .allow_empty(true)
        .interact_text()?;

    let rows = if q.trim().is_empty() {
        sqlx::query(
            r#"SELECT s.id, s.name, s.owner_id,
                      (SELECT COUNT(*) FROM server_members sm WHERE sm.server_id = s.id) AS members
               FROM servers s
               ORDER BY s.id DESC
               LIMIT 200"#,
        )
        .fetch_all(db)
        .await?
    } else {
        let like = format!("%{}%", q.trim());
        sqlx::query(
            r#"SELECT s.id, s.name, s.owner_id,
                      (SELECT COUNT(*) FROM server_members sm WHERE sm.server_id = s.id) AS members
               FROM servers s
               WHERE s.name LIKE ? OR CAST(s.id AS TEXT) LIKE ?
               ORDER BY s.id DESC
               LIMIT 200"#,
        )
        .bind(&like)
        .bind(&like)
        .fetch_all(db)
        .await?
    };

    if rows.is_empty() {
        anyhow::bail!("No servers found");
    }

    let items: Vec<String> = rows
        .iter()
        .map(|r| {
            let id: i64 = r.get("id");
            let name: String = r.get("name");
            let owner_id: i64 = r.get("owner_id");
            let members: i64 = r.get("members");
            format!("#{} {} (owner #{}, members {})", id, name, owner_id, members)
        })
        .collect();

    let idx = FuzzySelect::with_theme(theme)
        .with_prompt("Pick server")
        .items(&items)
        .default(0)
        .interact()?;

    let id: i64 = rows[idx].get("id");
    Ok(id)
}

async fn ban_user(db: &SqlitePool, user_id: i64) -> anyhow::Result<()> {
    let now = auth::now_iso();
    let mut tx = db.begin().await?;

    let affected = sqlx::query(
        r#"UPDATE users
           SET is_banned = 1,
               token_version = token_version + 1
           WHERE id = ?"#,
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    if affected == 0 {
        anyhow::bail!("User not found");
    }

    let _ = sqlx::query("UPDATE user_sessions SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL")
        .bind(&now)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("UPDATE refresh_sessions SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL")
        .bind(&now)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    println!("OK: user #{} banned", user_id);
    Ok(())
}

async fn ban_user_checked(st: &Settings, db: &SqlitePool, user_id: i64) -> anyhow::Result<()> {
    let theme = ColorfulTheme::default();
    let row = sqlx::query("SELECT username, COALESCE(email,'') AS email FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(db)
        .await?;
    let Some(r) = row else { anyhow::bail!("User not found") };
    let username: String = r.get("username");
    let email: String = r.get("email");

    let is_test = is_test_user(&st.test_user_re, &username, &email);
    if !is_test {
        require_admin_password(st, &theme, "ban non-test user")?;
    }

    let phrase = format!("BAN USER {}", user_id);
    require_typed_phrase(&theme, &phrase)?;
    if !Confirm::with_theme(&theme)
        .with_prompt("Are you sure?")
        .default(false)
        .interact()?
    {
        println!("Cancelled");
        return Ok(());
    }

    ban_user(db, user_id).await
}

fn is_test_user(re: &Regex, username: &str, email: &str) -> bool {
    re.is_match(username) || (!email.is_empty() && re.is_match(email))
}

fn is_test_server(re: &Regex, name: &str) -> bool {
    re.is_match(name)
}

fn require_admin_password(st: &Settings, theme: &ColorfulTheme, why: &str) -> anyhow::Result<()> {
    if !st.has_admin_password() {
        anyhow::bail!(
            "Admin password is not configured (set LB_ADMIN_PASSWORD_HASH or LB_ADMIN_PASSWORD). Refusing: {}",
            why
        );
    }

    let typed = Password::with_theme(theme)
        .with_prompt("Admin password")
        .interact()?;

    if let Some(plain) = &st.admin_password_plain {
        if &typed == plain {
            return Ok(());
        }
    }

    if let Some(phc) = &st.admin_password_hash {
        let parsed = PasswordHash::new(phc).context("LB_ADMIN_PASSWORD_HASH invalid")?;
        let argon2 = argon2::Argon2::default();
        if argon2
            .verify_password(typed.as_bytes(), &parsed)
            .is_ok()
        {
            return Ok(());
        }
    }

    anyhow::bail!("Wrong admin password")
}

fn require_typed_phrase(theme: &ColorfulTheme, phrase: &str) -> anyhow::Result<()> {
    let typed: String = Input::with_theme(theme)
        .with_prompt(format!("Type exactly: {}", phrase))
        .interact_text()?;
    if typed.trim() != phrase {
        anyhow::bail!("Confirmation phrase mismatch")
    }
    Ok(())
}

async fn purge_server(st: &Settings, db: &SqlitePool, server_id: i64) -> anyhow::Result<()> {
    let theme = ColorfulTheme::default();

    let row = sqlx::query("SELECT id, name FROM servers WHERE id = ?")
        .bind(server_id)
        .fetch_optional(db)
        .await?;
    let Some(r) = row else {
        anyhow::bail!("Server not found")
    };

    let name: String = r.get("name");
    let is_test = is_test_server(&st.test_server_re, &name);
    if !is_test {
        require_admin_password(st, &theme, "delete non-test server")?;
    }

    let phrase = format!("DELETE SERVER {}", server_id);
    require_typed_phrase(&theme, &phrase)?;

    if !Confirm::with_theme(&theme)
        .with_prompt("Are you sure?")
        .default(false)
        .interact()?
    {
        println!("Cancelled");
        return Ok(());
    }

    purge_server_exec(db, server_id).await?;

    println!("OK: server #{} deleted", server_id);
    Ok(())
}

async fn purge_server_exec(db: &SqlitePool, server_id: i64) -> anyhow::Result<()> {
    let mut tx = db.begin().await?;

    let file_rows = sqlx::query(
        r#"SELECT f.storage_path, f.filename
           FROM files f
           JOIN chats c ON c.id = f.chat_id
           WHERE c.server_id = ?"#,
    )
    .bind(server_id)
    .fetch_all(&mut *tx)
    .await?;

    let mut file_paths: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    for fr in file_rows {
        let p: String = fr.get("storage_path");
        let stored_filename: String = fr.get("filename");
        let main = PathBuf::from(p);
        let thumb = PathBuf::from("storage/files/thumbs").join(format!(
            "{}.png",
            std::path::Path::new(&stored_filename)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&stored_filename)
        ));
        file_paths.push((main, Some(thumb)));
    }

    let chat_ids = sqlx::query_scalar::<_, i64>("SELECT id FROM chats WHERE server_id = ?")
        .bind(server_id)
        .fetch_all(&mut *tx)
        .await?;

    for chat_id in &chat_ids {
        let _ = sqlx::query(
            r#"DELETE FROM message_reactions
               WHERE message_id IN (SELECT id FROM messages WHERE chat_id = ?)"#,
        )
        .bind(*chat_id)
        .execute(&mut *tx)
        .await?;

        let _ = sqlx::query("DELETE FROM pinned_messages WHERE chat_id = ?")
            .bind(*chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query("DELETE FROM chat_reads WHERE chat_id = ?")
            .bind(*chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query("DELETE FROM files WHERE chat_id = ?")
            .bind(*chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query("DELETE FROM messages WHERE chat_id = ?")
            .bind(*chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query("DELETE FROM chat_participants WHERE chat_id = ?")
            .bind(*chat_id)
            .execute(&mut *tx)
            .await?;
    }

    let _ = sqlx::query("DELETE FROM chats WHERE server_id = ?")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM server_members WHERE server_id = ?")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;

    let affected = sqlx::query("DELETE FROM servers WHERE id = ?")
        .bind(server_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();

    if affected == 0 {
        anyhow::bail!("Server not found")
    }

    tx.commit().await?;

    for (main, thumb) in file_paths {
        let _ = std::fs::remove_file(&main);
        if let Some(t) = thumb {
            let _ = std::fs::remove_file(&t);
        }
    }

    Ok(())
}

async fn purge_user_content(st: &Settings, db: &SqlitePool, user_id: i64) -> anyhow::Result<()> {
    let theme = ColorfulTheme::default();

    let row = sqlx::query("SELECT username, COALESCE(email,'') AS email FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(db)
        .await?;
    let Some(r) = row else { anyhow::bail!("User not found") };
    let username: String = r.get("username");
    let email: String = r.get("email");
    let is_test = is_test_user(&st.test_user_re, &username, &email);
    if !is_test {
        require_admin_password(st, &theme, "purge non-test user content")?;
    }

    let phrase = format!("PURGE USER CONTENT {}", user_id);
    require_typed_phrase(&theme, &phrase)?;

    if !Confirm::with_theme(&theme)
        .with_prompt("Are you sure?")
        .default(false)
        .interact()?
    {
        println!("Cancelled");
        return Ok(());
    }

    let mut tx = db.begin().await?;

    let file_rows = sqlx::query("SELECT storage_path, filename FROM files WHERE uploaded_by = ?")
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
    let mut file_paths: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    for fr in file_rows {
        let p: String = fr.get("storage_path");
        let stored_filename: String = fr.get("filename");
        let main = PathBuf::from(p);
        let thumb = PathBuf::from("storage/files/thumbs").join(format!(
            "{}.png",
            std::path::Path::new(&stored_filename)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&stored_filename)
        ));
        file_paths.push((main, Some(thumb)));
    }

    let profile_rows = sqlx::query("SELECT storage_path FROM profile_files WHERE uploaded_by = ?")
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
    let mut profile_paths: Vec<PathBuf> = Vec::new();
    for pr in profile_rows {
        let p: String = pr.get("storage_path");
        profile_paths.push(PathBuf::from(p));
    }

    let _ = sqlx::query(
        r#"DELETE FROM message_reactions
           WHERE message_id IN (SELECT id FROM messages WHERE sender_id = ?)"#,
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;

    let _ = sqlx::query(
        r#"DELETE FROM pinned_messages
           WHERE message_id IN (SELECT id FROM messages WHERE sender_id = ?)"#,
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;

    let _ = sqlx::query("DELETE FROM files WHERE uploaded_by = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM messages WHERE sender_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM profile_files WHERE uploaded_by = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    for (main, thumb) in file_paths {
        let _ = std::fs::remove_file(&main);
        if let Some(t) = thumb {
            let _ = std::fs::remove_file(&t);
        }
    }
    for p in profile_paths {
        let _ = std::fs::remove_file(&p);
    }

    println!("OK: user #{} content purged", user_id);
    Ok(())
}

async fn purge_user(st: &Settings, db: &SqlitePool, user_id: i64, permanent_ban: bool) -> anyhow::Result<()> {
    let theme = ColorfulTheme::default();

    let row = sqlx::query("SELECT username, COALESCE(email,'') AS email FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(db)
        .await?;
    let Some(r) = row else { anyhow::bail!("User not found") };
    let username: String = r.get("username");
    let email: String = r.get("email");

    let is_test = is_test_user(&st.test_user_re, &username, &email);
    if !is_test {
        require_admin_password(st, &theme, "delete non-test user")?;
    }

    let phrase = if permanent_ban {
        format!("DELETE USER {}", user_id)
    } else {
        format!("PURGE USER {}", user_id)
    };
    require_typed_phrase(&theme, &phrase)?;

    if !Confirm::with_theme(&theme)
        .with_prompt("Are you sure?")
        .default(false)
        .interact()?
    {
        println!("Cancelled");
        return Ok(());
    }

    purge_user_exec(db, user_id).await?;
    println!("OK: user #{} deleted", user_id);
    Ok(())
}

async fn purge_user_exec(db: &SqlitePool, user_id: i64) -> anyhow::Result<()> {
    let mut tx = db.begin().await?;

    let owned_servers = sqlx::query_scalar::<_, i64>("SELECT id FROM servers WHERE owner_id = ?")
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;

    for sid in owned_servers {
        purge_server_exec(db, sid).await?;
    }

    let mut tx = db.begin().await?;

    let dm_chat_ids = sqlx::query_scalar::<_, i64>(
        "SELECT chat_id FROM dm_chats WHERE user_a = ? OR user_b = ?",
    )
    .bind(user_id)
    .bind(user_id)
    .fetch_all(&mut *tx)
    .await?;

    let dm_file_rows = if dm_chat_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query(
            r#"SELECT f.storage_path, f.filename
               FROM files f
               WHERE f.chat_id IN (SELECT chat_id FROM dm_chats WHERE user_a = ? OR user_b = ?)"#,
        )
        .bind(user_id)
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?
    };

    let mut file_paths: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    for fr in dm_file_rows {
        let p: String = fr.get("storage_path");
        let stored_filename: String = fr.get("filename");
        let main = PathBuf::from(p);
        let thumb = PathBuf::from("storage/files/thumbs").join(format!(
            "{}.png",
            std::path::Path::new(&stored_filename)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&stored_filename)
        ));
        file_paths.push((main, Some(thumb)));
    }

    let user_file_rows = sqlx::query("SELECT storage_path, filename FROM files WHERE uploaded_by = ?")
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
    for fr in user_file_rows {
        let p: String = fr.get("storage_path");
        let stored_filename: String = fr.get("filename");
        let main = PathBuf::from(p);
        let thumb = PathBuf::from("storage/files/thumbs").join(format!(
            "{}.png",
            std::path::Path::new(&stored_filename)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&stored_filename)
        ));
        file_paths.push((main, Some(thumb)));
    }

    let profile_rows = sqlx::query("SELECT storage_path FROM profile_files WHERE uploaded_by = ?")
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
    let mut profile_paths: Vec<PathBuf> = Vec::new();
    for pr in profile_rows {
        let p: String = pr.get("storage_path");
        profile_paths.push(PathBuf::from(p));
    }

    let _ = sqlx::query("DELETE FROM message_reactions WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query(
        r#"DELETE FROM message_reactions
           WHERE message_id IN (SELECT id FROM messages WHERE sender_id = ?)"#,
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;

    let _ = sqlx::query("DELETE FROM pinned_messages WHERE pinned_by = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query(
        r#"DELETE FROM pinned_messages
           WHERE message_id IN (SELECT id FROM messages WHERE sender_id = ?)"#,
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;

    let _ = sqlx::query("DELETE FROM chat_reads WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM chat_participants WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM server_members WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM friendships WHERE user_id = ? OR friend_id = ?")
        .bind(user_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM friend_requests WHERE sender_id = ? OR receiver_id = ?")
        .bind(user_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM user_blocks WHERE blocker_id = ? OR blocked_id = ?")
        .bind(user_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM user_presence WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM user_settings WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM user_profile WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM user_sessions WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM refresh_sessions WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM email_codes WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM profile_files WHERE uploaded_by = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM files WHERE uploaded_by = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM messages WHERE sender_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    for chat_id in dm_chat_ids {
        let _ = sqlx::query(
            r#"DELETE FROM message_reactions WHERE message_id IN (SELECT id FROM messages WHERE chat_id = ?)"#,
        )
        .bind(chat_id)
        .execute(&mut *tx)
        .await?;

        let _ = sqlx::query("DELETE FROM pinned_messages WHERE chat_id = ?")
            .bind(chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query("DELETE FROM chat_reads WHERE chat_id = ?")
            .bind(chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query("DELETE FROM files WHERE chat_id = ?")
            .bind(chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query("DELETE FROM messages WHERE chat_id = ?")
            .bind(chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query("DELETE FROM chat_participants WHERE chat_id = ?")
            .bind(chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query("DELETE FROM dm_chats WHERE chat_id = ?")
            .bind(chat_id)
            .execute(&mut *tx)
            .await?;

        let _ = sqlx::query("DELETE FROM chats WHERE id = ?")
            .bind(chat_id)
            .execute(&mut *tx)
            .await?;
    }

    let affected = sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();

    if affected == 0 {
        anyhow::bail!("User not found")
    }

    tx.commit().await?;

    for (main, thumb) in file_paths {
        let _ = std::fs::remove_file(&main);
        if let Some(t) = thumb {
            let _ = std::fs::remove_file(&t);
        }
    }
    for p in profile_paths {
        let _ = std::fs::remove_file(&p);
    }

    Ok(())
}

async fn purge_test_users(st: &Settings, db: &SqlitePool, regex: Option<String>) -> anyhow::Result<()> {
    let theme = ColorfulTheme::default();
    let re = if let Some(r) = regex {
        Regex::new(&r).context("Invalid regex")?
    } else {
        st.test_user_re.clone()
    };

    let rows = sqlx::query(
        r#"SELECT id, username, COALESCE(email,'') AS email, is_banned, created_at
           FROM users
           ORDER BY id DESC
           LIMIT 1000"#,
    )
    .fetch_all(db)
    .await?;

    let mut pick_rows: Vec<(i64, String, String)> = Vec::new();
    let mut items: Vec<String> = Vec::new();
    for r in rows {
        let id: i64 = r.get("id");
        let username: String = r.get("username");
        let email: String = r.get("email");
        let is_banned: i64 = r.get("is_banned");
        let created_at: String = r.get("created_at");
        if is_test_user(&re, &username, &email) {
            let banned = if is_banned != 0 { "banned" } else { "" };
            let line = if email.is_empty() {
                format!("#{} {} {} {}", id, username, banned, created_at)
            } else {
                format!("#{} {} <{}> {} {}", id, username, email, banned, created_at)
            };
            pick_rows.push((id, username, email));
            items.push(line);
        }
    }

    if items.is_empty() {
        println!("No test users found");
        return Ok(());
    }

    let selected = MultiSelect::with_theme(&theme)
        .with_prompt("Select test users to delete")
        .items(&items)
        .interact()?;

    if selected.is_empty() {
        println!("Nothing selected");
        return Ok(());
    }

    let phrase = format!("DELETE {} TEST USERS", selected.len());
    require_typed_phrase(&theme, &phrase)?;

    if !Confirm::with_theme(&theme)
        .with_prompt("Are you sure?")
        .default(false)
        .interact()?
    {
        println!("Cancelled");
        return Ok(());
    }

    for idx in selected {
        let (id, _u, _e) = &pick_rows[idx];
        purge_user_exec(db, *id).await?;
        println!("OK: user #{} deleted", id);
    }

    Ok(())
}
