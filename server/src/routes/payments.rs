use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use sqlx::Row;
use std::collections::HashMap;

use crate::server::AppState;

pub struct MonetaConfig {
    pub mnt_id: String,
    pub mnt_signature: String,
    pub test_mode: bool,
}

impl MonetaConfig {
    pub fn from_env() -> Self {
        Self {
            mnt_id: std::env::var("MONETA_MNT_ID").unwrap_or_default(),
            mnt_signature: std::env::var("MONETA_MNT_SIGNATURE").unwrap_or_default(),
            test_mode: std::env::var("MONETA_TEST_MODE")
                .ok()
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false),
        }
    }
}

#[derive(Deserialize)]
pub struct CreatePaymentRequest {
    pub plan_id: String,
    pub mode: String,
    pub payment_method: String,
    pub gift_to: Option<String>,
    pub server_id: Option<i64>,
}

#[derive(Serialize)]
pub struct CreatePaymentResponse {
    pub payment_url: String,
}

// Активация подписки
async fn activate_subscription(user_id: i64, plan_id: &str, db: &sqlx::PgPool) {
    let duration_days = 30;
    let now = chrono::Utc::now();
    let expires_at = now + chrono::Duration::days(duration_days);

    let existing = sqlx::query(
        "SELECT id FROM subscriptions WHERE user_id = $1 AND expires_at > NOW()",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    if let Some(row) = existing {
        let sub_id: i64 = row.get(0);
        sqlx::query("UPDATE subscriptions SET expires_at = $1 WHERE id = $2")
            .bind(&expires_at)
            .bind(sub_id)
            .execute(db)
            .await
            .ok();
    } else {
        sqlx::query(
            "INSERT INTO subscriptions (user_id, plan_id, expires_at) VALUES ($1, $2, $3)",
        )
        .bind(user_id)
        .bind(plan_id)
        .bind(&expires_at)
        .execute(db)
        .await
        .ok();
    }
}

async fn create_payment(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(payload): Json<CreatePaymentRequest>,
) -> Result<Json<CreatePaymentResponse>, StatusCode> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let (username, token_version) =
        crate::auth::decode_username(token).map_err(|_| StatusCode::UNAUTHORIZED)?;

    let user_row = sqlx::query(
        "SELECT id, token_version, is_banned FROM users WHERE username = $1 LIMIT 1"
    )
    .bind(&username)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::UNAUTHORIZED)?;

    let user_id: i64 = user_row.get(0);
    let db_token_version: i64 = user_row.get(1);
    let is_banned: bool = user_row.get(2);

    if db_token_version != token_version || is_banned {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let config = MonetaConfig::from_env();
    if config.mnt_id.is_empty() || config.mnt_signature.is_empty() {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    let price = match payload.plan_id.as_str() {
        "basic" => 249,
        "premium" => 349,
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let order_id = uuid::Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO payment_orders (id, user_id, plan_id, amount, status, created_at) VALUES ($1, $2, $3, $4, 'pending', NOW())",
    )
    .bind(&order_id)
    .bind(user_id)
    .bind(&payload.plan_id)
    .bind(price)
    .execute(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let amount = price.to_string();
    let description = format!("Подписка {} #{}", payload.plan_id, order_id);

    let signature_string = format!(
        "{}{}{}{}{}{}",
        config.mnt_id,
        order_id,
        amount,
        "RUB",
        description,
        config.mnt_signature
    );
    let mut hasher = Sha1::new();
    hasher.update(signature_string.as_bytes());
    let signature = hex::encode(hasher.finalize());

    let mut url = format!(
        "https://www.payanyway.ru/assistant.htm?MNT_ID={}&MNT_TRANSACTION_ID={}&MNT_AMOUNT={}&MNT_CURRENCY_CODE=RUB&MNT_DESCRIPTION={}&MNT_SIGNATURE={}",
        config.mnt_id,
        order_id,
        amount,
        urlencoding::encode(&description),
        signature
    );
    if config.test_mode {
        url.push_str("&MNT_TEST_MODE=1");
    }

    Ok(Json(CreatePaymentResponse { payment_url: url }))
}

async fn payment_callback(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let config = MonetaConfig::from_env();

    let check_string = format!(
        "{}{}{}{}{}{}",
        params.get("MNT_ID").unwrap_or(&"".to_string()),
        params.get("MNT_TRANSACTION_ID").unwrap_or(&"".to_string()),
        params.get("MNT_AMOUNT").unwrap_or(&"".to_string()),
        params.get("MNT_CURRENCY_CODE").unwrap_or(&"".to_string()),
        params.get("MNT_SUBSCRIBER_CODE").unwrap_or(&"".to_string()),
        config.mnt_signature,
    );
    let mut hasher = Sha1::new();
    hasher.update(check_string.as_bytes());
    let expected_signature = hex::encode(hasher.finalize());

    let mnt_signature = params.get("MNT_SIGNATURE").cloned().unwrap_or_default();
    if mnt_signature != expected_signature {
        return (StatusCode::BAD_REQUEST, "Invalid signature");
    }

    let transaction_id = params.get("MNT_TRANSACTION_ID").cloned().unwrap_or_default();

    let order = sqlx::query("SELECT user_id, plan_id, status FROM payment_orders WHERE id = $1")
        .bind(&transaction_id)
        .fetch_optional(&state.db)
        .await;

    match order {
        Ok(Some(row)) => {
            let status: String = row.get(2);
            if status == "paid" {
                return (StatusCode::OK, "Already processed");
            }
            let user_id: i64 = row.get(0);
            let plan_id: String = row.get(1);

            activate_subscription(user_id, &plan_id, &state.db).await;

            sqlx::query("UPDATE payment_orders SET status = 'paid' WHERE id = $1")
                .bind(&transaction_id)
                .execute(&state.db)
                .await
                .ok();
            (StatusCode::OK, "OK")
        }
        Ok(None) => (StatusCode::OK, "Order not found"),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/create", post(create_payment))
        .route("/callback", axum::routing::get(payment_callback))
}