use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct RegisterApiRequest {
    pub name: String,
    pub slug: String,
    pub endpoint_url: String,
    pub price_usd: f64,
    pub description: String,
    pub owner_wallet: String,
    #[serde(default = "crate::api_management::helpers::default_category")]
    pub category: String,
    #[serde(default = "crate::api_management::helpers::default_listing_metadata")]
    pub listing_metadata: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct RegisterApiResponse {
    pub success: bool,
    pub api_id: String,
    pub slug: String,
    pub gateway_url: String,
    pub status: String,
    pub verified: bool,
    pub health_check_passed: bool,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateApiRequest {
    pub owner_wallet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listing_metadata: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct UpdateApiResponse {
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct MyApisQuery {
    pub wallet: String,
}

#[derive(Debug, Serialize)]
pub struct MyApisResponse {
    pub apis: Vec<CommunityApiInfo>,
    pub total_earnings_usd: f64,
}

#[derive(Debug, Serialize)]
pub struct CommunityApiInfo {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub endpoint_url: String,
    pub price_usd: f64,
    pub active: bool,
    pub verified: bool,
    pub created_at: String,
    pub total_requests: i64,
    pub total_earned_usd: f64,
    pub pending_withdrawal_usd: f64,
}

#[derive(Debug, Deserialize)]
pub struct DeleteApiQuery {
    pub wallet: String,
}

#[derive(Debug, Serialize)]
pub struct DeleteApiResponse {
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEndpointDocsRequest {
    pub owner_wallet: String,
    pub method: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_body: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_example: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_example: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct UpdateEndpointDocsResponse {
    pub success: bool,
    pub message: String,
}
