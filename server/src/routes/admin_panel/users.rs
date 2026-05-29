use crate::{auth, server::{AdminSession, AppState}};
use super::*;

use axum::{
    extract::{Form, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse},
};
use serde::Deserialize;
use sqlx::{Row, SqlitePool};

// =============================
// Helpers (user-only)
// =============================

pub(crate) fn normalized_user_mode(input: Option<&str>) -> &'static str {
    match input.unwrap_or("all").trim().to_ascii_lowercase().as_str() {
        "active" => "active",
        "banned" => "banned",
        "review" => "review",
        _ => "all",
    }
}

pub(crate) fn user_mode_matches(user: &UserRow, mode: &str) -> bool {
    match mode {
        "active" => !user.is_banned,
        "banned" => user.is_banned,
        "review" => user.trust_review_status == "review" || user.trust_factor < 60,
        _ => true,
    }
}

pub(crate) fn user_page_url(base_path: &str, embedded: bool, q: &str, mode: &str, user_id: Option<i64>) -> String {
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    if embedded {
        ser.append_pair("view", "users");
    }
    let q = q.trim();
    if !q.is_empty() {
        ser.append_pair("q", q);
    }
    if mode != "all" {
        ser.append_pair("mode", mode);
    }
    if let Some(user_id) = user_id {
        ser.append_pair("user_id", &user_id.to_string());
    }
    let query = ser.finish();
    if query.is_empty() { base_path.to_string() } else { format!("{base_path}?{query}") }
}

fn reason_label(reason: &str) -> &'static str {
    match reason {
        "spam" => "Спам",
        "abuse" => "Оскорбления",
        "avatar" => "Аватар",
        "username" => "Ник",
        "ads" => "Реклама",
        "scam" => "Скам",
        _ => "Другое",
    }
}

fn report_status_label(status: &str) -> &'static str {
    match status {
        "reviewed" => "Просмотрено",
        "rejected" => "Отклонено",
        _ => "Новая",
    }
}

fn cookie_consent_label(status: &str) -> &'static str {
    match status {
        "accepted" => "Принято",
        "declined" => "Отказ",
        _ => "Не выбрано",
    }
}

fn trust_review_label(status: &str) -> &'static str {
    match status {
        "review" => "На проверке",
        _ => "Чисто",
    }
}

// =============================
// Render functions (user-only)
// =============================

fn render_user_reports_html(sess: &AdminSession, reports: &[UserReportRow], return_to: &str) -> String {
    if reports.is_empty() {
        return "<div class='admin-user-emptyline'>Репортов по этому пользователю нет.</div>".to_string();
    }

    let mut out = String::new();
    for r in reports {
        let message = if r.message.trim().is_empty() {
            "Без комментария".to_string()
        } else {
            escape_html(&r.message)
        };
        let msg_id = r.message_id.map(|id| format!(" · сообщение #{id}")).unwrap_or_default();
        let actions = if r.status == "open" {
            format!(
                r#"<div class='admin-report-actions'>
  <form method='post' action='/admin/reports/{id}/status' data-ajax-report-status>
    <input type='hidden' name='csrf' value='{csrf}' />
    <input type='hidden' name='return_to' value='{return_to}' />
    <input type='hidden' name='status' value='reviewed' />
    <button type='submit' class='btn-soft'>Просмотрено</button>
  </form>
  <form method='post' action='/admin/reports/{id}/status' data-ajax-report-status>
    <input type='hidden' name='csrf' value='{csrf}' />
    <input type='hidden' name='return_to' value='{return_to}' />
    <input type='hidden' name='status' value='rejected' />
    <button type='submit' class='btn-soft'>Отклонить</button>
  </form>
</div>"#,
                id = r.id,
                csrf = escape_html(&sess.csrf),
                return_to = escape_html(return_to),
            )
        } else {
            String::new()
        };
        out.push_str(&format!(
            r#"<div class='admin-report-row'>
  <div class='admin-report-top'>
    <span class='admin-report-reason'>{reason}</span>
    <span class='admin-report-status {status_class}'>{status}</span>
  </div>
  <div class='admin-report-text'>{message}</div>
  <div class='admin-report-meta'>От: #{reporter_id} {reporter_name} · {created_at}{msg_id}</div>
  {actions}
</div>"#,
            reason = reason_label(&r.reason),
            status = report_status_label(&r.status),
            status_class = if r.status == "open" { "open" } else { "done" },
            message = message,
            reporter_id = r.reporter_id,
            reporter_name = escape_html(&r.reporter_username),
            created_at = escape_html(&fmt_admin_dt(&r.created_at)),
            msg_id = escape_html(&msg_id),
            actions = actions,
        ));
    }
    out
}

pub(crate) fn render_user_detail_card(
    sess: &AdminSession,
    user: &UserRow,
    reports: &[UserReportRow],
    current_return_to: &str,
) -> String {
    let email_html = if user.email.trim().is_empty() { "без e-mail".to_string() } else { escape_html(&user.email) };
    let initial = user.username.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_else(|| "?".to_string());
    let online_label = if user.is_online { "Онлайн" } else { "Оффлайн" };
    let online_class = if user.is_online { "online" } else { "offline" };
    let access_label = if user.is_banned { "Ограничен" } else { "Без ограничений" };
    let access_class = if user.is_banned { "banned" } else { "clear" };
    let trust_needs_review = user.trust_review_status == "review" || user.trust_factor < 60;
    let trust_class = if trust_needs_review { "review" } else { "clear" };
    let trust_label = if trust_needs_review {
        "Проверка".to_string()
    } else {
        format!("Trust {}", user.trust_factor)
    };
    let last_seen = if user.presence_updated_at.trim().is_empty() { "нет данных".to_string() } else { fmt_admin_dt(&user.presence_updated_at) };
    let avatar_html = if let Some(file_id) = user.avatar_file_id {
        format!("<img class='admin-user-avatar-img' src='/admin/profile-files/{file_id}/raw' alt='avatar' />")
    } else {
        escape_html(&initial)
    };
    let ban_reason_html = if user.is_banned && !user.ban_reason.trim().is_empty() {
        format!(
            "<div class='admin-user-section compact-ban-reason'><strong>Причина бана</strong><div class='admin-user-section-muted'>{}</div><div class='admin-report-meta'>{}</div></div>",
            escape_html(&user.ban_reason),
            if user.ban_at.trim().is_empty() { "".to_string() } else { format!("Выдан: {}", escape_html(&fmt_admin_dt(&user.ban_at))) }
        )
    } else {
        String::new()
    };
    let trust_review_html = if trust_needs_review {
        let reason = if user.trust_review_reason.trim().is_empty() {
            "Причина не указана".to_string()
        } else {
            escape_html(&user.trust_review_reason)
        };
        let at = if user.trust_review_at.trim().is_empty() {
            String::new()
        } else {
            format!("<div class='admin-report-meta'>Событие: {}</div>", escape_html(&fmt_admin_dt(&user.trust_review_at)))
        };
        format!(
            "<div class='admin-user-section compact-ban-reason'><strong>Проверка доверия</strong><div class='admin-user-section-muted'>{reason}</div>{at}</div>"
        )
    } else {
        String::new()
    };
    let main_action = if user.is_banned {
        format!(
            r#"<form method='post' action='/admin/users/{id}/unban' data-ajax-user-action>
  <input type='hidden' name='csrf' value='{csrf}' />
  <input type='hidden' name='return_to' value='{return_to}' />
  <button type='submit' class='btn-ok'>Разбанить</button>
</form>"#,
            id = user.id,
            csrf = escape_html(&sess.csrf),
            return_to = escape_html(current_return_to),
        )
    } else {
        format!(
            r#"<form method='post' action='/admin/users/{id}/ban' data-ajax-user-action>
  <input type='hidden' name='csrf' value='{csrf}' />
  <input type='hidden' name='return_to' value='{return_to}' />
  <input type='text' name='reason' class='admin-ban-reason-input' placeholder='Причина бана' maxlength='180' />
  <button type='submit' class='btn-soft'>Заблокировать</button>
</form>"#,
            id = user.id,
            csrf = escape_html(&sess.csrf),
            return_to = escape_html(current_return_to),
        )
    };

    format!(
        r#"<div class='admin-user-card' data-admin-user-detail-card data-user-id='{id}' data-user-banned='{banned}' data-user-online='{online_data}'>
  <div class='admin-user-card-head'>
    <div class='admin-user-card-ident'>
      <button type='button' class='admin-user-avatar' data-admin-user-details='{id}' data-details-url='/admin/users/{id}/details'>{avatar_html}</button>
      <div class='admin-user-card-title'>
        <div class='admin-user-name'>{username}</div>
        <div class='admin-user-sub'>ID #{id} · {email_html}</div>
        <div class='admin-user-pills'>
          <span class='admin-user-pill {online_class}'>{online_label}</span>
          <span class='admin-user-pill {access_class}'>{access_label}</span>
          <span class='admin-user-pill {trust_class}'>{trust_label}</span>
        </div>
      </div>
    </div>
    <button type='button' class='admin-user-gear' data-admin-user-details='{id}' data-details-url='/admin/users/{id}/details' title='Детали и аватар'>⚙</button>
  </div>

  <div class='admin-user-info-grid compact'>
    <div class='admin-user-info'><span>Регистрация</span><strong>{created_at}</strong></div>
    <div class='admin-user-info'><span>Последняя активность</span><strong>{last_seen}</strong></div>
    <div class='admin-user-info'><span>Cookies</span><strong>{cookie_status}</strong></div>
    <div class='admin-user-info'><span>Trust</span><strong>{trust_factor} / 100 · {review_status}</strong></div>
  </div>

  {ban_reason_html}
  {trust_review_html}

  <div class='admin-user-section'>
    <div class='admin-user-section-head'>
      <strong>Репорты</strong>
      <span>{report_count}</span>
    </div>
    <div class='admin-report-list'>{reports_html}</div>
  </div>

  <div class='admin-user-actions'>
    {main_action}
    <form method='post' action='/admin/users/{id}/purge' data-ajax-user-action>
      <input type='hidden' name='csrf' value='{csrf}' />
      <input type='hidden' name='return_to' value='{return_to}' />
      <button type='submit' class='btn-soft'>Удалить контент</button>
    </form>
    <form method='post' action='/admin/users/{id}/ban_forever' data-ajax-user-action data-danger-action='1'>
      <input type='hidden' name='csrf' value='{csrf}' />
      <input type='hidden' name='return_to' value='{return_to}' />
      <button type='submit' class='btn-danger'>Удалить аккаунт</button>
    </form>
  </div>
</div>"#,
        id = user.id,
        banned = if user.is_banned { "1" } else { "0" },
        online_data = if user.is_online { "1" } else { "0" },
        avatar_html = avatar_html,
        username = escape_html(&user.username),
        email_html = email_html,
        online_class = online_class,
        online_label = online_label,
        access_class = access_class,
        access_label = access_label,
        trust_class = trust_class,
        trust_label = escape_html(&trust_label),
        created_at = escape_html(&fmt_admin_dt(&user.created_at)),
        last_seen = escape_html(&last_seen),
        cookie_status = cookie_consent_label(&user.cookie_consent_status),
        trust_factor = user.trust_factor,
        review_status = trust_review_label(&user.trust_review_status),
        ban_reason_html = ban_reason_html,
        trust_review_html = trust_review_html,
        report_count = reports.len(),
        reports_html = render_user_reports_html(sess, reports, current_return_to),
        main_action = main_action,
        csrf = escape_html(&sess.csrf),
        return_to = escape_html(current_return_to),
    )
}

pub(crate) fn render_user_details_modal(user: &UserRow) -> String {
    let email_html = if user.email.trim().is_empty() { "без e-mail".to_string() } else { escape_html(&user.email) };
    let initial = user.username.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_else(|| "?".to_string());
    let avatar_big = if let Some(file_id) = user.avatar_file_id {
        format!(r#"<img class='admin-modal-avatar-img' src='/admin/profile-files/{file_id}/raw' alt='avatar' />"#)
    } else {
        format!("<div class='admin-modal-avatar-empty'>{}</div>", escape_html(&initial))
    };
    let avatar_actions = if let Some(file_id) = user.avatar_file_id {
        format!(
            r#"<div class='admin-modal-actions'>
  <a href='/admin/profile-files/{file_id}/raw' target='_blank' rel='noopener'>Открыть аватар</a>
  <a href='/admin/profile-files/{file_id}/raw' download>Скачать</a>
</div>"#,
        )
    } else {
        "<div class='admin-user-emptyline'>Аватар не установлен.</div>".to_string()
    };
    format!(
        r#"<div class='admin-modal-head'>
  <div>
    <div class='admin-modal-title'>{username}</div>
    <div class='admin-modal-sub'>ID #{id} · {email_html}</div>
  </div>
  <button type='button' class='admin-modal-close' data-admin-modal-close>✕</button>
</div>
<div class='admin-modal-body'>
  <div class='admin-modal-avatar'>{avatar_big}</div>
  {avatar_actions}
  <div class='admin-user-info-grid'>
    <div class='admin-user-info'><span>Регистрация</span><strong>{created_at}</strong></div>
    <div class='admin-user-info'><span>Последняя активность</span><strong>{last_seen}</strong></div>
    <div class='admin-user-info'><span>Статус</span><strong>{status}</strong></div>
    <div class='admin-user-info'><span>Бан</span><strong>{ban}</strong></div>
  </div>
</div>"#,
        username = escape_html(&user.username),
        id = user.id,
        email_html = email_html,
        avatar_big = avatar_big,
        avatar_actions = avatar_actions,
        created_at = escape_html(&fmt_admin_dt(&user.created_at)),
        last_seen = escape_html(&if user.presence_updated_at.trim().is_empty() { "нет данных".to_string() } else { fmt_admin_dt(&user.presence_updated_at) }),
        status = if user.is_online { "онлайн" } else { "оффлайн" },
        ban = if user.is_banned { "ограничен" } else { "нет" },
    )
}

pub(crate) fn render_users_panel_body(
    sess: &AdminSession,
    users: &[UserRow],
    query: &str,
    embedded: bool,
    mode: &str,
    requested_user_id: Option<i64>,
    current_return_to: &str,
    selected_reports: &[UserReportRow],
) -> String {
    let base_path = if embedded { "/admin/center" } else { "/admin/users" };
    let mode = normalized_user_mode(Some(mode));
    let filtered: Vec<&UserRow> = users.iter().filter(|u| user_mode_matches(u, mode)).collect();
    let selected = requested_user_id
        .and_then(|id| filtered.iter().copied().find(|u| u.id == id))
        .or_else(|| filtered.first().copied());
    let selected_id = selected.map(|u| u.id);

    let all_href = user_page_url(base_path, embedded, query, "all", None);
    let active_href = user_page_url(base_path, embedded, query, "active", None);
    let banned_href = user_page_url(base_path, embedded, query, "banned", None);
    let review_href = user_page_url(base_path, embedded, query, "review", None);
    let search_html = if embedded {
        format!(
            r#"<div class='admin-user-search'>
  <input type='text' data-persist-key='admin-center-users-search' data-filter-input='users' value='{qval}' placeholder='Поиск: имя / e-mail / id' />
  <button type='button' class='btn-soft' data-clear-filter='users'>Сбросить</button>
</div>"#,
            qval = escape_html(query),
        )
    } else {
        format!(
            r#"<form method='get' action='{action}' class='admin-user-search'>
  <input type='hidden' name='mode' value='{mode}' />
  <input type='text' name='q' value='{qval}' placeholder='Поиск: имя / e-mail / id' />
  <button type='submit'>Найти</button>
</form>"#,
            action = base_path,
            mode = escape_html(mode),
            qval = escape_html(query),
        )
    };

    let mut rows_html = String::new();
    if filtered.is_empty() {
        rows_html.push_str("<div class='admin-user-emptyline'>Пользователи не найдены.</div>");
    } else {
        for user in &filtered {
            let initial = user.username.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_else(|| "?".to_string());
            let row_href = user_page_url(base_path, embedded, query, mode, Some(user.id));
            let card_url = format!("/admin/users/{}/card?return_to={}", user.id, url::form_urlencoded::byte_serialize(current_return_to.as_bytes()).collect::<String>());
            let details_url = format!("/admin/users/{}/details", user.id);
            let active = if selected_id == Some(user.id) { " active" } else { "" };
            let needs_review = user.trust_review_status == "review" || user.trust_factor < 60;
            let pill_class = if user.is_banned { "banned" } else if needs_review { "review" } else if user.is_online { "online" } else { "offline" };
            let pill_text = if user.is_banned { "Бан" } else if needs_review { "Проверка" } else if user.is_online { "Онлайн" } else { "Оффлайн" };
            let avatar_html = if let Some(file_id) = user.avatar_file_id {
                format!("<img class='admin-user-row-avatar-img' src='/admin/profile-files/{file_id}/raw' alt='avatar' />")
            } else {
                escape_html(&initial)
            };
            let filter = format!("#{} {} {} {} {}", user.id, user.username.to_lowercase(), user.email.to_lowercase(), user.cookie_consent_status, user.trust_review_status);
            rows_html.push_str(&format!(
                r#"<a class='admin-user-row{active}' href='{href}' data-admin-user-row data-user-id='{id}' data-card-url='{card_url}' data-details-url='{details_url}' data-filter-item='users' data-filter='{filter}'>
  <div class='admin-user-row-avatar'>{avatar_html}</div>
  <div class='admin-user-row-main'>
    <div class='admin-user-row-name'>{username}</div>
    <div class='admin-user-row-meta'>ID #{id}</div>
  </div>
  <span class='admin-user-pill {pill_class}'>{pill_text}</span>
</a>"#,
                active = active,
                href = escape_html(&row_href),
                id = user.id,
                card_url = escape_html(&card_url),
                details_url = escape_html(&details_url),
                filter = escape_html(&filter),
                avatar_html = avatar_html,
                username = escape_html(&user.username),
                pill_class = pill_class,
                pill_text = pill_text,
            ));
        }
    }

    let detail_html = if let Some(user) = selected {
        render_user_detail_card(sess, user, selected_reports, current_return_to)
    } else {
        "<div class='admin-user-empty-detail'>Выбери пользователя слева.</div>".to_string()
    };

    format!(
        r#"<div class='admin-users-shell'>
  <div class='admin-users-titlebar'>
    <strong>Пользователи</strong>
  </div>
  <div class='admin-users-grid'>
    <aside class='admin-users-list'>
      <div class='admin-user-tabs'>
        <a href='{all_href}' class='{all_cls}'>Все</a>
        <a href='{active_href}' class='{active_cls}'>Обычные</a>
        <a href='{review_href}' class='{review_cls}'>Проверка</a>
        <a href='{banned_href}' class='{banned_cls}'>Забаненные</a>
      </div>
      {search_html}
      <div class='admin-user-list-scroll'>{rows_html}</div>
    </aside>
    <section class='admin-users-detail' data-admin-user-detail>{detail_html}</section>
  </div>
</div>"#,
        all_href = escape_html(&all_href),
        active_href = escape_html(&active_href),
        banned_href = escape_html(&banned_href),
        review_href = escape_html(&review_href),
        all_cls = if mode == "all" { "active" } else { "" },
        active_cls = if mode == "active" { "active" } else { "" },
        banned_cls = if mode == "banned" { "active" } else { "" },
        review_cls = if mode == "review" { "active" } else { "" },
        search_html = search_html,
        rows_html = rows_html,
        detail_html = detail_html,
    )
}

// =============================
// Handlers
// =============================

pub(crate) async fn users_list(
    State(st): State<AppState>,
    ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let query = q.q.clone().unwrap_or_default().trim().to_string();
    let mode = normalized_user_mode(q.mode.as_deref());
    let embed = q.embed == Some(1);
    let base_path = if embed { "/admin/center" } else { "/admin/users" };

    let body = match fetch_users(&st.db, &query, 200).await {
        Ok(list) => {
            let selected_id = q.user_id.or_else(|| list.iter().find(|u| user_mode_matches(u, mode)).map(|u| u.id));
            let fallback_return_to = user_page_url(base_path, embed, &query, mode, selected_id);
            let current_return_to = safe_admin_return_to(q.return_to.as_deref().unwrap_or(""), &fallback_return_to);
            let reports = match selected_id {
                Some(id) => fetch_user_reports(&st.db, id, 8).await.unwrap_or_default(),
                None => Vec::new(),
            };
            render_users_panel_body(
                &sess,
                &list,
                &query,
                embed,
                mode,
                selected_id,
                &current_return_to,
                &reports,
            )
        }
        Err(err) => format!(
            "<div class='card'><div class='empty-state'>Ошибка БД: {}</div></div>",
            escape_html(&format!("{}", err))
        ),
    };

    if embed {
        embedded_page("Админка • Пользователи", &body, q.msg.as_deref()).into_response()
    } else {
        page("Админка • Пользователи", &body, q.msg.as_deref()).into_response()
    }
}

pub(crate) async fn admin_user_card_fragment(
    State(st): State<AppState>,
    ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() { return e.into_response(); }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) { return e.into_response(); }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let user = match fetch_user_by_id(&st.db, id).await {
        Ok(Some(v)) => v,
        Ok(None) => return (StatusCode::NOT_FOUND, "Пользователь не найден").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Ошибка БД: {e}")).into_response(),
    };
    let reports = fetch_user_reports(&st.db, id, 8).await.unwrap_or_default();
    let fallback = user_page_url("/admin/users", false, "", "all", Some(id));
    let return_to = safe_admin_return_to(q.return_to.as_deref().unwrap_or(""), &fallback);
    Html(render_user_detail_card(&sess, &user, &reports, &return_to)).into_response()
}

pub(crate) async fn admin_user_details_fragment(
    State(st): State<AppState>,
    ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() { return e.into_response(); }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) { return e.into_response(); }
    if let Err(r) = require_auth(&st, &headers) { return r.into_response(); }

    match fetch_user_by_id(&st.db, id).await {
        Ok(Some(user)) => Html(render_user_details_modal(&user)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "Пользователь не найден").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Ошибка БД: {e}")).into_response(),
    }
}

pub(crate) async fn user_ban(
    State(st): State<AppState>,
    ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    action_user_common(st, Some(peer), headers, id, f, UserAction::Заблокировать).await
}

pub(crate) async fn user_purge_content(
    State(st): State<AppState>,
    ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    action_user_common(st, Some(peer), headers, id, f, UserAction::PurgeContent).await
}

pub(crate) async fn user_unban(
    State(st): State<AppState>,
    ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    action_user_common(st, Some(peer), headers, id, f, UserAction::Unban).await
}

pub(crate) async fn user_ban_forever(
    State(st): State<AppState>,
    ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ActionForm>,
) -> impl IntoResponse {
    action_user_common(st, Some(peer), headers, id, f, UserAction::DeleteAccount).await
}

enum UserAction {
    Заблокировать,
    Unban,
    PurgeContent,
    DeleteAccount,
}

async fn action_user_common(
    st: AppState,
    peer: Option<std::net::SocketAddr>,
    headers: HeaderMap,
    user_id: i64,
    f: ActionForm,
    act: UserAction,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, peer) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    if f.csrf != sess.csrf {
        return admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/users"), "CSRF-токен не совпадает").into_response();
    }

    let user_row = sqlx::query("SELECT username, COALESCE(email,'') AS email FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(&st.db)
        .await;

    let Ok(Some(r)) = user_row else {
        return admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/users"), "Пользователь не найден").into_response();
    };
    let username: String = r.get("username");
    let email: String = r.get("email");

    let _re = test_user_re();
    let _is_test = is_test_user(&_re, &username, &email);

    let res = match act {
        UserAction::Заблокировать => ban_user_exec(&st.db, user_id, &f.reason).await,
        UserAction::Unban => unban_user_exec(&st.db, user_id).await,
        UserAction::PurgeContent => purge_user_content_exec(&st.db, user_id).await,
        UserAction::DeleteAccount => purge_user_exec(&st.db, user_id).await,
    };

    match res {
        Ok(_) => admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/users"), "Готово").into_response(),
        Err(e) => admin_redirect_with_msg(&safe_admin_return_to(&f.return_to, "/admin/users"), &format!("Ошибка: {}", e)).into_response(),
    }
}

// =============================
// Тестовые пользователи page
// =============================

#[derive(Deserialize, Default)]
pub(crate) struct TestUsersQuery {
    msg: Option<String>,
}

pub(crate) async fn test_users_page(
    State(st): State<AppState>,
    ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<TestUsersQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    let re = test_user_re();
    let list = fetch_test_users(&st.db, &re, 1000).await;

    let mut rows = String::new();
    match list {
        Ok(users) => {
            for u in &users {
                rows.push_str(&format!(
                    r#"<tr>
<td><input type='checkbox' name='user_ids' value='{id}' /></td>
<td>#{id}</td>
<td>{username}<div class='small'>{email}</div></td>
<td>{banned}</td>
<td class='small'>{created_at}</td>
</tr>"#,
                    id = u.id,
                    username = escape_html(&u.username),
                    email = escape_html(&u.email),
                    banned = if u.is_banned { "заблокирован" } else { "" },
                    created_at = escape_html(&fmt_admin_dt(&u.created_at)),
                ));
            }

            let body = format!(
                r#"<div class='card'>
<h2>Тестовые пользователи</h2>
<div class='small'>Шаблон: <code>{re}</code> (измени LB_TEST_USER_REGEX, если нужно)</div>
<form method='post' action='/admin/test-users'>
  <input type='hidden' name='csrf' value='{csrf}' />
  <table class='table'>
  <thead><tr><th></th><th>ID</th><th>Пользователь</th><th>Статус</th><th>Создан</th></tr></thead>
  <tbody>
  {rows}
  </tbody>
  </table>
  <div style='height:10px'></div>
  <button type='submit' class='btn-danger'>Удалить выбранных</button>
</form>
</div>"#,
                re = escape_html(re.as_str()),
                csrf = escape_html(&sess.csrf),
                rows = rows
            );

            return page("Админка • Тестовые пользователи", &body, q.msg.as_deref()).into_response();
        }
        Err(e) => {
            let body = format!(
                "<div class='card'>Ошибка БД: {}</div>",
                escape_html(&format!("{}", e))
            );
            return page("Админка • Тестовые пользователи", &body, q.msg.as_deref()).into_response();
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct DeleteTestUsersForm {
    csrf: String,
    #[serde(default)]
    user_ids: Vec<i64>,
}

pub(crate) async fn test_users_delete(
    State(st): State<AppState>,
    ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Form(f): Form<DeleteTestUsersForm>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_panel_enabled() {
        return e.into_response();
    }
    if let Err(e) = require_allow_ip(&st, &headers, Some(peer)) {
        return e.into_response();
    }
    let (_sid, sess) = match require_auth(&st, &headers) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };

    if f.csrf != sess.csrf {
        return admin_redirect_with_msg("/admin/test-users", "CSRF-токен не совпадает").into_response();
    }

    if f.user_ids.is_empty() {
        return admin_redirect_with_msg("/admin/test-users", "Ничего не выбрано").into_response();
    }

    // Safety: verify they are actually test users
    let re = test_user_re();
    for id in &f.user_ids {
        let row = sqlx::query("SELECT username, COALESCE(email,'') AS email FROM users WHERE id = ?")
            .bind(*id)
            .fetch_optional(&st.db)
            .await;
        let Ok(Some(r)) = row else {
            return admin_redirect_with_msg("/admin/test-users", "Пользователь не найден").into_response();
        };
        let username: String = r.get("username");
        let email: String = r.get("email");
        if !is_test_user(&re, &username, &email) {
            return admin_redirect_with_msg("/admin/test-users", "Операция отклонена: в списке есть обычный пользователь").into_response();
        }
    }

    for id in &f.user_ids {
        if let Err(e) = purge_user_exec(&st.db, *id).await {
            return admin_redirect_with_msg("/admin/test-users", &format!("Ошибка: {}", e)).into_response();
        }
    }

    admin_redirect_with_msg("/admin/test-users", "Готово").into_response()
}

// =============================
// Destructive ops (user)
// =============================

fn clean_admin_reason(raw: &str) -> String {
    let mut out = raw.trim().chars().take(180).collect::<String>();
    if out.is_empty() {
        out = "Без указанной причины".to_string();
    }
    out
}

async fn insert_moderation_event(db: &SqlitePool, user_id: i64, kind: &str, reason: &str, details: &str) -> anyhow::Result<()> {
    let now = auth::now_iso();
    sqlx::query(
        r#"INSERT INTO moderation_events(user_id, admin_id, kind, reason, details, created_at)
           VALUES(?, NULL, ?, ?, ?, ?)"#,
    )
    .bind(user_id)
    .bind(kind)
    .bind(reason)
    .bind(details)
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

async fn ban_user_exec(db: &SqlitePool, user_id: i64, reason: &str) -> anyhow::Result<()> {
    let clean_reason = clean_admin_reason(reason);
    let affected = sqlx::query("UPDATE users SET is_banned = 1, token_version = token_version + 1 WHERE id = ?")
        .bind(user_id)
        .execute(db)
        .await?
        .rows_affected();
    if affected == 0 {
        anyhow::bail!("Пользователь не найден")
    }
    insert_moderation_event(db, user_id, "ban", &clean_reason, "admin_panel").await?;
    Ok(())
}

async fn unban_user_exec(db: &SqlitePool, user_id: i64) -> anyhow::Result<()> {
    let affected = sqlx::query("UPDATE users SET is_banned = 0, token_version = token_version + 1 WHERE id = ?")
        .bind(user_id)
        .execute(db)
        .await?
        .rows_affected();
    if affected == 0 {
        anyhow::bail!("Пользователь не найден")
    }
    insert_moderation_event(db, user_id, "unban", "Разбан через админ-панель", "admin_panel").await?;
    Ok(())
}

async fn purge_user_content_exec(db: &SqlitePool, user_id: i64) -> anyhow::Result<()> {
    use std::path::PathBuf;

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

    let _ = sqlx::query("DELETE FROM gif_assets WHERE owner_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query(
        r#"UPDATE gif_assets
           SET source_file_id = NULL
           WHERE source_file_id IN (SELECT id FROM files WHERE uploaded_by = ?)"#,
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;

    let _ = sqlx::query("DELETE FROM files WHERE uploaded_by = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query(
        r#"UPDATE user_reports
           SET message_id = NULL
           WHERE message_id IN (SELECT id FROM messages WHERE sender_id = ?)"#,
    )
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

    let _ = file_paths;
    let _ = cleanup_file_storage_orphans_db(db).await;
    for p in profile_paths {
        let _ = std::fs::remove_file(&p);
    }

    Ok(())
}

async fn purge_user_exec(db: &SqlitePool, user_id: i64) -> anyhow::Result<()> {
    use std::path::PathBuf;

    // delete owned servers first (needs separate tx because purge_server_exec uses its own tx)
    {
        let mut tx = db.begin().await?;
        let owned_servers = sqlx::query_scalar::<_, i64>("SELECT id FROM servers WHERE owner_id = ?")
            .bind(user_id)
            .fetch_all(&mut *tx)
            .await?;
        tx.commit().await?;

        for sid in owned_servers {
            purge_server_exec(db, sid).await?;
        }
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

    let _ = sqlx::query("DELETE FROM user_reports WHERE reporter_id = ? OR target_user_id = ?")
        .bind(user_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM user_suggestions WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query("DELETE FROM gif_assets WHERE owner_id = ?")
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

    let _ = sqlx::query(
        r#"UPDATE gif_assets
           SET source_file_id = NULL
           WHERE source_file_id IN (SELECT id FROM files WHERE uploaded_by = ?)"#,
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;

    let _ = sqlx::query("DELETE FROM files WHERE uploaded_by = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    let _ = sqlx::query(
        r#"UPDATE user_reports
           SET message_id = NULL
           WHERE message_id IN (SELECT id FROM messages WHERE sender_id = ?)"#,
    )
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

        let _ = sqlx::query(
            r#"UPDATE gif_assets
               SET source_file_id = NULL
               WHERE source_file_id IN (SELECT id FROM files WHERE chat_id = ?)"#,
        )
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
        anyhow::bail!("Пользователь не найден")
    }

    tx.commit().await?;

    let _ = file_paths;
    let _ = cleanup_file_storage_orphans_db(db).await;
    for p in profile_paths {
        let _ = std::fs::remove_file(&p);
    }

    Ok(())
}
