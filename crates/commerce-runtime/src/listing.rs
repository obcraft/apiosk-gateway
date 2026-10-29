use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// All supported listing types
#[derive(Debug, Clone, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "listing_type", rename_all = "snake_case")]
pub enum ListingType {
    Api,
    Skill,
    Product,
    Dataset,
    Service,
    Connector,
    /// External, already-x402-native listing that Apiosk indexes but does NOT
    /// proxy: the agent pays the provider directly (provider resource URL +
    /// provider payTo). Must never enter the proxy/settlement path.
    Federated,
}

impl std::fmt::Display for ListingType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ListingType::Api => write!(f, "api"),
            ListingType::Skill => write!(f, "skill"),
            ListingType::Product => write!(f, "product"),
            ListingType::Dataset => write!(f, "dataset"),
            ListingType::Service => write!(f, "service"),
            ListingType::Connector => write!(f, "connector"),
            ListingType::Federated => write!(f, "federated"),
        }
    }
}

/// Extended API/listing row from database
#[derive(Debug, Clone, FromRow)]
pub struct Listing {
    pub id: Uuid,
    pub slug: String,
    pub origin_url: String,
    pub wallet_address: String,
    pub status: String,
    pub owner_id: Uuid,
    pub commission_rate: bigdecimal::BigDecimal,
    pub listing_type: ListingType,
    pub is_platform_owned: bool,
    pub listing_metadata: serde_json::Value,
}

/// Skill listing details
#[derive(Debug, Clone, FromRow)]
pub struct SkillListing {
    pub id: Uuid,
    pub api_id: Uuid,
    pub endpoint_url: String,
    pub input_schema: Option<serde_json::Value>,
    pub tool_manifest: serde_json::Value,
    pub auth_type: String,
    pub auth_secret: Option<String>,
}

/// Product listing details
#[derive(Debug, Clone, FromRow)]
pub struct ProductListing {
    pub id: Uuid,
    pub api_id: Uuid,
    pub storage_type: String,
    pub storage_path: String,
    pub file_name: String,
    pub mime_type: String,
    pub file_size_bytes: Option<i64>,
    pub license_type: String,
    pub license_key: Option<String>,
    pub download_expiry_seconds: i32,
    pub max_downloads: i32,
}

/// Dataset listing details
#[derive(Debug, Clone, FromRow)]
pub struct DatasetListing {
    pub id: Uuid,
    pub api_id: Uuid,
    pub delivery_type: String,
    pub bulk_url: Option<String>,
    pub query_endpoint: Option<String>,
    pub data_schema: Option<serde_json::Value>,
    pub sample_data: Option<serde_json::Value>,
    pub record_count: Option<i64>,
    pub data_format: String,
}

/// Service listing details
#[derive(Debug, Clone, FromRow)]
pub struct ServiceListing {
    pub id: Uuid,
    pub api_id: Uuid,
    pub webhook_url: String,
    pub relay_method: String,
    pub trigger_schema: Option<serde_json::Value>,
    pub auth_type: String,
    pub auth_secret: Option<String>,
    pub timeout_ms: i32,
}

/// Connector listing details
#[derive(Debug, Clone, FromRow)]
pub struct ConnectorListing {
    pub id: Uuid,
    pub api_id: Uuid,
    pub platform: String,
    pub auth_type: String,
    pub relay_url: String,
    pub supported_actions: serde_json::Value,
    pub auth_secret: Option<String>,
}

/// Product purchase record
#[derive(Debug, Clone, FromRow)]
pub struct ProductPurchase {
    pub id: Uuid,
    pub product_id: Uuid,
    pub api_id: Uuid,
    pub buyer_wallet: String,
    pub tx_hash: String,
    pub access_token: String,
    pub download_count: i32,
    pub max_downloads: i32,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Result of dispatching to a listing type handler
#[derive(Debug)]
pub struct DispatchResult {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
    pub extra_headers: Vec<(String, String)>,
}

impl DispatchResult {
    pub fn json(status: u16, body: impl Serialize) -> Self {
        Self {
            status,
            content_type: "application/json".to_string(),
            body: serde_json::to_vec(&body).unwrap_or_default(),
            extra_headers: vec![],
        }
    }

    pub fn error(status: u16, code: &str, message: &str) -> Self {
        Self::json(
            status,
            serde_json::json!({
                "error": code,
                "message": message,
            }),
        )
    }
}
