//! Official Crunchbase read tools. Fixed destinations, no paid-call retries,
//! bounded replies, server-only OAuth, and best-effort public-page enrichment.
use reqwest::{Client, Response};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::time::Duration;
mod reader;
mod validation;
pub(super) use reader::enrich;
const ORIGIN: &str = "https://mcp.crunchbase.com";
const PROTOCOL: &str = "2025-11-25";
const MAX_RESPONSE: usize = 4 * 1024 * 1024;
type Result<T> = std::result::Result<T, &'static str>;

pub(super) fn valid_route(origin: &str, method: &str, path: &str) -> bool {
    origin.trim_end_matches('/') == ORIGIN && method == "POST" && tool(path).is_some()
}
fn tool(path: &str) -> Option<&'static str> {
    match path {
        "/mcp/cb_reference" => Some("cb_reference"),
        "/mcp/cb_entity_autocomplete" => Some("cb_entity_autocomplete"),
        "/mcp/cb_search_query" => Some("cb_search_query"),
        "/mcp/cb_entity_get" => Some("cb_entity_get"),
        _ => None,
    }
}
fn client(timeout: u64) -> Result<Client> {
    Client::builder().redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5)).timeout(Duration::from_secs(timeout))
        .user_agent("ApioskGateway/1.0").build().map_err(|_| "crunchbase_client_unavailable")
}
fn bearer(value: &Value) -> Result<String> {
    value["access_token"].as_str().filter(|s| !s.is_empty() && s.len() <= 16384
        && s.bytes().all(|b| (33..=126).contains(&b)))
        .map(|s| format!("Bearer {s}")).ok_or("crunchbase_reconnect_required")
}
/// Refresh under a cross-process lock so rotating refresh tokens cannot race.
/// This happens before the buyer's payment. Tokens never enter public metadata.
pub(super) async fn credentials(pool: &PgPool) -> Result<Vec<(String, String)>> {
    let mut tx = pool.begin().await.map_err(|_| "crunchbase_credentials_unavailable")?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('apiosk-crunchbase-oauth'))")
        .execute(&mut *tx).await.map_err(|_| "crunchbase_credentials_unavailable")?;
    let row: Option<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT id,decrypted_secret FROM vault.decrypted_secrets WHERE name='CRUNCHBASE_MCP_OAUTH'")
        .fetch_optional(&mut *tx).await.map_err(|_| "crunchbase_credentials_unavailable")?;
    let (id, secret) = row.ok_or("crunchbase_reconnect_required")?;
    let mut token: Value = serde_json::from_str(&secret).map_err(|_| "crunchbase_reconnect_required")?;
    let now = chrono::Utc::now().timestamp();
    if token["expires_at"].as_i64().unwrap_or(0) <= now + 90 {
        let refresh = token["refresh_token"].as_str().filter(|s| !s.is_empty()).ok_or("crunchbase_reconnect_required")?;
        let client_id = token["client_id"].as_str().filter(|s| !s.is_empty()).ok_or("crunchbase_reconnect_required")?;
        let mut form = vec![("grant_type", "refresh_token"), ("refresh_token", refresh),
            ("client_id", client_id), ("resource", ORIGIN)];
        if let Some(secret) = token["client_secret"].as_str().filter(|s| !s.is_empty()) { form.push(("client_secret", secret)); }
        let response = client(15)?.post("https://oauth.crunchbase.com/token").form(&form).send().await
            .map_err(|_| "crunchbase_refresh_uncertain")?;
        if !response.status().is_success() { return Err("crunchbase_reconnect_required"); }
        let new: Value = serde_json::from_slice(&bounded_body(response, 65536).await?)
            .map_err(|_| "crunchbase_reconnect_required")?;
        bearer(&new)?;
        if !new["token_type"].as_str().is_some_and(|s| s.eq_ignore_ascii_case("bearer")) {
            return Err("crunchbase_reconnect_required");
        }
        let expires = new["expires_in"].as_i64().filter(|n| *n > 90).ok_or("crunchbase_reconnect_required")?;
        token["access_token"] = new["access_token"].clone();
        token["expires_at"] = json!(now.saturating_add(expires));
        if new["refresh_token"].as_str().is_some_and(|s| !s.is_empty()) { token["refresh_token"] = new["refresh_token"].clone(); }
        sqlx::query("SELECT vault.update_secret($1,$2)").bind(id).bind(token.to_string())
            .execute(&mut *tx).await.map_err(|_| "crunchbase_refresh_store_failed")?;
    }
    let auth = bearer(&token)?;
    tx.commit().await.map_err(|_| "crunchbase_credentials_unavailable")?;
    Ok(vec![("Authorization".into(), auth)])
}
async fn bounded_body(mut response: Response, cap: usize) -> Result<Vec<u8>> {
    if response.content_length().is_some_and(|n| n > cap as u64) { return Err("crunchbase_response_too_large"); }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "crunchbase_response_incomplete")? {
        if bytes.len() + chunk.len() > cap { return Err("crunchbase_response_too_large"); }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn rpc_result(bytes: &[u8], id: u64) -> Result<Value> {
    fn matching(value: Value, id: u64) -> Result<Value> {
        if value["jsonrpc"] != "2.0" || value["id"] != id { return Err("crunchbase_invalid_rpc_response"); }
        if value.get("error").is_some() { return Err("crunchbase_rpc_error"); }
        value.get("result").cloned().ok_or("crunchbase_invalid_rpc_response")
    }
    if let Ok(value) = serde_json::from_slice::<Value>(bytes) { return matching(value, id); }
    let text = std::str::from_utf8(bytes).map_err(|_| "crunchbase_invalid_rpc_response")?.replace("\r\n", "\n");
    for event in text.split("\n\n") {
        let data = event.lines().filter_map(|line| line.strip_prefix("data:").map(str::trim_start)).collect::<Vec<_>>().join("\n");
        if let Ok(value) = serde_json::from_str::<Value>(&data) {
            if value["id"] == id { return matching(value, id); }
        }
    }
    Err("crunchbase_invalid_rpc_response")
}
async fn rpc(client: &Client, auth: &str, session: Option<&str>, protocol: &str, body: Value) -> Result<(Value, Option<String>)> {
    let mut request = client.post(ORIGIN).header("Authorization", auth)
        .header("Accept", "application/json, text/event-stream").header("MCP-Protocol-Version", protocol).json(&body);
    if let Some(session) = session { request = request.header("Mcp-Session-Id", session); }
    let mut response = request.send().await.map_err(|_| "crunchbase_call_uncertain")?;
    if !response.status().is_success() {
        return Err(if response.status() == 401 { "crunchbase_reconnect_required" } else { "crunchbase_upstream_error" });
    }
    let session = response.headers().get("Mcp-Session-Id").and_then(|v| v.to_str().ok()).map(str::to_owned);
    if body.get("id").is_none() { return Ok((Value::Null, session)); }
    let id = body["id"].as_u64().ok_or("crunchbase_invalid_rpc_response")?;
    // SSE connections can remain open after the result event. Stop when the
    // matching response is complete, not when the provider closes the stream.
    let streaming = response.headers().get("content-type").and_then(|h| h.to_str().ok())
        .is_some_and(|h| h.starts_with("text/event-stream"));
    if streaming {
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "crunchbase_response_incomplete")? {
            if bytes.len() + chunk.len() > MAX_RESPONSE { return Err("crunchbase_response_too_large"); }
            bytes.extend_from_slice(&chunk);
            if let Ok(result) = rpc_result(&bytes,id) { return Ok((result,session)); }
            if rpc_result(&bytes,id).err() == Some("crunchbase_rpc_error") { return Err("crunchbase_rpc_error"); }
        }
        return Err("crunchbase_response_incomplete");
    }
    let bytes = bounded_body(response, MAX_RESPONSE).await?;
    Ok((rpc_result(&bytes,id)?,session))
}
fn normalize(result: Value, name: &str) -> Result<Value> {
    if result["isError"].as_bool() == Some(true) { return Err("crunchbase_tool_error"); }
    let data = if let Some(data) = result.get("structuredContent").filter(|v| !v.is_null()) { data.clone() }
    else {
        let blocks = result["content"].as_array().ok_or("crunchbase_empty_result")?;
        let texts: Vec<&str> = blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect();
        if texts.is_empty() { return Err("crunchbase_empty_result"); }
        let text = texts.join("\n\n");
        serde_json::from_str(&text).unwrap_or_else(|_| json!({"text":text}))
    };
    // MCP transport success is not data success. The live Crunchbase schemas
    // carry provider failures in structuredContent.error (sometimes isError=false).
    if data.get("error").is_some_and(|v| !v.is_null())
        || data["response_status"].as_u64().is_some_and(|status| status >= 400) {
        return Err("crunchbase_tool_error");
    }
    Ok(json!({"source":"Crunchbase","transport":"official_mcp","tool":name,"data":data,
        "retrieved_at":chrono::Utc::now().to_rfc3339()}))
}
pub(super) async fn call(path: &str, body: &[u8], headers: &[(String,String)]) -> Result<Value> {
    let name = tool(path).ok_or("unsupported_crunchbase_route")?;
    let mut arguments: Value = serde_json::from_slice(body).map_err(|_| "invalid_crunchbase_arguments")?;
    validation::prepare(name, &mut arguments)?;
    let auth = headers.iter().find(|(k,_)| k.eq_ignore_ascii_case("authorization")).map(|(_,v)| v.as_str()).ok_or("crunchbase_reconnect_required")?;
    let client = client(45)?;
    let (init, session) = rpc(&client, auth, None, PROTOCOL, json!({"jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":PROTOCOL,"capabilities":{},"clientInfo":{"name":"Apiosk","version":"1.0"}}})).await?;
    let protocol = init["protocolVersion"].as_str().filter(|s| matches!(*s, "2025-03-26"|"2025-06-18"|"2025-11-25")).ok_or("crunchbase_unsupported_protocol")?;
    rpc(&client, auth, session.as_deref(), protocol, json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await?;
    // Exactly one tools/call. Transport failures are never automatically replayed.
    let (result, _) = rpc(&client, auth, session.as_deref(), protocol, json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":arguments}})).await?;
    normalize(result, name)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "operator-only: approved production Vault account; one free reference call"]
    async fn live_reference_uses_vault_and_the_real_adapter() {
        use std::str::FromStr;
        let database=std::env::var("APIOSK_CRUNCHBASE_ACCEPTANCE_DATABASE_URL").expect("operator database environment required");
        let options=sqlx::postgres::PgConnectOptions::from_str(&database)
            .expect("valid operator connection")
            .ssl_mode(sqlx::postgres::PgSslMode::VerifyFull)
            .ssl_root_cert("../deploy/certs/supabase-prod-ca-2021.crt");
        let pool=sqlx::postgres::PgPoolOptions::new().max_connections(1).connect_with(options).await;
        if let Err(error)=&pool {
            let kind=match error {
                sqlx::Error::Io(_)=>"io",sqlx::Error::Tls(_)=>"tls",
                sqlx::Error::Database(_)=>"database",sqlx::Error::PoolTimedOut=>"pool_timeout",_=>"configuration",
            };
            panic!("operator database connection failed: {kind}");
        }
        let pool=pool.unwrap();
        let headers=credentials(&pool).await.expect("Vault credentials available");
        let result=call("/mcp/cb_reference",br#"{"path":"entities/organization"}"#,&headers).await.expect("official adapter reference succeeds");
        assert_eq!(result["source"],"Crunchbase");
        assert_eq!(result["transport"],"official_mcp");
        assert!(result["data"]["content"].as_str().is_some_and(|s| s.contains("funding_total")));
        assert!(reader::enrich(&serde_json::to_vec(&result).unwrap()).await.is_none());
        pool.close().await;
    }
    #[test] fn routing_and_rpc_errors_are_closed() {
        assert!(valid_route(ORIGIN,"POST","/mcp/cb_entity_get"));
        for origin in ["https://mcp.crunchbase.com.evil.test","http://mcp.crunchbase.com"] { assert!(!valid_route(origin,"POST","/mcp/cb_entity_get")); }
        for path in ["/mcp/cb_list_create","/mcp/cb_entity_get?x=1","/mcp/../cb_entity_get"] { assert!(tool(path).is_none()); }
        assert!(rpc_result(br#"{"jsonrpc":"2.0","id":2,"error":{"message":"private"}}"#,2).is_err());
        assert!(rpc_result(br#"{"jsonrpc":"2.0","id":3,"result":{}}"#,2).is_err());
        assert_eq!(rpc_result(b"event: message\r\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"ok\":true}}\r\n\r\n",2).unwrap(),json!({"ok":true}));
        assert!(normalize(json!({"isError":true,"content":[{"type":"text","text":"error"}]}),"cb_entity_get").is_err());
    }
    #[test] fn preserves_company_people_and_funding_cards() {
        let data=json!({"properties":{"identifier":{"value":"Example"}},"cards":{"employees":[{"name":"Person","title":"CEO"}],"raised_funding_rounds":[{"money_raised":1000000}],"board_members":[{"name":"Advisor"}]}});
        assert_eq!(normalize(json!({"structuredContent":data}),"cb_entity_get").unwrap()["data"],data);
        assert_eq!(normalize(json!({"content":[{"type":"text","text":data.to_string()}]}),"cb_entity_get").unwrap()["data"],data);
    }
    #[test] fn structured_failures_are_not_billable_successes() {
        for data in [json!({"error":{"code":"DATA_TOKENS_EXHAUSTED","message":"quota"}}),json!({"entities":[],"response_status":403})] {
            assert!(normalize(json!({"isError":false,"structuredContent":data}),"cb_search_query").is_err());
            assert!(normalize(json!({"content":[{"type":"text","text":data.to_string()}]}),"cb_search_query").is_err());
        }
        assert!(normalize(json!({"structuredContent":{"entities":[],"error":null,"response_status":200}}),"cb_search_query").is_ok());
    }
    #[test] fn authenticated_public_samples_pass_the_runtime_contract() {
        let records:Vec<Value>=serde_json::from_str(include_str!("../../../tests/fixtures/crunchbase-mcp-live.json")).unwrap();
        assert_eq!(records.len(),9);
        for record in records {
            let name=record["name"].as_str().unwrap();
            let mut args=record["arguments"].clone();
            validation::prepare(name,&mut args).unwrap();
            let result=normalize(json!({"structuredContent":record["data"]}),name).unwrap();
            assert_eq!(result["data"],record["data"]);
        }
    }
}
