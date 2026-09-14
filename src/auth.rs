use crate::parser::bounded_json;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct Principal {
    pub owner: Uuid,
    pub connection: Option<Uuid>,
    pub workspace: Option<Uuid>,
}
impl Principal {
    pub fn is_person(&self) -> bool {
        self.connection.is_none()
    }
}
pub struct Auth {
    pub pool: PgPool,
    pub http: reqwest::Client,
    pub supabase_url: String,
    pub publishable_key: String,
}
impl Auth {
    pub async fn authenticate(&self, bearer: &str) -> anyhow::Result<Principal> {
        anyhow::ensure!(!bearer.is_empty() && bearer.len() <= 8192, "unauthorized");
        if bearer.starts_with("apk_access_") || bearer.starts_with("apk_live_") {
            let hash = hex::encode(Sha256::digest(bearer.as_bytes()));
            let query = if bearer.starts_with("apk_access_") {
                "SELECT owner_id,connection_id FROM public.use_agent_token($1) WHERE status='ok'"
            } else {
                "SELECT owner_id,connection_id FROM public.use_agent_connection($1)"
            };
            let row = sqlx::query(query)
                .bind(hash)
                .fetch_optional(&self.pool)
                .await?
                .ok_or_else(|| anyhow::anyhow!("unauthorized"))?;
            return Ok(Principal {
                owner: row.get("owner_id"),
                connection: Some(row.get("connection_id")), workspace: None, });
        }
        // Verify a session with Auth, never decode an unverified JWT for owner.
        let response = self
            .http
            .get(format!("{}/auth/v1/user", self.supabase_url))
            .header("apikey", &self.publishable_key)
            .bearer_auth(bearer)
            .send()
            .await?
            .error_for_status()?;
        let user = bounded_json(response, 65536).await?;
        let owner = user["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("unauthorized"))?
            .parse()?;
        Ok(Principal {
            owner,
            connection: None, workspace: None, })
    }
}
