//! Connected access to the frozen Ask page's interpretation and source search.
//! Authentication and signed snapshot scope differ; discovery semantics do not.
use super::*;
use crate::{capability_intake::{Match, ParsedRequest}, search_snapshot::Snapshot, server::ask_v2::{self, Origin}};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParseReceipt {
    purpose: String,
    owner: Uuid,
    connection: Uuid,
    workspace: Option<Uuid>,
    parsed_digest: String,
}

impl ParseReceipt {
    fn issue(signer: &crate::state::Signer, principal: &crate::auth::Principal, parsed: &ParsedRequest) -> anyhow::Result<String> {
        let receipt = Self { purpose:"ask-page-parse-v1".into(), owner:principal.owner,
            connection:principal.connection.ok_or_else(|| anyhow::anyhow!("connection required"))?,
            workspace:principal.workspace, parsed_digest:crate::state::digest(parsed) };
        let payload = serde_json::to_vec(&receipt)?;
        Ok(format!("{}.{}", URL_SAFE_NO_PAD.encode(&payload), signer.search_signature(&payload)))
    }
    fn verify(signer: &crate::state::Signer, principal: &crate::auth::Principal, parsed: &ParsedRequest, token: &str) -> anyhow::Result<()> {
        anyhow::ensure!(token.len() <= 2048, "invalid parse receipt");
        let (payload, signature) = token.split_once('.').ok_or_else(|| anyhow::anyhow!("invalid parse receipt"))?;
        let payload = URL_SAFE_NO_PAD.decode(payload)?;
        signer.verify_search(&payload, signature)?;
        let receipt: Self = serde_json::from_slice(&payload)?;
        anyhow::ensure!(receipt.purpose == "ask-page-parse-v1" && receipt.owner == principal.owner
            && Some(receipt.connection) == principal.connection && receipt.workspace == principal.workspace
            && receipt.parsed_digest == crate::state::digest(parsed), "parse belongs to another request");
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Question { question: String }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Refresh { question: String, parsed_request: ParsedRequest, search_id: String }

async fn connected(state: &ServerState, headers: &HeaderMap) -> Result<AuthContext, HttpResponse> {
    let ctx = context(state, headers, &["ask:create"]).await?;
    if ctx.principal.connection.is_none() {
        return Err(problem(StatusCode::FORBIDDEN, "connection_required", "Use a connected account for this Ask request."));
    }
    state.engine.store.check_workspace(&ctx.principal).await.map_err(|error| not_found(&error))?;
    Ok(ctx)
}

fn valid_question(question: &str) -> bool { !question.is_empty() && question.encode_utf16().count() <= 16_000 }

// Each App origin has an independent response. A stale or oversized native
// snapshot must not erase healthy external sources (or conversely).
fn signed_origin(signer: &crate::state::Signer, principal: &crate::auth::Principal,
    parsed: &ParsedRequest, origin: &str, mut value: Value, prepared_version: Option<&str>) -> Result<Value, Value> {
    if origin == "apiosk" && prepared_version.is_some_and(|version| value["catalog_version"] != version) {
        return Err(json!({"origin":origin,"code":"search_refresh_required"}));
    }
    let token = Snapshot::issue_connected(signer, principal, parsed, &value)
        .map_err(|_| json!({"origin":origin,"code":"source_snapshot_unavailable"}))?;
    value[if origin == "apiosk" { "search_id" } else { "discovery_id" }] = json!(token);
    Ok(value)
}

pub async fn discover(State(state): State<ServerState>, headers: HeaderMap, body: Bytes) -> HttpResponse {
    let result = async {
        let ctx = connected(&state, &headers).await?;
        let request: Question = parse(&body)?;
        let question = request.question.trim();
        if !valid_question(question) { return Err(problem(StatusCode::BAD_REQUEST, "invalid_question", "Enter a question of up to 16,000 characters.")); }
        let Some(client) = state.engine.parser.ask_v2() else { return Err(problem(StatusCode::SERVICE_UNAVAILABLE, "parser_unavailable", "Ask is temporarily unavailable.")); };
        let Ok(_permit) = state.permits.try_acquire() else { return Err(busy()); };
        if !client.admit(&ctx.principal.owner.to_string()) || !client.admit_search(&ctx.principal.owner.to_string()) { return Err(busy()); }
        let catalog = state.engine.catalog.get().await.map_err(|error| unavailable("Ask catalog", &error))?;
        let started = Instant::now();
        // Exactly the App's first-turn model request and validation. Connected
        // planning repairs and expanded discovery never modify this parse.
        let interpretation = client.interpret_with_context(question, question, question, &[]).await
            .map_err(|error| unavailable("Ask interpretation", &error))?;
        let parsed = interpretation.parsed_request;
        let matches = crate::ask_v2::search::search(&parsed, &catalog);
        searches(&state, &ctx, question, parsed, interpretation.output.model, interpretation.attempts,
            started, Some((&matches, &catalog.version))).await
    }.await;
    result.unwrap_or_else(|response| response)
}

pub async fn refresh(State(state): State<ServerState>, headers: HeaderMap, body: Bytes) -> HttpResponse {
    let result = async {
        let ctx = connected(&state, &headers).await?;
        let request: Refresh = parse(&body)?;
        let question = request.question.trim();
        if !valid_question(question) || request.parsed_request.meta.text_sha256 != hex::encode(Sha256::digest(question.as_bytes())) {
            return Err(problem(StatusCode::BAD_REQUEST, "invalid_question", "Keep the original question and signed parse when refreshing sources."));
        }
        ParseReceipt::verify(&state.engine.signer, &ctx.principal, &request.parsed_request, &request.search_id)
            .or_else(|_| Snapshot::open_connected(&state.engine.signer, &ctx.principal, &request.parsed_request, &request.search_id).map(|_| ()))
            .map_err(|_| problem(StatusCode::CONFLICT, "source_refresh_required", "This signed search does not belong to this connection and question."))?;
        let Ok(_permit) = state.permits.try_acquire() else { return Err(busy()); };
        if state.engine.parser.ask_v2().is_some_and(|client| !client.admit_search(&ctx.principal.owner.to_string())) { return Err(busy()); }
        let model = request.parsed_request.meta.model.clone();
        searches(&state, &ctx, question, request.parsed_request, model, 0, Instant::now(), None).await
    }.await;
    result.unwrap_or_else(|response| response)
}

async fn searches(state: &ServerState, ctx: &AuthContext, question: &str, parsed: ParsedRequest,
    model: String, attempts: usize, started: Instant, prepared: Option<(&[Match], &str)>) -> Answer {
    let (mut local, external, company_context) = tokio::join!(
        ask_v2::search_catalog_with_matches(state, &parsed, Origin::Apiosk, prepared.map(|(matches, _)| matches)),
        ask_v2::search_catalog(state, &parsed, Origin::Coinbase),
        crate::subject_context::collect(&parsed),
    );
    if let Ok(value) = &mut local {
        if let Ok(catalog) = state.engine.catalog.get().await {
            value["complete_plan_available"] = json!(ask_v2::complete_local_plan(&catalog, question, &parsed, value));
        }
    }
    let mut response = merge_origins(&state.engine.signer, &ctx.principal, &parsed,
        [("apiosk", local), ("coinbase", external)], prepared.map(|(_, version)| version));
    // Preserve the authoritative parse even if neither origin can be signed.
    // The distinct purpose/type cannot pass source-snapshot validation; only
    // this adapter's refresh route accepts it.
    response["parse_id"] = json!(ParseReceipt::issue(&state.engine.signer, &ctx.principal, &parsed)
        .map_err(|error| unavailable("Ask parse receipt", &error))?);
    for (field, value) in [("question", json!(question)), ("parsed_request", json!(parsed)), ("model", json!(model)),
        ("attempts", json!(attempts)), ("company_context", json!(company_context)), ("context_status", json!("complete")),
        ("timing", json!({"parse_ms":started.elapsed().as_millis() as u64,"model_http_requests":crate::telemetry::model_http_requests()}))] {
        response[field] = value;
    }
    Ok(Json(response).into_response())
}

fn merge_origins(signer: &crate::state::Signer, principal: &crate::auth::Principal, parsed: &ParsedRequest,
    origins: [(&str, Result<Value, HttpResponse>); 2], prepared_version: Option<&str>) -> Value {
    let mut searches = serde_json::Map::new();
    let mut apis = Vec::new();
    let mut errors = Vec::new();
    let mut search_id = None;
    let mut discovery_id = None;
    let mut source_options = json!([]);
    for (origin, result) in origins {
        match result {
            Ok(value) => {
                let value = match signed_origin(signer, principal, parsed, origin, value, prepared_version) {
                    Ok(value) => value,
                    Err(error) => { errors.push(error); continue; }
                };
                if origin == "apiosk" { search_id = value["search_id"].as_str().map(str::to_owned); source_options = value["source_options"].clone(); }
                else { discovery_id = value["discovery_id"].as_str().map(str::to_owned); }
                for found in value["matches"].as_array().into_iter().flatten() {
                    for candidate in found[origin].as_array().into_iter().flatten() {
                        let mut api = candidate.clone();
                        api["requested_capability"] = found["slug"].clone();
                        api["discovery_query"] = found["query"].clone();
                        if !apis.contains(&api) { apis.push(api); }
                    }
                }
                if value["matches"].as_array().into_iter().flatten().any(|found| found["coinbase_status"] == "unavailable") {
                    errors.push(json!({"origin":origin,"code":"search_unavailable"}));
                }
                searches.insert(origin.into(), value);
            }
            Err(_) => errors.push(json!({"origin":origin,"code":"search_unavailable"})),
        }
    }
    let status = if !errors.is_empty() { "partial" } else if apis.is_empty() { "needs_input" } else { "choices" };
    json!({"status":status,"apis":apis,"searches":searches,"errors":errors,"search_id":search_id,
        "discovery_id":discovery_id,"source_options":source_options})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_origin_keeps_the_other_origin_signable_and_scope_bound() {
        let signer = crate::state::Signer::new(vec![7;32]).unwrap();
        let principal = crate::auth::Principal { owner:Uuid::new_v4(), connection:Some(Uuid::new_v4()), workspace:None };
        let parsed = crate::capability_intake::normalize(&json!({"language":"en","subjects":[],"capabilities":[{"slug":"general.conversation",
            "name":"General question","description":"General question","domain":"general","inputs":[],"outputs":[],"subject_id":null,"source_hint":null,"confidence":1}],
            "deliverable":{"format":"chat","operations":[]}}).to_string(), "What is available?", "fixture").unwrap();
        let healthy = json!({"catalog_version":"","matches":[],"source_options":[],"apiosk_sources_searched":0});
        let stale = json!({"catalog_version":"new","matches":[],"source_options":[],"apiosk_sources_searched":1});
        assert_eq!(signed_origin(&signer, &principal, &parsed, "apiosk", stale.clone(), Some("old")).unwrap_err(),
            json!({"origin":"apiosk","code":"search_refresh_required"}));
        let oversized = json!({"catalog_version":"current","matches":[{"slug":"general.info","query":"x".repeat(130 * 1024),
            "operations":[],"apiosk":[],"coinbase":[],"coinbase_status":"complete"}],"source_options":[],"apiosk_sources_searched":1});
        assert_eq!(signed_origin(&signer, &principal, &parsed, "apiosk", oversized.clone(), Some("current")).unwrap_err(),
            json!({"origin":"apiosk","code":"source_snapshot_unavailable"}));
        for (failed, version) in [(stale, "old"), (oversized.clone(), "current")] {
            let combined = merge_origins(&signer, &principal, &parsed,
                [("apiosk", Ok(failed)), ("coinbase", Ok(healthy.clone()))], Some(version));
            assert_eq!(combined["status"], "partial");
            assert!(combined["searches"]["apiosk"].is_null());
            assert!(combined["search_id"].is_null());
            assert!(combined["searches"]["coinbase"].is_object());
            assert!(combined["discovery_id"].is_string());
            assert_eq!(combined["source_options"], json!([]));
            assert_eq!(combined["errors"][0]["origin"], "apiosk");
        }
        let native = json!({"catalog_version":"current","matches":[],"source_options":[],"apiosk_sources_searched":1});
        let combined = merge_origins(&signer, &principal, &parsed,
            [("apiosk", Ok(native)), ("coinbase", Ok(oversized))], Some("current"));
        assert_eq!(combined["status"], "partial");
        assert!(combined["searches"]["apiosk"].is_object() && combined["search_id"].is_string());
        assert!(combined["searches"]["coinbase"].is_null() && combined["discovery_id"].is_null());
        assert_eq!(combined["errors"][0]["origin"], "coinbase");
        let accepted = signed_origin(&signer, &principal, &parsed, "coinbase", healthy, Some("old")).unwrap();
        let token = accepted["discovery_id"].as_str().unwrap();
        assert!(Snapshot::open_connected(&signer, &principal, &parsed, token).is_ok());
        let other = crate::auth::Principal { connection:Some(Uuid::new_v4()), ..principal };
        assert!(Snapshot::open_connected(&signer, &other, &parsed, token).is_err());
        let receipt = ParseReceipt::issue(&signer, &principal, &parsed).unwrap();
        assert!(ParseReceipt::verify(&signer, &principal, &parsed, &receipt).is_ok());
        assert!(ParseReceipt::verify(&signer, &other, &parsed, &receipt).is_err());
        assert!(Snapshot::open_connected(&signer, &principal, &parsed, &receipt).is_err(), "parse receipt is not a source snapshot");
    }
}
