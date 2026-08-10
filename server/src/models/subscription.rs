
#[derive(Iden)]
#[sea_query(rename = "subscriptions")]
pub enum SubscriptionIden {
    Table,
    Id,
    UserId,
    PlanId,
    ExpiresAt,
    CreatedAt,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Subscription {
    pub id: i64,
    pub user_id: i64,
    pub plan_id: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}