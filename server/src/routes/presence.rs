use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Serialize;
use sqlx::Row;

use crate::server::AppState;
use crate::middleware::auth_guard::AuthUser;

#[derive(Serialize)]
pub struct OnlineUser {
    pub user_id: i64,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/online", get(online))
}

async fn online(
    State(st): State<AppState>,
    _me: AuthUser,
) -> impl IntoResponse {
    let rows = sqlx::query(
        "SELECT user_id FROM user_presence WHERE is_online = 1 ORDER BY user_id",
    )
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();

    let users: Vec<OnlineUser> = rows
        .into_iter()
        .map(|row| OnlineUser {
            user_id: row.get::<i64, _>("user_id"),
        })
        .collect();

    (StatusCode::OK, Json(users)).into_response()
}
