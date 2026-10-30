//! Per-trigger review routing for every authenticated owner, separate from Ask.
use super::*;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ApprovalMode { OnRequest, Automatic }

impl ApprovalMode {
    pub(super) fn as_str(self) -> &'static str {
        match self { Self::OnRequest => "on_request", Self::Automatic => "automatic" }
    }
}

pub(super) async fn resolve(owner: Uuid, account: &str, channel: &str, user: &str) -> Result<String, Response> {
    if !(3..=24).contains(&user.len()) || !user.starts_with(['U', 'W']) || !user.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) {
        return Err(error(StatusCode::BAD_REQUEST, "Select an active Slack teammate."));
    }
    let composio = client()?;
    let info = composio.trigger_channel(owner, account, channel).await.map_err(provider_error)?;
    let team = info["channel"]["context_team_id"].as_str().or(info["channel"]["team_id"].as_str())
        .ok_or_else(|| error(StatusCode::BAD_GATEWAY, "Slack's workspace could not be verified."))?;
    match composio.apiosk_bot(owner, Some(team)).await.map_err(provider_error)? {
        Some(bot) if bot.team == team => {},
        _ => return Err(error(StatusCode::CONFLICT, "Add Apiosk to this Slack workspace before selecting an approver.")),
    }
    let mut cursor = String::new();
    let mut seen = std::collections::HashSet::new();
    for _ in 0..25 {
        let page = composio.trigger_members(owner, account, &cursor).await.map_err(provider_error)?;
        if let Some(member) = page["members"].as_array().into_iter().flatten().find(|m| m["id"] == user) {
            let name: String = member["name"].as_str().unwrap_or(user).chars().filter(|c| !c.is_control()).take(120).collect();
            return Ok(if name.trim().is_empty() { user.to_owned() } else { name });
        }
        cursor = page["next_cursor"].as_str().unwrap_or_default().to_owned();
        if cursor.is_empty() { return Err(error(StatusCode::BAD_REQUEST, "That approver is no longer an active teammate in this workspace.")); }
        if !seen.insert(cursor.clone()) { break; }
    }
    Err(error(StatusCode::BAD_GATEWAY, "Slack's teammates could not be fully verified. Try again."))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::server) struct Update {
    approver_id: Option<String>,
    approval_mode: Option<ApprovalMode>,
}

pub(in crate::server) async fn update(State(state): State<ServerState>, headers: HeaderMap, Path(id): Path<Uuid>, Json(body): Json<Update>) -> Response {
    let owner = match app_person(&state, &headers).await { Ok(p) => p.owner, Err(r) => return r };
    let pool = &state.engine.store.pool;
    let connection: Option<(String,String)> = match sqlx::query_as("SELECT connected_account_id,channel_id FROM gateway_v2.inbound_triggers WHERE id=$1 AND owner_id=$2 AND source='slack'")
        .bind(id).bind(owner).fetch_optional(pool).await { Ok(v) => v, Err(e) => return db_error(e) };
    let Some((account,channel)) = connection else { return StatusCode::NOT_FOUND.into_response(); };
    let name = match body.approver_id.as_deref() {
        Some(user) => match resolve(owner, &account, &channel, user).await { Ok(name) => Some(name), Err(r) => return r },
        None => return error(StatusCode::BAD_REQUEST, "Choose an approver for this Slack trigger. You can change the teammate, but an approver is required for both actions."),
    };
    let result: Result<Option<Value>,_> = sqlx::query_scalar("UPDATE gateway_v2.inbound_triggers t SET approver_id=$3,approver_name=$4,approval_mode=coalesce($5,approval_mode) WHERE id=$1 AND owner_id=$2 RETURNING to_jsonb(t)-'webhook_secret_hash'")
        .bind(id).bind(owner).bind(body.approver_id).bind(name).bind(body.approval_mode.map(ApprovalMode::as_str)).fetch_optional(pool).await;
    match result {
        Ok(Some(trigger)) => { let _ = changed().send(owner); Json(json!({"trigger":trigger})).into_response() },
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => db_error(e),
    }
}
