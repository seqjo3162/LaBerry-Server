use crate::server::AppState;
use axum::{
    extract::{Form, Path, Query, State},
    http::HeaderMap,
    response::IntoResponse,
};
use sqlx::{Row, PgPool};
use super::*;

#[derive(Clone)]
pub(crate) struct ServerRow {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) owner_id: i64,
    pub(crate) owner_username: String,
    pub(crate) created_at: String,
}

pub(crate) async fn fetch_servers(db: &PgPool, q: &str, limit: i64) -> anyhow::Result<Vec<ServerRow>> {
    if q.is_empty() {
        let rows = sqlx::query(
            r#"SELECT s.id, s.name, s.owner_id, COALESCE(u.username,'') AS owner_username, s.created_at
               FROM servers s LEFT JOIN users u ON u.id = s.owner_id ORDER BY s.id DESC LIMIT $1"#,
        ).bind(limit).fetch_all(db).await?;
        return Ok(rows.into_iter().map(|r| ServerRow {
            id: r.get("id"), name: r.get("name"), owner_id: r.get("owner_id"),
            owner_username: r.get("owner_username"), created_at: r.get("created_at"),
        }).collect());
    }
    if let Ok(id) = q.parse::<i64>() {
        let rows = sqlx::query(
            r#"SELECT s.id, s.name, s.owner_id, COALESCE(u.username,'') AS owner_username, s.created_at
               FROM servers s LEFT JOIN users u ON u.id = s.owner_id WHERE s.id = $1 LIMIT $2"#,
        ).bind(id).bind(limit).fetch_all(db).await?;
        return Ok(rows.into_iter().map(|r| ServerRow {
            id: r.get("id"), name: r.get("name"), owner_id: r.get("owner_id"),
            owner_username: r.get("owner_username"), created_at: r.get("created_at"),
        }).collect());
    }
    let like = format!("%{}%", q);
    let rows = sqlx::query(
        r#"SELECT s.id, s.name, s.owner_id, COALESCE(u.username,'') AS owner_username, s.created_at
           FROM servers s LEFT JOIN users u ON u.id = s.owner_id WHERE s.name LIKE $1 ORDER BY s.id DESC LIMIT $2"#,
    ).bind(&like).bind(limit).fetch_all(db).await?;
    Ok(rows.into_iter().map(|r| ServerRow {
        id: r.get("id"), name: r.get("name"), owner_id: r.get("owner_id"),
        owner_username: r.get("owner_username"), created_at: r.get("created_at"),
    }).collect())
}

pub(crate) async fn purge_server_exec(db: &PgPool, server_id: i64) -> anyhow::Result<()> {
    use std::path::PathBuf;
    let mut tx = db.begin().await?;
    let file_rows = sqlx::query(r#"SELECT f.storage_path, f.filename FROM files f JOIN chats c ON c.id = f.chat_id WHERE c.server_id = $1"#)
        .bind(server_id).fetch_all(&mut *tx).await?;
    let mut file_paths: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    for fr in file_rows {
        let p: String = fr.get("storage_path");
        let stored_filename: String = fr.get("filename");
        let main = PathBuf::from(p);
        let thumb = PathBuf::from("storage/files/thumbs").join(format!("{}.png",
            std::path::Path::new(&stored_filename).file_stem().and_then(|s| s.to_str()).unwrap_or(&stored_filename)));
        file_paths.push((main, Some(thumb)));
    }
    let chat_ids = sqlx::query_scalar::<_, i64>("SELECT id FROM chats WHERE server_id = $1").bind(server_id).fetch_all(&mut *tx).await?;
    for chat_id in &chat_ids {
        let _ = sqlx::query(r#"DELETE FROM message_reactions WHERE message_id IN (SELECT id FROM messages WHERE chat_id = $1)"#).bind(*chat_id).execute(&mut *tx).await?;
        let _ = sqlx::query("DELETE FROM pinned_messages WHERE chat_id = $1").bind(*chat_id).execute(&mut *tx).await?;
        let _ = sqlx::query("DELETE FROM chat_reads WHERE chat_id = $1").bind(*chat_id).execute(&mut *tx).await?;
        let _ = sqlx::query(r#"UPDATE gif_assets SET source_file_id = NULL WHERE source_file_id IN (SELECT id FROM files WHERE chat_id = $1)"#).bind(*chat_id).execute(&mut *tx).await?;
        let _ = sqlx::query("DELETE FROM files WHERE chat_id = $1").bind(*chat_id).execute(&mut *tx).await?;
        let _ = sqlx::query("DELETE FROM messages WHERE chat_id = $1").bind(*chat_id).execute(&mut *tx).await?;
        let _ = sqlx::query("DELETE FROM chat_participants WHERE chat_id = $1").bind(*chat_id).execute(&mut *tx).await?;
    }
    let _ = sqlx::query("DELETE FROM chats WHERE server_id = $1").bind(server_id).execute(&mut *tx).await?;
    let _ = sqlx::query("DELETE FROM server_members WHERE server_id = $1").bind(server_id).execute(&mut *tx).await?;
    let affected = sqlx::query("DELETE FROM servers WHERE id = $1").bind(server_id).execute(&mut *tx).await?.rows_affected();
    if affected == 0 { anyhow::bail!("Сервер не найден") }
    tx.commit().await?;
    let _ = file_paths;
    let _ = cleanup_file_storage_orphans_db(db).await;
    Ok(())
}

pub(crate) fn render_servers_panel_body(query: &str, rows_html: &str, embedded: bool) -> String {
    if embedded {
        return format!(r#"<div class='card'><div class='search-row'><div class='hstack'><h2 style='margin:0;'>Серверы</h2><span class='pill'>UTC</span></div><div class='center-inline-search'><input type='text' data-persist-key='admin-center-servers-search' data-filter-input='servers' value='{qval}' placeholder='Поиск: название сервера / id' /><button type='button' class='btn-soft' data-clear-filter='servers'>Сбросить</button></div></div></div><div class='card'><div class='servers-list' data-filter-list='servers'>{rows}</div></div>"#, qval = escape_html(query), rows = rows_html);
    }
    format!(r#"<div class='card'><div class='search-row'><div class='hstack'><h2 style='margin:0;'>Серверы</h2><span class='pill'>UTC</span></div><form method='get' action='/admin/servers'><input type='text' name='q' value='{qval}' placeholder='Поиск: название сервера / id (пусто = последние)' /><button type='submit'>Найти</button></form></div></div><div class='card'><div class='servers-list'>{rows}</div></div>"#, qval = escape_html(query), rows = rows_html)
}

pub(crate) async fn servers_list(State(st): State<AppState>, ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>, headers: HeaderMap, Query(q): Query<ListQuery>) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() { return e.into_response(); }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) { return e.into_response(); }
    let (_sid, sess) = match require_auth(&st, &headers) { Ok(v) => v, Err(r) => return r.into_response() };
    let query = q.q.clone().unwrap_or_default().trim().to_string();
    let servers = fetch_servers(&st.db, &query, 200).await;
    let mut rows_html = String::new();
    match servers {
        Ok(list) => {
            if list.is_empty() { rows_html.push_str("<div class='empty-state'>Серверы не найдены.</div>"); } 
            else {
                for s in list {
                    rows_html.push_str(&format!(r#"<div class='server-row-card'><div class='server-main'><div class='server-top'><div class='server-title'><span class='server-id'>#{id}</span><span class='server-name'>{name}</span></div><div class='user-meta'>Создан: {created_at}</div></div><div class='server-meta'>Владелец: #{owner_id} {owner_name}</div></div><div class='server-actions'><form method="post" action="/admin/servers/{id}/remove_user" class="inline-form" style="display:flex; gap:4px; align-items:center;"><input type="hidden" name="csrf" value="{csrf}" /><input type="hidden" name="return_to" value="{return_to}" /><input type="text" name="user_id" placeholder="User ID" style="width:70px; padding:6px;" required /><button type="submit" class="btn-danger" style="padding:6px 10px;">Удалить юзера</button></form><form method='post' action='/admin/servers/{id}/add_all_users' class='inline-form'><input type='hidden' name='csrf' value='{csrf}' /><input type='hidden' name='return_to' value='{return_to}' /><button type='submit' class='btn-soft'>Добавить всех</button></form><form method='post' action='/admin/servers/{id}/delete' class='inline-form'><input type='hidden' name='csrf' value='{csrf}' /><input type='hidden' name='return_to' value='{return_to}' /><button type='submit' class='btn-danger'>Удалить сервер</button></form></div></div>"#,
                        id = s.id, name = escape_html(&s.name), owner_id = s.owner_id, owner_name = escape_html(&s.owner_username), created_at = escape_html(&fmt_admin_dt(&s.created_at)), csrf = escape_html(&sess.csrf),
                        return_to = escape_html(&safe_admin_return_to(q.return_to.as_deref().unwrap_or(""), if q.embed == Some(1) { "/admin/center?view=servers" } else { "/admin/servers" })),
                    ));
                }
            }
        }
        Err(err) => { rows_html.push_str(&format!("<div class='empty-state'>Ошибка БД: {}</div>", escape_html(&format!("{}", err)))); }
    }
    let body = render_servers_panel_body(&query, &rows_html, q.embed == Some(1));
    if q.embed == Some(1) { embedded_page("Админка • Серверы", &body, q.msg.as_deref()).into_response() } 
    else { page("Админка • Серверы", &body, q.msg.as_deref()).into_response() }
}

pub(crate) async fn server_delete(State(st): State<AppState>, ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>, headers: HeaderMap, Path(id): Path<i64>, Form(f): Form<ActionForm>) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() { return e.into_response(); }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) { return e.into_response(); }
    let (_sid, sess) = match require_auth(&st, &headers) { Ok(v) => v, Err(r) => return r.into_response() };
    if f.csrf != sess.csrf { return admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/servers"), "CSRF-токен не совпадает").into_response(); }
    let row = sqlx::query("SELECT name FROM servers WHERE id = $1").bind(id).fetch_optional(&st.db).await;
    let Ok(Some(r)) = row else { return admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/servers"), "Сервер не найден").into_response(); };
    let name: String = r.get("name");
    let _re = test_server_re(); let _is_test = is_test_server(&_re, &name);
    match purge_server_exec(&st.db, id).await {
        Ok(_) => admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/servers"), "Готово").into_response(),
        Err(e) => admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/servers"), &format!("Ошибка: {}", e)).into_response(),
    }
}

pub(crate) async fn server_add_all_users(State(st): State<AppState>, ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>, headers: HeaderMap, Path(id): Path<i64>, Form(f): Form<ActionForm>) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() { return e.into_response(); }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) { return e.into_response(); }
    let (_sid, sess) = match require_auth(&st, &headers) { Ok(v) => v, Err(r) => return r.into_response() };
    let return_to = safe_admin_return_to(&f.return_to, "/admin/servers");
    if f.csrf != sess.csrf { return admin_redirect_with_msg(&return_to, "CSRF-токен не совпадает").into_response(); }
    let exists = sqlx::query_scalar::<_, i64>("SELECT 1 FROM servers WHERE id = $1 LIMIT 1").bind(id).fetch_optional(&st.db).await.ok().flatten().is_some();
    if !exists { return admin_redirect_with_msg(&return_to, "Сервер не найден").into_response(); }
    let res = sqlx::query(r#"INSERT INTO server_members(server_id, user_id, role) SELECT $1, id, 'member' FROM users WHERE NOT is_banned ON CONFLICT DO NOTHING"#).bind(id).execute(&st.db).await;
    match res {
        Ok(done) => admin_redirect_with_msg(&return_to, &format!("Готово. Добавлено: {}", done.rows_affected())).into_response(),
        Err(e) => admin_redirect_with_msg(&return_to, &format!("Ошибка: {}", e)).into_response(),
    }
}

pub(crate) async fn server_remove_user(
    State(st): State<AppState>,
    ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() { return e.into_response(); }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) { return e.into_response(); }
    let (_sid, sess) = match require_auth(&st, &headers) { Ok(v) => v, Err(r) => return r.into_response() };
    let return_to = safe_admin_return_to(&f.return_to, "/admin/servers");
    if f.csrf != sess.csrf { return admin_redirect_with_msg(&return_to, "CSRF-токен не совпадает").into_response(); }
    
    let user_id: i64 = match f.user_id.trim().parse() { 
        Ok(uid) => uid, 
        Err(_) => return admin_redirect_with_msg(&return_to, "Некорректный ID").into_response() 
    };
    
    let res = sqlx::query("DELETE FROM server_members WHERE server_id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(&st.db)
        .await;
        
    match res {
        Ok(done) => {
            if done.rows_affected() > 0 { 
                admin_redirect_with_msg(&return_to, "Пользователь удален").into_response() 
            } else { 
                admin_redirect_with_msg(&return_to, "Пользователь не найден в сервере").into_response() 
            }
        }
        Err(e) => admin_redirect_with_msg(&return_to, &format!("Ошибка: {}", e)).into_response(),
    }
}