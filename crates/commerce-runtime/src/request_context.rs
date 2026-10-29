use sqlx::FromRow;
use uuid::Uuid;

pub const SESSION_ID_HEADER: &str = "x-apiosk-session-id";
pub const REQUEST_ID_HEADER: &str = "x-request-id";

#[derive(Debug, Clone, FromRow)]
pub struct PresenceSessionContext {
    pub session_id: Uuid,
    pub client_id: String,
    pub managed_wallet_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct RequestContext {
    pub request_id: Uuid,
    pub presence: Option<PresenceSessionContext>,
}
