
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use chrono::{DateTime, Utc};
use sea_query::Iden;

#[derive(Iden)]
#[iden(rename = "payment_orders")]
pub enum PaymentOrderIden {
    Table,
    Id,
    UserId,
    PlanId,
    Amount,
    Status,
    CreatedAt,
    PaidAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PaymentOrder {
    pub id: String,
    pub user_id: i64,
    pub plan_id: String,
    pub amount: i32,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub paid_at: Option<DateTime<Utc>>,
}