//! Inbound triggers, Slack conversation and durable dossier handoff.
//! Slack ingress never authorizes spending; the existing app flow owns purchases.
mod activity;
pub(super) mod dossiers;
mod approvers;
mod reply;
mod slack;
mod tools;

pub(super) use tools::{slack_interactions, tool_test};
pub(super) use approvers::update as update_approver;

/// Whether this person has added Apiosk to their Slack workspace.
pub(super) async fn apiosk_bot(State(state): State<ServerState>, headers: HeaderMap) -> Response {
    let owner = match app_person(&state, &headers).await {
        Ok(p) => p.owner,
        Err(r) => return r,
    };
    if integrations::triggers::apiosk_bot_config().is_none() {
        return Json(json!({"configured": false, "connected": false})).into_response();
    }
    let composio = match client() {
        Ok(c) => c,
        Err(r) => return r,
    };
    match composio.apiosk_bot_accounts(owner).await {
        Ok(accounts) => Json(json!({"configured": true, "connected": !accounts.is_empty()})).into_response(),
        Err(e) => provider_error(e),
    }
}

pub(super) async fn apiosk_bot_connect(State(state): State<ServerState>, headers: HeaderMap) -> Response {
    let owner = match app_person(&state, &headers).await {
        Ok(p) => p.owner,
        Err(r) => return r,
    };
    let composio = match client() {
        Ok(c) => c,
        Err(r) => return r,
    };
    match composio.apiosk_bot_connect(owner).await {
        Ok(v) => Json(v).into_response(),
        Err(ComposioError::NeedsSetup) => error(StatusCode::SERVICE_UNAVAILABLE, "Apiosk for Slack is not set up on this server yet."),
        Err(e) => provider_error(e),
    }
}

use super::{app_person, failure, ServerState};
use crate::integrations::{self, Composio, ComposioError};
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive},
        IntoResponse, Response, Sse,
    },
    Json,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::{
    convert::Infallible,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

fn error(status: StatusCode, message: &str) -> Response {
    failure(status, "trigger_error", message)
}
fn db_error(e: sqlx::Error) -> Response {
    tracing::warn!(error = %e, "trigger persistence failed");
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "Trigger storage is unavailable. Try again shortly.",
    )
}
fn provider_error(e: ComposioError) -> Response {
    tracing::warn!(error = %e, "trigger provider failed");
    match e {
        ComposioError::AccountNotFound => error(StatusCode::CONFLICT, "Reconnect this Slack account and try again."),
        ComposioError::InvalidAlias => error(StatusCode::BAD_REQUEST, "Use a connection name of 1 to 100 characters."),
        _ => error(StatusCode::BAD_GATEWAY, "Slack setup could not finish. Check the connection, conversation access and Composio Slack event setup, then retry."),
    }
}
fn client() -> Result<&'static Composio, Response> {
    integrations::configured().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Composio connections are not configured.",
        )
    })
}
fn configured_secret() -> Option<String> {
    std::env::var("COMPOSIO_WEBHOOK_SECRET")
        .ok()
        .filter(|s| !s.trim().is_empty())
}
fn changed() -> &'static tokio::sync::broadcast::Sender<Uuid> {
    static CHANGED: OnceLock<tokio::sync::broadcast::Sender<Uuid>> = OnceLock::new();
    CHANGED.get_or_init(|| tokio::sync::broadcast::channel(256).0)
}
fn hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}
fn new_secret() -> String {
    format!("trg_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn bounded(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.chars().count() <= max
}

/// Slack bold markers and whitespace do not change the prefix match.
fn matches(text: &str, prefix: &str) -> bool {
    let normalize = |s: &str| {
        s.replace(['*', '_'], "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    normalize(text).starts_with(&normalize(prefix))
}

const TRIGGER_SELECT: &str = "SELECT to_jsonb(t) - 'webhook_secret_hash' AS value FROM gateway_v2.inbound_triggers t WHERE owner_id=$1 ORDER BY created_at DESC LIMIT 100";
const EVENT_FIELDS: &str = "jsonb_build_object('id',e.id::text,'trigger_id',e.trigger_id,'trigger_name',t.name,'source',t.source,'channel_name',t.channel_name,'text',e.text,'matched',e.matched,'is_test',e.is_test,'received_at',e.received_at,'activity',e.activity,'action',CASE WHEN e.matched THEN 'Hello world' ELSE NULL END)";

async fn events(pool: &PgPool, owner: Uuid, after: Option<i64>) -> Result<Vec<Value>, sqlx::Error> {
    let query = if after.is_some() {
        format!("SELECT {EVENT_FIELDS} FROM gateway_v2.inbound_trigger_events e JOIN gateway_v2.inbound_triggers t ON t.id=e.trigger_id WHERE e.owner_id=$1 AND e.id>$2 ORDER BY e.id LIMIT 200")
    } else {
        format!("SELECT {EVENT_FIELDS} FROM gateway_v2.inbound_trigger_events e JOIN gateway_v2.inbound_triggers t ON t.id=e.trigger_id WHERE e.owner_id=$1 ORDER BY e.id DESC LIMIT 50")
    };
    let query = sqlx::query_scalar(&query).bind(owner);
    if let Some(after) = after {
        query.bind(after).fetch_all(pool).await
    } else {
        query.fetch_all(pool).await
    }
}

pub(super) async fn list(State(state): State<ServerState>, headers: HeaderMap) -> Response {
    let owner = match app_person(&state, &headers).await {
        Ok(p) => p.owner,
        Err(r) => return r,
    };
    let pool = &state.engine.store.pool;
    let triggers: Vec<Value> = match sqlx::query_scalar(TRIGGER_SELECT)
        .bind(owner)
        .fetch_all(pool)
        .await
    {
        Ok(v) => v,
        Err(e) => return db_error(e),
    };
    match events(pool, owner, None).await {
        Ok(events) => Json(json!({"triggers":triggers,"cursor":events.first().and_then(|e|e["id"].as_str()).unwrap_or("0"),"events":events,
            "slack_delivery_ready": configured_secret().is_some(),"composio_configured":integrations::configured().is_some(),
            "tool_tests": tools::enabled_for(owner), "apiosk_bot_configured": integrations::triggers::apiosk_bot_config().is_some()})).into_response(),
        Err(e) => db_error(e),
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Connect {
    alias: Option<String>,
}
pub(super) async fn connect(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(source): Path<String>,
    Json(body): Json<Connect>,
) -> Response {
    let owner = match app_person(&state, &headers).await {
        Ok(p) => p.owner,
        Err(r) => return r,
    };
    let toolkit = match source.as_str() {
        "slack" => "slack",
        "teams" => "microsoft_teams",
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let composio = match client() {
        Ok(c) => c,
        Err(r) => return r,
    };
    match composio
        .trigger_connect(owner, toolkit, body.alias.as_deref())
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => provider_error(e),
    }
}

#[derive(Deserialize)]
pub(super) struct ChannelsQuery {
    account: String,
    #[serde(default)]
    cursor: String,
}
pub(super) async fn channels(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(q): Query<ChannelsQuery>,
) -> Response {
    let owner = match app_person(&state, &headers).await {
        Ok(p) => p.owner,
        Err(r) => return r,
    };
    if q.cursor.len() > 2000 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let composio = match client() {
        Ok(c) => c,
        Err(r) => return r,
    };
    match composio
        .trigger_channels(owner, &q.account, &q.cursor)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => provider_error(e),
    }
}

pub(super) async fn members(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(q): Query<ChannelsQuery>,
) -> Response {
    let owner = match app_person(&state, &headers).await {
        Ok(p) => p.owner,
        Err(r) => return r,
    };
    if q.cursor.len() > 2000 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let composio = match client() {
        Ok(c) => c,
        Err(r) => return r,
    };
    match composio.trigger_members(owner, &q.account, &q.cursor).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => provider_error(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Conversation {
    account: String,
    name: String,
    #[serde(default)]
    users: Vec<String>,
    /// A new channel's description (Slack's purpose) and the people to invite.
    #[serde(default)]
    description: String,
    #[serde(default)]
    members: Vec<String>,
}
pub(super) async fn create_conversation(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(body): Json<Conversation>,
) -> Response {
    let owner = match app_person(&state, &headers).await {
        Ok(p) => p.owner,
        Err(r) => return r,
    };
    let valid_name = !body.name.is_empty()
        && body.name.len() <= 80
        && body
            .name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    let slack_user = |s: &String| valid_id(s) && s.starts_with(['U', 'W']);
    if !valid_name
        || (!body.users.is_empty()
            && (!(2..=8).contains(&body.users.len())
                || !body.users.iter().all(slack_user)))
    {
        return error(
            StatusCode::BAD_REQUEST,
            "Use a lowercase channel name, or 2–8 Slack user IDs for a group chat.",
        );
    }
    let description = body.description.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut members = body.members;
    if description.chars().count() > 250
        || members.len() > 50
        || !members.iter().all(slack_user)
        || (!body.users.is_empty() && (!description.is_empty() || !members.is_empty()))
    {
        return error(
            StatusCode::BAD_REQUEST,
            "Use a channel description of at most 250 characters and at most 50 people to invite.",
        );
    }
    let composio = match client() {
        Ok(c) => c,
        Err(r) => return r,
    };
    // Apiosk joins every new channel so it can answer there.
    if body.users.is_empty() {
        match composio.apiosk_bot(owner, None).await {
            Ok(Some(bot)) if !bot.user.is_empty() && !members.contains(&bot.user) => members.push(bot.user),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "Apiosk bot not identified"),
        }
    }
    match composio
        .trigger_create_conversation(owner, &body.account, &body.name, &body.users, &description, &members)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => provider_error(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateTrigger {
    id: Uuid,
    name: String,
    source: String,
    match_prefix: String,
    connected_account_id: Option<String>,
    channel_id: Option<String>,
    approver_id: Option<String>,
    approval_mode: Option<approvers::ApprovalMode>,
}

pub(super) async fn create(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(body): Json<CreateTrigger>,
) -> Response {
    let owner = match app_person(&state, &headers).await {
        Ok(p) => p.owner,
        Err(r) => return r,
    };
    if !bounded(&body.name, 100)
        || body.match_prefix.chars().count() > 100
        || !["slack", "webhook", "teams"].contains(&body.source.as_str())
        || (body.approver_id.is_some() && body.source != "slack")
        || (body.source != "slack" && matches!(body.approval_mode, Some(approvers::ApprovalMode::Automatic)))
        || (!body.match_prefix.trim().is_empty()
            && body.match_prefix.replace(['*', '_'], "").trim().is_empty())
    {
        return error(StatusCode::BAD_REQUEST, "Enter a name and valid source. An optional message prefix can have up to 100 characters.");
    }
    let pool = &state.engine.store.pool;
    let mut tx = match pool.begin().await {
        Ok(t) => t,
        Err(e) => return db_error(e),
    };
    // Serialize creation and event commits per owner, including simultaneous retries.
    if let Err(e) = sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,29102026))")
        .bind(owner.to_string())
        .execute(&mut *tx)
        .await
    {
        return db_error(e);
    }
    match sqlx::query_scalar::<_,Value>("SELECT to_jsonb(t)-'webhook_secret_hash' FROM gateway_v2.inbound_triggers t WHERE id=$1 AND owner_id=$2").bind(body.id).bind(owner).fetch_optional(&mut *tx).await {
        Ok(Some(v)) => return Json(json!({"trigger":v,"replayed":true})).into_response(),
        Err(e) => return db_error(e), _ => {}
    }
    if body.source == "slack" && body.approver_id.as_deref().is_none_or(|id| id.trim().is_empty()) {
        return error(StatusCode::BAD_REQUEST, "Choose an approver for this Slack trigger, including when recording activity and alerts.");
    }
    // Older approval clients omit the mode; preserve their explicit approval intent.
    let approval_mode = body.approval_mode.unwrap_or(if body.approver_id.is_some() {
        approvers::ApprovalMode::Automatic
    } else { approvers::ApprovalMode::OnRequest });
    let count: i64 = match sqlx::query_scalar(
        "SELECT count(*) FROM gateway_v2.inbound_triggers WHERE owner_id=$1",
    )
    .bind(owner)
    .fetch_one(&mut *tx)
    .await
    {
        Ok(n) => n,
        Err(e) => return db_error(e),
    };
    if count >= 50 {
        return error(
            StatusCode::CONFLICT,
            "This account has reached the limit of 50 triggers.",
        );
    }
    let mut provider_id = None;
    let mut channel_name = None;
    let mut account_id = None;
    let mut channel_id = None;
    let mut secret = None;
    let mut approver_name = None;
    if body.source == "slack" {
        if configured_secret().is_none() {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Slack event delivery needs to be configured before this trigger can listen.",
            );
        }
        let (Some(account), Some(channel)) = (
            body.connected_account_id.as_deref(),
            body.channel_id.as_deref(),
        ) else {
            return error(
                StatusCode::BAD_REQUEST,
                "Select a Slack account and conversation.",
            );
        };
        if !valid_id(channel) {
            return error(StatusCode::BAD_REQUEST, "Invalid Slack conversation.");
        }
        let exists: bool = match sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway_v2.inbound_triggers WHERE owner_id=$1 AND connected_account_id=$2 AND channel_id=$3)").bind(owner).bind(account).bind(channel).fetch_one(&mut *tx).await { Ok(v)=>v,Err(e)=>return db_error(e) };
        if exists {
            return error(
                StatusCode::CONFLICT,
                "This conversation already has a trigger.",
            );
        }
        let composio = match client() {
            Ok(c) => c,
            Err(r) => return r,
        };
        let info = match composio.trigger_channel(owner, account, channel).await {
            Ok(v) => v,
            Err(e) => return provider_error(e),
        };
        if info["channel"]["id"] != channel
            || info["channel"]["is_archived"] == true
            || info["channel"]["is_im"] == true
        {
            return error(
                StatusCode::BAD_REQUEST,
                "Choose an active channel or group chat.",
            );
        }
        channel_name = Some(
            info["channel"]["name"]
                .as_str()
                .unwrap_or(channel)
                .to_owned(),
        );
        if let Some(user) = body.approver_id.as_deref() {
            approver_name = match approvers::resolve(owner, account, channel, user).await {
                Ok(name) => Some(name), Err(r) => return r,
            };
        }
        provider_id = match composio.trigger_subscribe(owner, account, channel).await {
            Ok(id) => Some(id),
            Err(e) => return provider_error(e),
        };
        account_id = Some(account.to_owned());
        channel_id = Some(channel.to_owned());
    } else {
        secret = Some(new_secret());
    }
    let value: Value = match sqlx::query_scalar("INSERT INTO gateway_v2.inbound_triggers AS t(id,owner_id,name,source,match_prefix,connected_account_id,channel_id,channel_name,provider_trigger_id,webhook_secret_hash,approver_id,approver_name,approval_mode) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) RETURNING to_jsonb(t)-'webhook_secret_hash'")
        .bind(body.id).bind(owner).bind(body.name.trim()).bind(&body.source).bind(body.match_prefix.trim()).bind(account_id).bind(channel_id).bind(channel_name).bind(provider_id).bind(secret.as_deref().map(hash)).bind(body.approver_id).bind(approver_name).bind(approval_mode.as_str()).fetch_one(&mut *tx).await {
            Ok(v)=>v, Err(e)=>return db_error(e)
        };
    if let Err(e) = tx.commit().await {
        return db_error(e);
    }
    let _ = changed().send(owner);
    Json(json!({"trigger":value,"webhook_secret":secret})).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Control {
    enabled: bool,
}
pub(super) async fn control(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<Control>,
) -> Response {
    let owner = match app_person(&state, &headers).await {
        Ok(p) => p.owner,
        Err(r) => return r,
    };
    if body.enabled {
        let incomplete: bool = match sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway_v2.inbound_triggers WHERE id=$1 AND owner_id=$2 AND source='slack' AND approver_id IS NULL)")
            .bind(id).bind(owner).fetch_one(&state.engine.store.pool).await {
                Ok(v) => v, Err(e) => return db_error(e),
            };
        if incomplete { return error(StatusCode::CONFLICT, "Choose an approver before resuming this Slack trigger."); }
    }
    match sqlx::query(
        "UPDATE gateway_v2.inbound_triggers SET enabled=$3 WHERE id=$1 AND owner_id=$2",
    )
    .bind(id)
    .bind(owner)
    .bind(body.enabled)
    .execute(&state.engine.store.pool)
    .await
    {
        Ok(r) if r.rows_affected() == 1 => {
            let _ = changed().send(owner);
            Json(json!({"enabled":body.enabled})).into_response()
        }
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => db_error(e),
    }
}
pub(super) async fn rotate(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let owner = match app_person(&state, &headers).await {
        Ok(p) => p.owner,
        Err(r) => return r,
    };
    let secret = new_secret();
    match sqlx::query("UPDATE gateway_v2.inbound_triggers SET webhook_secret_hash=$3 WHERE id=$1 AND owner_id=$2 AND source IN ('teams','webhook')").bind(id).bind(owner).bind(hash(&secret)).execute(&state.engine.store.pool).await {
        Ok(r) if r.rows_affected()==1 => Json(json!({"webhook_secret":secret})).into_response(),
        Ok(_) => StatusCode::NOT_FOUND.into_response(), Err(e)=>db_error(e)
    }
}

/// Signature validation uses the exact bytes received, a five-minute replay window,
/// and constant-time MAC comparison. Composio's secret is used literally (not base64-decoded).
fn verify(secret: &str, headers: &HeaderMap, body: &[u8], now: i64) -> bool {
    let get = |key| headers.get(key).and_then(|v| v.to_str().ok());
    let (Some(id), Some(timestamp), Some(signature)) = (
        get("webhook-id"),
        get("webhook-timestamp"),
        get("webhook-signature"),
    ) else {
        return false;
    };
    if id.is_empty()
        || id.len() > 200
        || timestamp
            .parse::<i64>()
            .ok()
            .is_none_or(|t| now.abs_diff(t) > 300)
    {
        return false;
    }
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(format!("{id}.{timestamp}.").as_bytes());
    mac.update(body);
    signature.split_whitespace().any(|s| {
        s.strip_prefix("v1,")
            .and_then(|s| STANDARD.decode(s).ok())
            .is_some_and(|s| mac.clone().verify_slice(&s).is_ok())
    })
}

// All ingress paths share this transaction, so retries and pause races cannot
// run the action twice. Per-owner ordering keeps SSE cursors safe across commits.
async fn record(
    pool: &PgPool,
    id: Uuid,
    owner: Uuid,
    key: &str,
    text: &str,
    test: bool,
    expected_hash: Option<&str>,
) -> Result<Value, Response> {
    if !bounded(key, 300) || text.chars().count() > 8000 || text.trim().is_empty() {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "Provide a non-empty text (up to 8,000 characters) and event ID.",
        ));
    }
    let mut tx = pool.begin().await.map_err(db_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,29102026))")
        .bind(owner.to_string())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    let row=sqlx::query("SELECT enabled,match_prefix,webhook_secret_hash,approver_id,approver_name,approval_mode FROM gateway_v2.inbound_triggers WHERE id=$1 AND owner_id=$2 FOR SHARE").bind(id).bind(owner).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or_else(||StatusCode::NOT_FOUND.into_response())?;
    if let Some(expected) = expected_hash {
        if row
            .get::<Option<String>, _>("webhook_secret_hash")
            .as_deref()
            != Some(expected)
        {
            return Err(StatusCode::UNAUTHORIZED.into_response());
        }
    }
    if !row.get::<bool, _>("enabled") {
        return Ok(json!({"accepted":false,"reason":"paused"}));
    }
    let duplicate: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway_v2.inbound_trigger_events WHERE trigger_id=$1 AND event_key=$2)").bind(id).bind(key).fetch_one(&mut *tx).await.map_err(db_error)?;
    if duplicate {
        return Ok(json!({"accepted":true,"duplicate":true}));
    }
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM gateway_v2.inbound_trigger_events WHERE trigger_id=$1 AND activity IS NULL AND received_at>now()-interval '1 minute'").bind(id).fetch_one(&mut *tx).await.map_err(db_error)?;
    if count >= 120 {
        return Err(error(
            StatusCode::TOO_MANY_REQUESTS,
            "This trigger is receiving too many events. Retry in a minute.",
        ));
    }
    let matched = matches(text, &row.get::<String, _>("match_prefix"));
    sqlx::query("INSERT INTO gateway_v2.inbound_trigger_events(trigger_id,owner_id,event_key,text,matched,is_test) VALUES($1,$2,$3,$4,$5,$6)").bind(id).bind(owner).bind(key).bind(text).bind(matched).bind(test).execute(&mut *tx).await.map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    let _ = changed().send(owner);
    Ok(
        json!({"accepted":true,"matched":matched,"action":if matched {Some("Hello world")} else {None},
            "approver_id":row.get::<Option<String>,_>("approver_id"),"approver_name":row.get::<Option<String>,_>("approver_name"),"approval_mode":row.get::<String,_>("approval_mode")}),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Incoming {
    text: String,
    event_id: String,
}
pub(super) async fn webhook(
    State(state): State<ServerState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<Incoming>,
) -> Response {
    let Some(secret) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if secret.len() > 200 || !bounded(&body.event_id, 200) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let digest = hash(secret);
    let owner:Uuid=match sqlx::query_scalar("SELECT owner_id FROM gateway_v2.inbound_triggers WHERE id=$1 AND webhook_secret_hash=$2 AND source IN ('webhook','teams')").bind(id).bind(&digest).fetch_optional(&state.engine.store.pool).await {
        Ok(Some(owner))=>owner,Ok(None)=>return StatusCode::UNAUTHORIZED.into_response(),Err(e)=>return db_error(e)
    };
    match record(
        &state.engine.store.pool,
        id,
        owner,
        &format!("webhook:{}", body.event_id),
        &body.text,
        false,
        Some(&digest),
    )
    .await
    {
        Ok(v) => Json(v).into_response(),
        Err(r) => r,
    }
}

pub(super) async fn composio_webhook(
    State(state): State<ServerState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(secret) = configured_secret() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !verify(&secret, &headers, &body, chrono::Utc::now().timestamp()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if value["type"] != "composio.trigger.message" {
        return Json(json!({"accepted":false,"reason":"unhandled_event"})).into_response();
    }
    let metadata = &value["metadata"];
    if metadata["trigger_slug"] != integrations::triggers::SLACK_TRIGGER {
        return Json(json!({"accepted":false,"reason":"unhandled_trigger"})).into_response();
    }
    let data = &value["data"];
    if data["bot_id"].as_str().is_some_and(|s| !s.is_empty())
        || data["subtype"]
            .as_str()
            .is_some_and(|s| !["", "file_share"].contains(&s))
    {
        return Json(json!({"accepted":false,"reason":"bot_or_message_update"})).into_response();
    }
    let get = |v: &Value, k: &str| v[k].as_str().map(str::to_owned).unwrap_or_default();
    let (trigger, account, user, channel, text, ts) = (
        get(metadata, "trigger_id"),
        get(metadata, "connected_account_id"),
        get(metadata, "user_id"),
        get(data, "channel"),
        get(data, "text"),
        get(data, "ts"),
    );
    if [
        trigger.as_str(),
        account.as_str(),
        user.as_str(),
        channel.as_str(),
        ts.as_str(),
    ]
    .iter()
    .any(|s| s.is_empty() || s.len() > 150)
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(owner) = user
        .strip_prefix("apiosk_user_")
        .and_then(|s| Uuid::parse_str(s).ok())
    else {
        return Json(json!({"accepted":false})).into_response();
    };
    let ids:Vec<Uuid>=match sqlx::query_scalar("SELECT id FROM gateway_v2.inbound_triggers WHERE owner_id=$1 AND provider_trigger_id=$2 AND connected_account_id=$3 AND channel_id=$4 AND source='slack'").bind(owner).bind(&trigger).bind(&account).bind(&channel).fetch_all(&state.engine.store.pool).await {Ok(v)=>v,Err(e)=>return db_error(e)};
    let mut fresh_ids = Vec::new();
    let mut reviews = Vec::new();
    for id in &ids {
        match record(
            &state.engine.store.pool,
            *id,
            owner,
            &format!("slack:{channel}:{ts}"),
            &text,
            false,
            None,
        )
        .await
        {
            Ok(v) => { if v["accepted"] == true && v["duplicate"] != true {
                fresh_ids.push(*id);
                if v["matched"] == true && v["approval_mode"] == "automatic" {
                    if let Some(user) = v["approver_id"].as_str() {
                        reviews.push((*id, user.to_owned(), v["approver_name"].as_str().unwrap_or(user).to_owned()));
                    }
                }
            } },
            Err(r) => return r,
        }
    }
    // Paused triggers and redelivered events get no reply. The reply is written
    // after the event commits, so Composio's webhook is answered without waiting.
    // Apiosk answers from its own bot in this workspace with a normal channel
    // message that tags the asker, or in the thread a question was asked in.
    // "@Apiosk" arrives as a mention of the bot's Slack user. Looked up after
    // the webhook is answered.
    if let (true, Ok(composio)) = (!fresh_ids.is_empty(), client()) {
        let pool = state.engine.store.pool.clone();
        let dossier_engine = state.engine.clone();
        let event_ts = ts.clone();
        let team = data["team_id"].as_str().filter(|s| valid_id(s)).map(str::to_owned);
        let thread = data["thread_ts"].as_str().filter(|s| !s.is_empty() && s.len() <= 150).map(str::to_owned);
        let asker = data["user"].as_str().filter(|s| valid_id(s) && s.starts_with(['U', 'W'])).map(str::to_owned);
        tokio::spawn(async move {
            // A review must use the bot in this conversation's workspace,
            // including when Composio omits team_id from the incoming event.
            let team = if reviews.is_empty() && team.is_some() { team } else {
                let info = match composio.trigger_channel(owner, &account, &channel).await {
                    Ok(info) => info,
                    Err(e) => return tracing::warn!(error = %e, "review workspace lookup failed"),
                };
                let Some(team) = info["channel"]["context_team_id"].as_str().or(info["channel"]["team_id"].as_str()) else {
                    return tracing::warn!("review workspace could not be verified");
                };
                Some(team.to_owned())
            };
            let bot = match composio.apiosk_bot(owner, team.as_deref()).await {
                Ok(bot) => bot,
                Err(e) => return tracing::warn!(error = %e, "Apiosk bot lookup failed"),
            };
            let named = bot.as_ref().is_some_and(|b| !b.user.is_empty() && text.contains(&format!("<@{}>", b.user)));
            if named || reply::mentions(&text) || thread.is_some() {
                if let Some(ref bot) = bot {
                    let root = thread.as_deref().unwrap_or(&event_ts);
                    match dossiers::handle(&dossier_engine,composio,owner,fresh_ids[0],&bot.team,&bot.account,&channel,root,&text,named || reply::mentions(&text)).await {
                        Ok(true) => return,
                        Ok(false) => {},
                        Err(error) => {
                            tracing::warn!(%error,"Slack dossier request failed");
                            let _=composio.apiosk_bot_post(owner,&bot.account,&channel,Some(root),"The dossier request could not be completed. Retry in this same thread to recover it; no new purchase has been authorized.",None).await;
                            return;
                        }
                    }
                }
            }
            if !reviews.is_empty() {
                if let Some(bot) = bot {
                    if team.as_deref() != Some(bot.team.as_str()) {
                        return tracing::warn!("review bot workspace could not be verified");
                    }
                    for (trigger, approver, name) in reviews {
                        tools::request_review(&pool, composio, owner, &bot.account, trigger, &channel, &text, &approver, &name).await;
                    }
                } else { tracing::warn!(surface = "trigger-approval", "no Apiosk bot for review request"); }
                return;
            }
            if !reply::mentions(&text) && !named {
                return;
            }
            match bot {
                Some(bot) => reply::send(composio, pool, fresh_ids, owner, bot.account, channel, thread, asker, text).await,
                None => tracing::warn!(surface = "trigger-reply", reason = "no_apiosk_bot", "trigger reply skipped"),
            }
        });
    }
    Json(json!({"accepted":!ids.is_empty()})).into_response()
}

pub(super) async fn test(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<Incoming>,
) -> Response {
    let owner = match app_person(&state, &headers).await {
        Ok(p) => p.owner,
        Err(r) => return r,
    };
    match record(
        &state.engine.store.pool,
        id,
        owner,
        &format!("test:{}", body.event_id),
        &body.text,
        true,
        None,
    )
    .await
    {
        Ok(v) => Json(v).into_response(),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
pub(super) struct StreamQuery {
    after: i64,
}
pub(super) async fn stream(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(q): Query<StreamQuery>,
) -> Response {
    let owner = match app_person(&state, &headers).await {
        Ok(p) => p.owner,
        Err(r) => return r,
    };
    if q.after < 0 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    static STREAMS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let Ok(permit) = STREAMS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(128)))
        .clone()
        .try_acquire_owned()
    else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let mut changes = changed().subscribe();
    let (sender, receiver) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(8);
    tokio::spawn(async move {
        let _permit = permit;
        let mut cursor = q.after;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(55);
        if sender
            .send(Ok(Event::default().event("ready").data("{}")))
            .await
            .is_err()
        {
            return;
        }
        loop {
            match events(&state.engine.store.pool, owner, Some(cursor)).await {
                Ok(rows) => {
                    for row in rows {
                        let Some(id) = row["id"].as_str().and_then(|s| s.parse::<i64>().ok())
                        else {
                            return;
                        };
                        if sender
                            .send(Ok(Event::default()
                                .event("trigger")
                                .id(id.to_string())
                                .data(row.to_string())))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        cursor = id;
                    }
                }
                Err(e) => {
                    tracing::warn!(error=%e,"trigger stream read failed");
                    return;
                }
            }
            tokio::select! {
                _=sender.closed()=>return,
                _=tokio::time::sleep_until(deadline)=>return,
                _=tokio::time::sleep(Duration::from_secs(1))=>{},
                result=changes.recv()=>{ if result.is_ok_and(|id| id==owner) { let _=sender.send(Ok(Event::default().event("refresh").data("{}"))).await; } }
            }
        }
    });
    Sse::new(ReceiverStream::new(receiver))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(10)))
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supplier_prefix_matches_bold_and_multiline_only_at_start() {
        assert!(matches(
            "New supplier: **Acme B.V.**\nKVK 12345678 · IBAN NL91 ABNA 0417 1643 00",
            "New supplier:"
        ));
        assert!(matches(" *NEW   SUPPLIER:* Acme B.V.", "New supplier:"));
        assert!(!matches("please say New supplier: Acme", "New supplier:"));
        assert!(!matches("New suppliers: Acme", "New supplier:"));
        assert!(matches("An entirely different message", ""));
    }
    #[test]
    fn verifies_raw_payload_rotation_and_replay_window() {
        let body = br#"{"text":"New supplier: Acme"}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"secret").unwrap();
        mac.update(b"msg_1.1000.");
        mac.update(body);
        let signature = STANDARD.encode(mac.finalize().into_bytes());
        let mut headers = HeaderMap::new();
        headers.insert("webhook-id", "msg_1".parse().unwrap());
        headers.insert("webhook-timestamp", "1000".parse().unwrap());
        headers.insert(
            "webhook-signature",
            format!("v1,invalid v1,{signature}").parse().unwrap(),
        );
        assert!(verify("secret", &headers, body, 1001));
        assert!(!verify("wrong", &headers, body, 1001));
        assert!(!verify("secret", &headers, b"tampered", 1001));
        assert!(!verify("secret", &headers, body, 1301));
        assert!(!verify("secret", &headers, body, 699));
        headers.insert("webhook-timestamp", i64::MIN.to_string().parse().unwrap());
        assert!(!verify("secret", &headers, body, 1001));
    }
}
