use crate::parser::bounded_json;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("identity service unavailable")]
    Unavailable(#[source] anyhow::Error),
}

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
    pub async fn authenticate(&self, bearer: &str) -> Result<Principal, AuthError> {
        if bearer.is_empty() || bearer.len() > 8192 {
            return Err(AuthError::Unauthorized);
        }
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
                .await
                .map_err(|error| AuthError::Unavailable(error.into()))?
                .ok_or(AuthError::Unauthorized)?;
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
            .await
            .map_err(|error| AuthError::Unavailable(error.into()))?;
        if matches!(response.status(), reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN) {
            return Err(AuthError::Unauthorized);
        }
        if !response.status().is_success() {
            return Err(AuthError::Unavailable(anyhow::anyhow!(
                "Supabase Auth returned {}",
                response.status()
            )));
        }
        let user = bounded_json(response, 65536)
            .await
            .map_err(AuthError::Unavailable)?;
        let owner = user["id"]
            .as_str()
            .ok_or_else(|| AuthError::Unavailable(anyhow::anyhow!("Supabase Auth response has no user id")))?
            .parse()
            .map_err(|error| AuthError::Unavailable(anyhow::Error::new(error)))?;
        Ok(Principal {
            owner,
            connection: None, workspace: None, })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::StatusCode, response::IntoResponse, routing::get, Router};
    use sqlx::postgres::PgPoolOptions;

    async fn auth_with(status: StatusCode) -> (Auth, tokio::task::JoinHandle<()>) {
        let app = Router::new().route(
            "/auth/v1/user",
            get(move || async move { (status, "{}").into_response() }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/postgres")
            .unwrap();
        (
            Auth {
                pool,
                http: reqwest::Client::new(),
                supabase_url: format!("http://{address}"),
                publishable_key: "fixture".into(),
            },
            server,
        )
    }

    #[tokio::test]
    async fn rejected_session_is_unauthorized() {
        let (auth, server) = auth_with(StatusCode::UNAUTHORIZED).await;
        assert!(matches!(auth.authenticate("session-token").await, Err(AuthError::Unauthorized)));
        server.abort();
    }

    #[tokio::test]
    async fn auth_outage_is_retryable_not_unauthorized() {
        let (auth, server) = auth_with(StatusCode::SERVICE_UNAVAILABLE).await;
        assert!(matches!(auth.authenticate("session-token").await, Err(AuthError::Unavailable(_))));
        server.abort();
    }
}
