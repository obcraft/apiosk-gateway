use crate::parser::bounded_json;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Organisation API keys: server-to-server credentials an organisation owns.
/// The App's `_shared/organisation-key.ts` mints the same format.
pub const ORGANISATION_KEY_PREFIX: &str = "apk_org_live_";
/// Every permission an organisation key can carry.
pub const API_KEY_SCOPES: [&str; 5] = ["sources:read", "sources:execute", "ask:create", "ask:execute", "billing:read"];

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("unauthorized")]
    Unauthorized,
    /// A real organisation key that may not be used now. Only its holder can
    /// reach these, so naming the reason tells a guesser nothing.
    #[error("api key refused: {0:?}")]
    Refused(KeyRefusal),
    #[error("identity service unavailable")]
    Unavailable(#[source] anyhow::Error),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRefusal {
    Expired,
    Revoked,
    /// The organisation cannot spend from its main account right now.
    Unavailable,
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
/// An organisation key's own constraints. Money stays with the organisation.
#[derive(Debug, Clone)]
pub struct ApiKey {
    pub id: Uuid,
    pub organisation_id: Uuid,
    pub name: String,
    pub scopes: Vec<String>,
    pub per_request_atomic: i64,
    pub monthly_atomic: i64,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// Who is calling, whatever credential they presented. After this, execution
/// does not care whether the caller is a server with an organisation key, a
/// chatbot through MCP OAuth, or a person in the App: an organisation key is
/// a connection of the organisation's main account, so it reaches the same
/// planner, executor, ceilings and ledger. Only `api_key` adds scopes.
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub principal: Principal,
    pub api_key: Option<ApiKey>,
}
impl AuthContext {
    pub fn actor_type(&self) -> &'static str {
        if self.api_key.is_some() {
            "api_key"
        } else if self.principal.is_person() {
            "user"
        } else {
            "oauth_client"
        }
    }
    /// People and connections act with their account's full authority; an
    /// organisation key only with the scopes it was created with.
    pub fn allows(&self, scope: &str) -> bool {
        self.api_key.as_ref().is_none_or(|key| key.scopes.iter().any(|s| s == scope))
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
        Ok(self.resolve(bearer).await?.principal)
    }
    pub async fn resolve(&self, bearer: &str) -> Result<AuthContext, AuthError> {
        if bearer.is_empty() || bearer.len() > 8192 {
            return Err(AuthError::Unauthorized);
        }
        if bearer.starts_with(ORGANISATION_KEY_PREFIX) {
            return self.organisation_key(bearer).await;
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
            return Ok(AuthContext { principal: Principal {
                owner: row.get("owner_id"),
                connection: Some(row.get("connection_id")), workspace: None, }, api_key: None });
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
        Ok(AuthContext { principal: Principal {
            owner,
            connection: None, workspace: None, }, api_key: None })
    }

    /// The key's hash resolves to the organisation's main account and the
    /// key's backing connection; the secret itself never reaches Postgres.
    async fn organisation_key(&self, bearer: &str) -> Result<AuthContext, AuthError> {
        let hash = hex::encode(Sha256::digest(bearer.as_bytes()));
        let row = sqlx::query("SELECT status,key_id,organisation_id,owner_id,connection_id,name,scopes,per_call_limit_micro_usd,monthly_limit_micro_usd,expires_at FROM public.use_organisation_api_key($1)")
            .bind(hash)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| AuthError::Unavailable(error.into()))?
            .ok_or(AuthError::Unauthorized)?;
        match row.get::<String, _>("status").as_str() {
            "ok" => {}
            "expired" => return Err(AuthError::Refused(KeyRefusal::Expired)),
            "revoked" => return Err(AuthError::Refused(KeyRefusal::Revoked)),
            "unavailable" => return Err(AuthError::Refused(KeyRefusal::Unavailable)),
            _ => return Err(AuthError::Unauthorized),
        }
        let decode = |error: sqlx::Error| AuthError::Unavailable(error.into());
        Ok(AuthContext {
            principal: Principal {
                owner: row.try_get("owner_id").map_err(decode)?,
                connection: Some(row.try_get("connection_id").map_err(decode)?),
                workspace: None,
            },
            api_key: Some(ApiKey {
                id: row.try_get("key_id").map_err(decode)?,
                organisation_id: row.try_get("organisation_id").map_err(decode)?,
                name: row.try_get("name").map_err(decode)?,
                scopes: row.try_get("scopes").map_err(decode)?,
                per_request_atomic: row.try_get("per_call_limit_micro_usd").map_err(decode)?,
                monthly_atomic: row.try_get("monthly_limit_micro_usd").map_err(decode)?,
                expires_at: row.try_get("expires_at").map_err(decode)?,
            }),
        })
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

    #[test]
    fn only_organisation_keys_are_scoped() {
        let principal = Principal { owner: Uuid::new_v4(), connection: None, workspace: None };
        let person = AuthContext { principal: principal.clone(), api_key: None };
        assert!(person.allows("ask:execute"));
        assert_eq!(person.actor_type(), "user");
        let key = AuthContext {
            principal: Principal { connection: Some(Uuid::new_v4()), ..principal },
            api_key: Some(ApiKey {
                id: Uuid::new_v4(), organisation_id: Uuid::new_v4(), name: "Nomi".into(),
                scopes: vec!["sources:read".into()], per_request_atomic: 1, monthly_atomic: 1,
                expires_at: chrono::Utc::now(),
            }),
        };
        assert!(key.allows("sources:read"));
        assert!(!key.allows("sources:execute"));
        assert_eq!(key.actor_type(), "api_key");
        assert!(API_KEY_SCOPES.contains(&"billing:read"));
        // Never confused with a person's legacy `apk_live_` agent key.
        assert!(!ORGANISATION_KEY_PREFIX.starts_with("apk_live_") && !"apk_live_x".starts_with(ORGANISATION_KEY_PREFIX));
    }

    #[tokio::test]
    async fn auth_outage_is_retryable_not_unauthorized() {
        let (auth, server) = auth_with(StatusCode::SERVICE_UNAVAILABLE).await;
        assert!(matches!(auth.authenticate("session-token").await, Err(AuthError::Unavailable(_))));
        server.abort();
    }
}
