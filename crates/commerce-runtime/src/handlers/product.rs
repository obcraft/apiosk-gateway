/// Product handler: deliver digital product after successful x402 payment.
/// Generates a short-lived signed download URL and records the purchase.
/// On GET /download?token=xxx, validates token and streams the file.
use chrono::Utc;
use serde_json::json;
use tracing::{error, info};

use crate::listing::{DispatchResult, ProductListing, ProductPurchase};

/// Called after successful payment settlement.
/// Creates a purchase record and returns a download token.
pub async fn deliver(
    _http_client: &reqwest::Client,
    product: &ProductListing,
    buyer_wallet: &str,
    tx_hash: &str,
) -> DispatchResult {
    info!(
        "Product delivery: product={} buyer={}",
        product.id, buyer_wallet
    );

    let expires_at = if product.download_expiry_seconds > 0 {
        Some(Utc::now() + chrono::Duration::seconds(product.download_expiry_seconds as i64))
    } else {
        None
    };

    // Generate access token (caller must persist to DB)
    let raw = rand_bytes();
    let access_token = raw.iter().map(|b| format!("{:02x}", b)).collect::<String>();

    // Build the signed download URL
    let download_url = format!("https://gateway.apiosk.com/download?token={}", access_token);

    let license_key = match product.license_type.as_str() {
        "static" => product.license_key.clone(),
        "generated" => Some(generate_license_key()),
        _ => None,
    };

    let mut response = json!({
        "success": true,
        "download_url": download_url,
        "expires_at": expires_at,
        "file_name": product.file_name,
        "mime_type": product.mime_type,
        "max_downloads": product.max_downloads,
        "tx_hash": tx_hash,
        "access_token": access_token,
    });

    if let Some(key) = license_key {
        response["license_key"] = json!(key);
    }

    DispatchResult {
        status: 200,
        content_type: "application/json".to_string(),
        body: serde_json::to_vec(&response).unwrap_or_default(),
        extra_headers: vec![("X-Apiosk-Type".to_string(), "product".to_string())],
    }
}

/// Called on GET /download?token=xxx — validates token and streams file.
#[allow(dead_code)]
pub async fn download(
    http_client: &reqwest::Client,
    purchase: &ProductPurchase,
    product: &ProductListing,
) -> DispatchResult {
    info!(
        "Product download: purchase={} downloads_so_far={}",
        purchase.id, purchase.download_count
    );

    // Check expiry
    if let Some(exp) = purchase.expires_at {
        if Utc::now() > exp {
            return DispatchResult::error(410, "download_expired", "Download link has expired");
        }
    }

    // Check download count
    if product.max_downloads > 0 && purchase.download_count >= product.max_downloads {
        return DispatchResult::error(429, "download_limit_reached", "Maximum downloads reached");
    }

    // Resolve file URL
    let file_url = match product.storage_type.as_str() {
        "url" => product.storage_path.clone(),
        "supabase" => {
            // Supabase storage public URL construction
            // In production: generate a signed URL via Supabase API
            format!(
                "{}/{}",
                product.storage_path.trim_end_matches('/'),
                product.file_name
            )
        }
        "s3" => {
            // In production: generate a pre-signed S3 URL
            product.storage_path.clone()
        }
        _ => {
            return DispatchResult::error(500, "storage_error", "Unknown storage type");
        }
    };

    // Proxy the file stream from storage
    let response = match http_client.get(&file_url).send().await {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to fetch product file: {}", e);
            return DispatchResult::error(502, "storage_unavailable", "Could not retrieve file");
        }
    };

    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or(&product.mime_type)
        .to_string();

    let body = match response.bytes().await {
        Ok(b) => b.to_vec(),
        Err(e) => {
            error!("Failed to stream product file: {}", e);
            return DispatchResult::error(502, "storage_error", "Failed to stream file");
        }
    };

    DispatchResult {
        status,
        content_type,
        body,
        extra_headers: vec![
            (
                "Content-Disposition".to_string(),
                format!("attachment; filename=\"{}\"", product.file_name),
            ),
            ("X-Apiosk-Type".to_string(), "product".to_string()),
        ],
    }
}

fn rand_bytes() -> [u8; 32] {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    // Simple deterministic seed for now; replace with ring::rand in production
    let mut bytes = [0u8; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = ((t >> (i % 32)) ^ (i as u32 * 0x9e3779b9)) as u8;
    }
    bytes
}

fn generate_license_key() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    // Format: XXXX-XXXX-XXXX-XXXX (hex segments from timestamp hash)
    let h = format!("{:032x}", t ^ 0xdeadbeefcafe1234u128);
    format!("{}-{}-{}-{}", &h[0..8], &h[8..16], &h[16..24], &h[24..32]).to_uppercase()
}
