
#[derive(Iden)]
#[sea_query(rename = "payment_orders")]
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