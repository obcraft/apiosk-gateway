//! Owner-only test tools for the Slack agent's responses. Each one posts as the
//! owner's Apiosk bot with labelled test data, so every message can be reviewed before
//! any paid call: nothing here screens, quotes, buys or monitors.
//! Response activity and approval decisions are saved in the owner's feed.
use super::{
    app_person, client, db_error, error, reply,
    slack::{self, esc},
    ServerState,
};
use crate::integrations::{Composio, ComposioError};
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

const NOTE: &str = "🧪 Test · sample data, no real checks and no charges";
const TEST_KVK: &str = "00000000";

/// `TRIGGER_TOOL_TEST_OWNER_IDS`, comma-separated owner UUIDs. A partly
/// malformed setting grants nobody access. Release builds always disable these tools.
pub(super) fn enabled_for(owner: Uuid) -> bool {
    cfg!(debug_assertions) && crate::recurring::parse_allowed_owners(std::env::var("TRIGGER_TOOL_TEST_OWNER_IDS").ok().as_deref()).contains(&owner)
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::server) enum Frequency {
    Daily,
    Weekly,
    Monthly,
}

#[derive(Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub(in crate::server) enum ToolTest {
    Chat { message: String },
    ScreenCompany { company: String, signals: bool },
    RequestApproval { company: String, approver: String, #[serde(default)] approver_name: Option<String>, signals: bool },
    CreateMonitor { company: String, frequency: Frequency, negative_news: bool },
}

fn valid_company(s: &str) -> bool {
    !s.trim().is_empty() && s.chars().count() <= 120 && !s.chars().any(char::is_control)
}
fn valid_user(s: &str) -> bool {
    (3..=24).contains(&s.len()) && s.starts_with(['U', 'W']) && s.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}
impl ToolTest {
    fn valid(&self) -> bool {
        match self {
            Self::Chat { message } => !message.trim().is_empty() && message.chars().count() <= 2000,
            Self::ScreenCompany { company, .. } | Self::CreateMonitor { company, .. } => valid_company(company),
            Self::RequestApproval { company, approver, approver_name, .. } => valid_company(company) && valid_user(approver)
                && approver_name.as_ref().is_none_or(|name| valid_company(name)),
        }
    }
}

fn md(text: impl Into<String>) -> Value {
    json!({"type": "mrkdwn", "text": text.into()})
}
fn section(text: impl Into<String>) -> Value {
    json!({"type": "section", "text": md(text)})
}
fn note() -> Value {
    json!({"type": "context", "elements": [md(NOTE)]})
}
fn dossier_url() -> String {
    let origin = std::env::var("APP_ORIGIN").unwrap_or_else(|_| "https://app.apiosk.com".into());
    format!("{}/monitoring", origin.trim_end_matches('/'))
}
fn link_button(text: &str) -> Value {
    json!({"type": "button", "action_id": "apiosk_open_dossier", "text": {"type": "plain_text", "text": text}, "url": dossier_url()})
}

/// The company-check signal kinds, in the snapshot's own categories.
fn findings(company: &str, signals: bool) -> Vec<String> {
    if !signals {
        return Vec::new();
    }
    vec![
        "🟠 *Sanctions lists:* possible name match; manual review required".into(),
        "🔵 *Business register:* registered address changed since the last check".into(),
        format!("📰 *News:* “Sample article about {}” (test source)", esc(company)),
    ]
}
fn profile(findings: &[String]) -> String {
    match findings.len() {
        0 => "🟢 No issues identified".into(),
        n => format!("🟠 Review needed · {n} findings"),
    }
}

fn screening(company: &str, signals: bool) -> (String, Value) {
    let found = findings(company, signals);
    let mut blocks = vec![
        json!({"type": "header", "text": {"type": "plain_text", "text": format!("Screening: {}", company.trim())}}),
        json!({"type": "section", "fields": [md(format!("*KVK*\n{TEST_KVK}")), md(format!("*Result*\n{}", profile(&found)))]}),
        section("*Checks performed*\n✅ Business register (KVK)   ✅ Sanctions lists   ✅ News from the last 7 days"),
    ];
    blocks.push(section(if found.is_empty() {
        "No findings requiring attention.".to_owned()
    } else {
        format!("*Findings*\n{}", found.join("\n"))
    }));
    blocks.push(json!({"type": "actions", "elements": [link_button("Open report")]}));
    blocks.push(note());
    (format!("Screening for {}: {}", esc(company), profile(&found)), Value::Array(blocks))
}

fn approval_card(company: &str, channel: &str, signals: bool, value: &str) -> (String, Value) {
    let found = findings(company, signals);
    let button = |id: &str, text: &str, style: &str| {
        json!({"type": "button", "action_id": id, "style": style, "value": value, "text": {"type": "plain_text", "text": text}})
    };
    let blocks = json!([
        {"type": "header", "text": {"type": "plain_text", "text": "Approval requested"}},
        section(format!("*{}* · KVK {TEST_KVK}\nNew supplier, requested in <#{channel}>.", esc(company))),
        {"type": "section", "fields": [md(format!("*Risk profile*\n{}", profile(&found))), md("*Checks performed*\nKVK · sanctions · news")]},
        {"type": "actions", "block_id": "apiosk_approval", "elements": [
            button("apiosk_approve", "Approve", "primary"), button("apiosk_reject", "Reject", "danger"), link_button("Open report")
        ]},
        note()
    ]);
    (format!("Approval requested for {}", esc(company)), blocks)
}

pub(super) fn monitor(company: &str, frequency: Frequency, negative_news: bool) -> (String, Value) {
    let (label, days) = match frequency {
        Frequency::Daily => ("Daily", 1),
        Frequency::Weekly => ("Weekly", 7),
        Frequency::Monthly => ("Monthly", 30),
    };
    let next = (chrono::Utc::now() + chrono::Duration::days(days)).timestamp();
    let checks = if negative_news { "Business register · Sanctions lists · Adverse news" } else { "Business register · Sanctions lists" };
    let blocks = json!([
        section(format!("*Monitoring set up for {}*", esc(company))),
        {"type": "section", "fields": [md(format!("*Frequency*\n{label}")), md(format!("*Checks*\n{checks}"))]},
        section(format!("Next check: <!date^{next}^{{date_long}}|soon>. I will post an update here when something changes.")),
        {"type": "context", "elements": [md("🧪 Demo simulation · nothing has been scheduled or charged")]}
    ]);
    (format!("Monitoring set up for {} ({})", esc(company), label.to_lowercase()), blocks)
}

fn hint(error: &anyhow::Error) -> &'static str {
    let said = error.to_string();
    let has = |code: &str| said.contains(code);
    if has("not_in_channel") {
        "Invite Apiosk to this conversation first: /invite @Apiosk"
    } else if has("channel_not_found") {
        "Apiosk cannot see this conversation. Was it added to the same workspace?"
    } else if has("invalid_auth") || has("account_inactive") || has("token_revoked") || has("connected account not found") {
        "Apiosk's Slack connection no longer works. Add Apiosk to Slack again."
    } else if has("missing_scope") {
        "The Apiosk Slack app needs the chat:write scope. Add Apiosk to Slack again."
    } else if has("user_not_found") {
        "That approver is not in Apiosk's workspace."
    } else if has("unconfigured") {
        "The chat model key is not configured."
    } else {
        "Slack did not accept the test message."
    }
}

/// Posts each test response through the owner's Apiosk bot.
struct Poster<'a> {
    composio: &'a Composio,
    owner: Uuid,
    bot: &'a str,
}
impl Poster<'_> {
    async fn post(&self, channel: &str, thread: Option<&str>, text: &str, blocks: &Value) -> Result<String, ComposioError> {
        self.composio.apiosk_bot_post(self.owner, self.bot, channel, thread, text, Some(blocks)).await
    }
}

struct Posted {
    text: String,
    activity: Value,
    delivery_error: Option<ComposioError>,
}
fn posted(kind: &str, channel: &str, ts: String, text: String, blocks: &Value) -> Posted {
    Posted { text, activity: json!({"kind":kind,"status":"sent","channel_id":channel,"message_ts":ts,"preview":super::activity::preview(blocks)}), delivery_error: None }
}

fn notice(line: &str, test: bool) -> Value {
    let mut blocks = vec![section(line)];
    if test { blocks.push(note()); }
    Value::Array(blocks)
}

fn review_card(message: &str, channel: &str, value: &str) -> (String, Value) {
    let mut blocks = vec![
        json!({"type":"header","text":{"type":"plain_text","text":"Review requested"}}),
        section(format!("A matching message in <#{channel}> needs your review.")),
    ];
    // Preserve the complete incoming message within Slack's per-section limit.
    let text: Vec<char> = esc(message).chars().collect();
    for chunk in text.chunks(2500) { blocks.push(section(chunk.iter().collect::<String>())); }
    blocks.push(json!({"type":"actions","block_id":"apiosk_approval","elements":[
        {"type":"button","action_id":"apiosk_approve","style":"primary","value":value,"text":{"type":"plain_text","text":"Approve"}},
        {"type":"button","action_id":"apiosk_reject","style":"danger","value":value,"text":{"type":"plain_text","text":"Reject"}}
    ]}));
    ("A Slack message is awaiting your review.".into(), Value::Array(blocks))
}

/// One approval request: who reviews what, and where the answer lands.
struct Approval<'a> {
    trigger: Uuid,
    channel: &'a str,
    company: String,
    approver: String,
    approver_name: Option<String>,
    /// Whether the card lists sample findings (tool tests only).
    signals: bool,
    /// A tool test: the notice says so, and nothing is recorded as real.
    test: bool,
    /// A human review request carries its message; a tool test builds the
    /// card from the company name instead.
    review: Option<&'a str>,
    /// The dossier the request belongs to, and the thread root it replies in.
    dossier: Option<(Uuid, &'a str)>,
}

async fn send_approval(to: Poster<'_>, request: Approval<'_>) -> anyhow::Result<Posted> {
    let Approval { trigger, channel, company, approver, approver_name, signals, test, review, dossier } = request;
    let line = format!("Requesting approval for *{}* from <@{approver}>. Sending DM…", esc(&company));
    // A new channel message, never a reply in the dossier's thread.
    let notice_ts = to.post(channel, None, &line, &notice(&line, test)).await?;
    let thread=dossier.map(|(_,root)|root.to_owned()).unwrap_or_else(||notice_ts.clone());
    anyhow::ensure!(!thread.is_empty(), "missing_message_ts");
    let value = json!({"g": trigger, "o": to.owner, "b": to.bot, "c": channel, "t": thread, "a": approver, "n": company.trim(), "x": if test { 1 } else { 0 }, "d":dossier.map(|(id,_)|id)}).to_string();
    let (text, mut blocks) = match review {
        Some(message) => review_card(message, channel, &value),
        None => approval_card(&company, channel, signals, &value),
    };
    // Slack chat.postMessage opens the bot's DM when channel is a user
    // ID. conversations.open would unnecessarily require im:write.
    if let Some((id,_))=dossier {
        blocks.as_array_mut().unwrap().push(json!({"type":"actions","elements":[{"type":"button","action_id":"apiosk_open_dossier","text":{"type":"plain_text","text":"Open dossier"},"url":super::dossiers::url(id)}]}));
    }
    let delivered = to.post(&approver, None, &text, &blocks).await;
    let line = if delivered.is_ok() {
        format!("I sent *{}* to <@{approver}> for approval by DM.", esc(&company))
    } else {
        format!("⚠️ The approval request for *{}* could not be delivered to <@{approver}> by DM. No request was sent.", esc(&company))
    };
    if let Err(e) = to.composio.apiosk_bot_update(to.owner, to.bot, channel, &notice_ts, &line, &notice(&line, test)).await {
        tracing::warn!(surface = "trigger-tool-test", error = %e, "approval delivery notice not updated");
        // A delivered DM must not look failed and invite a duplicate.
    }
    let mut result = posted("request_approval", channel, thread.clone(), line, &blocks);
    result.activity["status"] = json!(if delivered.is_ok() { "awaiting_approval" } else { "delivery_failed" });
    result.activity["company"] = json!(company);
    result.activity["approver_id"] = json!(approver);
    result.activity["approver_name"] = json!(approver_name);
    result.activity["request_key"] = json!(dossier.map(|(id,_)|format!("dossier:{id}")).unwrap_or_else(||format!("{channel}:{thread}")));
    if let Some((id,_))=dossier {result.activity["dossier_id"]=json!(id);}
    result.delivery_error = delivered.err();
    Ok(result)
}

/// The normal trigger action: an actual human review request, without sample findings.
pub(super) async fn request_review(pool: &sqlx::PgPool, composio: &Composio, owner: Uuid, bot: &str,
    trigger: Uuid, channel: &str, message: &str, approver: &str, name: &str) {
    let result = send_approval(Poster { composio, owner, bot }, Approval {
        trigger, channel, company: "Slack review request".into(), approver: approver.into(), approver_name: Some(name.into()),
        signals: false, test: false, review: Some(message), dossier: None,
    }).await;
    match result {
        Ok(posted) => {
            let ts = posted.activity["message_ts"].as_str().unwrap_or_default();
            if let Err(e) = super::activity::record(pool, trigger, owner, &format!("response:{channel}:{ts}"), &posted.text, false, &posted.activity).await {
                tracing::warn!(error = %e, "review request activity not saved");
            }
            if let Some(e) = posted.delivery_error { tracing::warn!(error = %e, "review DM not delivered"); }
        },
        Err(e) => tracing::warn!(error = %e, "review request not delivered"),
    }
}

pub(super) async fn request_dossier_review(pool:&sqlx::PgPool,c:&Composio,owner:Uuid,bot:&str,trigger:Uuid,channel:&str,thread:&str,d:&Value,approver:&str,name:&str)->anyhow::Result<()> {
    let id:Uuid=d["id"].as_str().unwrap_or_default().parse()?;
    let mut lock=pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,30102026))").bind(id.to_string()).execute(&mut *lock).await?;
    // One review decision per immutable first check, including after restarts.
    let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway_v2.inbound_trigger_events WHERE owner_id=$1 AND trigger_id=$2 AND activity->>'dossier_id'=$3 AND activity->>'kind' IN ('request_approval','approval_decision') AND activity->>'status'<>'delivery_failed')").bind(owner).bind(trigger).bind(id.to_string()).fetch_one(pool).await?;
    if exists {c.apiosk_bot_post(owner,bot,channel,None,"This dossier already has a review request. Its decision is recorded in Activity and the dossier.",None).await?;return Ok(())}
    let company=d["company"]["name"].as_str().unwrap_or("Company dossier");
    let message=format!("Review the saved source evidence for {} (KVK {}). This decision records your review; it does not authorize purchases or activate monitoring.\n{}",company,d["company"]["kvk_number"].as_str().unwrap_or_default(),super::dossiers::url(id));
    let posted=send_approval(Poster{composio:c,owner,bot},Approval{
        trigger,channel,company:company.into(),approver:approver.into(),approver_name:Some(name.into()),
        signals:false,test:false,review:Some(&message),dossier:Some((id,thread)),
    }).await?;
    super::activity::record(pool,trigger,owner,&format!("dossier-review:{id}:{}",Uuid::new_v4()),&posted.text,false,&posted.activity).await?;
    lock.commit().await?;
    if let Some(error)=posted.delivery_error {return Err(error.into())}
    Ok(())
}

async fn run(to: Poster<'_>, channel: &str, trigger: Uuid, test: ToolTest) -> anyhow::Result<Posted> {
    match test {
        ToolTest::Chat { message } => {
            let text = reply::compose(&message).await?;
            let blocks = json!([section(esc(&text)), {"type": "context", "elements": [md(format!("🧪 Test · reply to “{}”", esc(&message)))]}]);
            let ts = to.post(channel, None, &esc(&text), &blocks).await?;
            Ok(posted("chat", channel, ts, text, &blocks))
        }
        ToolTest::ScreenCompany { company, signals } => {
            let (text, blocks) = screening(&company, signals);
            let ts = to.post(channel, None, &text, &blocks).await?;
            Ok(posted("screen_company", channel, ts, text, &blocks))
        }
        ToolTest::RequestApproval { company, approver, approver_name, signals } =>
            send_approval(to, Approval { trigger, channel, company, approver, approver_name, signals, test: true, review: None, dossier: None }).await,
        ToolTest::CreateMonitor { company, frequency, negative_news } => {
            let (text, blocks) = monitor(&company, frequency, negative_news);
            let ts = to.post(channel, None, &text, &blocks).await?;
            Ok(posted("create_monitor", channel, ts, text, &blocks))
        }
    }
}

pub(in crate::server) async fn tool_test(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(test): Json<ToolTest>,
) -> Response {
    let owner = match app_person(&state, &headers).await {
        Ok(p) => p.owner,
        Err(r) => return r,
    };
    if !enabled_for(owner) {
        return error(StatusCode::NOT_FOUND, "Test tools are not available for this account.");
    }
    if !test.valid() {
        return error(StatusCode::BAD_REQUEST, "Check the test input and try again.");
    }
    let connection: Option<(Option<String>, Option<String>)> = match sqlx::query_as("SELECT channel_id, connected_account_id FROM gateway_v2.inbound_triggers WHERE id=$1 AND owner_id=$2 AND source='slack'")
        .bind(id).bind(owner).fetch_optional(&state.engine.store.pool).await
    {
        Ok(v) => v,
        Err(e) => return db_error(e),
    };
    let Some((Some(channel), Some(account))) = connection else {
        return error(StatusCode::NOT_FOUND, "Slack trigger not found.");
    };
    let composio = match client() {
        Ok(c) => c,
        Err(r) => return r,
    };
    let info = match composio.trigger_channel(owner, &account, &channel).await {
        Ok(info) => info,
        Err(e) => return super::provider_error(e),
    };
    let Some(team) = info["channel"]["context_team_id"].as_str().or(info["channel"]["team_id"].as_str()) else {
        return error(StatusCode::BAD_GATEWAY, "Slack's workspace could not be verified. Try again.");
    };
    let bot = match composio.apiosk_bot(owner, Some(team)).await {
        Ok(Some(bot)) if bot.team == team => bot,
        Ok(Some(_)) => return error(StatusCode::CONFLICT, "Add Apiosk to this Slack workspace first."),
        Ok(None) => return error(StatusCode::CONFLICT, "Add Apiosk to Slack first."),
        Err(e) => {
            tracing::warn!(surface = "trigger-tool-test", error = %e, "Apiosk bot lookup failed");
            return error(StatusCode::BAD_GATEWAY, "Apiosk's Slack connection could not be checked. Try again.");
        }
    };
    match run(Poster { composio, owner, bot: &bot.account }, &channel, id, test).await {
        Ok(posted) => {
            let ts = posted.activity["message_ts"].as_str().unwrap_or_default();
            let saved = super::activity::record(&state.engine.store.pool, id, owner, &format!("response:{channel}:{ts}"), &posted.text, true, &posted.activity).await;
            if let Err(ref e) = saved { tracing::warn!(error = %e, "response activity not saved"); }
            if let Some(e) = posted.delivery_error {
                return error(StatusCode::BAD_GATEWAY, hint(&e.into()));
            }
            Json(json!({"posted": true, "activity_saved": saved.is_ok()})).into_response()
        },
        Err(e) => {
            tracing::warn!(surface = "trigger-tool-test", error = %e, "trigger tool test not posted");
            error(StatusCode::BAD_GATEWAY, hint(&e))
        }
    }
}

struct Decision {
    dossier: Option<Uuid>,
    owner: Uuid,
    trigger: Option<Uuid>,
    bot: String,
    approve: bool,
    user: String,
    user_name: String,
    approver: String,
    channel: String,
    thread: String,
    company: String,
    test: bool,
    response_url: String,
}
fn decision(payload: &Value) -> Option<Decision> {
    if payload["type"] != "block_actions" {
        return None;
    }
    let action = &payload["actions"][0];
    let approve = match action["action_id"].as_str()? {
        "apiosk_approve" => true,
        "apiosk_reject" => false,
        _ => return None,
    };
    let value: Value = serde_json::from_str(action["value"].as_str()?).ok()?;
    let id = |v: &Value| v.as_str().filter(|s| !s.is_empty() && s.len() <= 150).map(str::to_owned);
    Some(Decision {
        dossier: value["d"].as_str().and_then(|s|s.parse().ok()),
        trigger: value["g"].as_str().and_then(|s| Uuid::parse_str(s).ok()),
        owner: value["o"].as_str().and_then(|s| Uuid::parse_str(s).ok())?,
        bot: id(&value["b"])?,
        approve,
        user: id(&payload["user"]["id"])?,
        user_name: payload["user"]["name"].as_str().filter(|s| valid_company(s)).unwrap_or_default().to_owned(),
        approver: id(&value["a"])?,
        channel: id(&value["c"])?,
        thread: id(&value["t"])?,
        company: value["n"].as_str().filter(|s| valid_company(s))?.to_owned(),
        test: value["x"] == 1,
        response_url: payload["response_url"].as_str()?.to_owned(),
    })
}

async fn decide(pool: sqlx::PgPool, composio: &'static Composio, d: Decision) {
    if d.user != d.approver {
        let only = json!({"response_type": "ephemeral", "replace_original": false,
            "text": format!("Only <@{}> can approve or reject this request.", d.approver)});
        if let Err(e) = slack::respond(&d.response_url, &only).await {
            tracing::warn!(surface = "trigger-approval", error = %e, "approval notice not delivered");
        }
        return;
    }
    let (icon, verb) = if d.approve { ("✅", "approved") } else { ("❌", "rejected") };
    let line = format!("{icon} *{}* was {verb} by <@{}>.", esc(&d.company), d.user);
    // Signed callbacks also support cards sent before activity recording existed.
    // Only a unique conversation belonging to this owner may receive the event.
    let triggers: Vec<Uuid> = match sqlx::query_scalar("SELECT id FROM gateway_v2.inbound_triggers WHERE owner_id=$1 AND channel_id=$2 AND source='slack' AND ($3::uuid IS NULL OR id=$3)")
        .bind(d.owner).bind(&d.channel).bind(d.trigger).fetch_all(&pool).await {
        Ok(ids) => ids,
        Err(e) => { tracing::warn!(error = %e, "approval activity lookup failed"); return; }
    };
    let [trigger] = triggers.as_slice() else { return; };
    let request_key = d.dossier.map(|id|format!("dossier:{id}")).unwrap_or_else(||format!("{}:{}", d.channel, d.thread));
    if let Some(id)=d.dossier {
        let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway_v2.slack_dossiers WHERE id=$1 AND owner_id=$2 AND trigger_id=$3 AND channel_id=$4 AND thread_ts=$5 AND state='checked')").bind(id).bind(d.owner).bind(trigger).bind(&d.channel).bind(&d.thread).fetch_one(&pool).await.unwrap_or(false);
        if !valid {return;}
    }
    let activity = json!({"dossier_id":d.dossier,"kind":"approval_decision","status":verb,"company":d.company,"approver_id":d.user,
        "approver_name":d.user_name,"channel_id":d.channel,"message_ts":d.thread,"request_key":request_key});
    match super::activity::record(&pool, *trigger, d.owner, &format!("decision:{request_key}"), &line, d.test, &activity).await {
        Ok(true) => {},
        Ok(false) => return, // First decision survives restarts and concurrent clicks.
        Err(e) => { tracing::warn!(error = %e, "approval decision not saved"); return; }
    }
    let mut blocks = vec![section(line.clone())];
    if d.test {
        blocks.push(note());
    }
    let blocks = Value::Array(blocks);
    let card = json!({"replace_original": true, "text": line, "blocks": blocks});
    if let Err(e) = slack::respond(&d.response_url, &card).await {
        tracing::warn!(surface = "trigger-approval", error = %e, "approval card not updated");
    }
    if let Err(e) = composio.apiosk_bot_post(d.owner, &d.bot, &d.channel, None, &line, Some(&blocks)).await {
        tracing::warn!(surface = "trigger-approval", error = %e, "approval confirmation not delivered");
    }
}

fn header<'a>(headers: &'a HeaderMap, key: &str) -> &'a str {
    headers.get(key).and_then(|v| v.to_str().ok()).unwrap_or_default()
}

/// Slack's interactivity request URL. Answered at once; Slack expects that
/// within three seconds, and the decision follows through `response_url`.
pub(in crate::server) async fn slack_interactions(State(state): State<ServerState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(secret) = slack::signing_secret() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let now = chrono::Utc::now().timestamp();
    if !slack::verify(&secret, header(&headers, "x-slack-request-timestamp"), header(&headers, "x-slack-signature"), &body, now) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(payload) = url::form_urlencoded::parse(&body)
        .find(|(k, _)| k == "payload")
        .and_then(|(_, v)| serde_json::from_str::<Value>(&v).ok())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if payload["actions"].as_array().is_some_and(|actions| actions.iter().any(|a| a["action_id"] == "apiosk_demo_approve")) {
        return Json(json!({"response_type":"ephemeral","text":"This button belongs to an old recording and cannot run a check. Mention Apiosk with a new screening request to receive a real price and dossier."})).into_response();
    }
    if let (Some(d), Ok(composio)) = (decision(&payload), client()) {
        tokio::spawn(decide(state.engine.store.pool.clone(), composio, d));
    }
    StatusCode::OK.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn approval_notifies_selected_user_and_reports_delivery_truthfully() {
        use axum::{routing::{get, post}, Router};
        use std::sync::{Arc, Mutex};
        std::env::set_var("APIOSK_SLACKBOT_AUTH_CONFIG", "ac_test_approval");
        for delivered in [true, false] {
            let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
            let owner = Uuid::nil();
            let app = Router::new()
                .route("/connected_accounts/:id", get(move |Path(id): Path<String>| async move {
                    if id == "ca_deleted" { return StatusCode::NOT_FOUND.into_response(); }
                    Json(json!({"id":id,"user_id":format!("apiosk_user_{owner}"),"status":"ACTIVE",
                        "toolkit":{"slug":"slackbot"},"auth_config":{"id":"ac_test_approval"}})).into_response()
                }))
                .route("/tools/execute/:tool", post({ let calls = calls.clone(); move |Path(tool): Path<String>, Json(body): Json<Value>| { let calls = calls.clone(); async move {
                    assert_eq!(tool, "SLACKBOT_SEND_MESSAGE", "no conversations.open or extra im:write permission");
                    let args = body["arguments"].clone();
                    let dm = args["channel"] == "USELECTED";
                    calls.lock().unwrap().push(args);
                    Json(if dm && !delivered { json!({"successful":false,"data":{"ok":false,"error":"user_not_found"}}) }
                        else { json!({"successful":true,"data":{"ok":true,"ts":if dm {"2.0"} else {"1.0"}}}) })
                }}}))
                .route("/tools/execute/proxy", post({ let calls = calls.clone(); move |Json(body): Json<Value>| { let calls = calls.clone(); async move {
                    assert_eq!(body["endpoint"], "https://slack.com/api/chat.update");
                    assert_eq!(body["connected_account_id"], "ca_bot");
                    calls.lock().unwrap().push(body["body"].clone());
                    Json(json!({"data":{"ok":true}}))
                }}}));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
            let composio = Composio::new("fixture".into(), base, "https://app.apiosk.com".into(), Default::default());
            assert!(matches!(composio.trigger_members(owner, "ca_deleted", "").await, Err(ComposioError::AccountNotFound)));
            let result = run(Poster { composio: &composio, owner, bot: "ca_bot" }, "CCHANNEL", Uuid::nil(),
                ToolTest::RequestApproval { company: "Test BV".into(), approver: "USELECTED".into(), approver_name: Some("Alex".into()), signals: false }).await;
            let result = result.unwrap();
            assert_eq!(result.delivery_error.is_none(), delivered);
            assert_eq!(result.activity["status"], if delivered { "awaiting_approval" } else { "delivery_failed" });
            let calls = calls.lock().unwrap();
            assert_eq!(calls.len(), 3);
            assert_eq!(calls[0]["channel"], "CCHANNEL");
            assert!(calls[0]["fallback_text"].as_str().unwrap().contains("Sending DM"));
            assert_eq!(calls[1]["channel"], "USELECTED");
            let payload: Value = serde_json::from_str(calls[1]["blocks"][3]["elements"][0]["value"].as_str().unwrap()).unwrap();
            assert_eq!(payload["a"], "USELECTED");
            assert_eq!(payload["t"], "1.0");
            assert_eq!(calls[2]["ts"], "1.0");
            assert_eq!(calls[2]["text"].as_str().unwrap().contains("No request was sent"), !delivered);
            task.abort();
        }
        std::env::remove_var("APIOSK_SLACKBOT_AUTH_CONFIG");
    }

    #[test]
    fn inputs_are_bounded_and_approvers_are_slack_user_ids() {
        assert!(ToolTest::RequestApproval { company: "Coolblue".into(), approver: "U0123ABCD".into(), approver_name: None, signals: false }.valid());
        assert!(!ToolTest::RequestApproval { company: "Coolblue".into(), approver: "C0123ABCD".into(), approver_name: None, signals: false }.valid());
        assert!(!ToolTest::ScreenCompany { company: " ".into(), signals: true }.valid());
        assert!(!ToolTest::ScreenCompany { company: "a".repeat(121), signals: true }.valid());
        assert!(!ToolTest::Chat { message: String::new() }.valid());
    }

    #[test]
    fn company_names_cannot_inject_slack_mentions_or_links() {
        // Plain-text headers are not parsed; fallback text and mrkdwn are.
        let name = "<!channel> & <https://x.test|klik>";
        for (text, blocks) in [screening(name, true), approval_card(name, "C1", true, "{}"), monitor(name, Frequency::Daily, false)] {
            let parsed = format!("{text}{}", blocks.as_array().unwrap().iter().filter(|b| b["type"] != "header").map(Value::to_string).collect::<String>());
            assert!(!parsed.contains("<!channel>") && !parsed.contains("<https://x.test"), "{parsed}");
            assert!(parsed.contains("&lt;!channel&gt; &amp;"));
        }
    }

    #[test]
    fn every_response_is_labelled_as_test_data() {
        for (_, blocks) in [screening("A", false), approval_card("A", "C1", true, "{}"), monitor("A", Frequency::Weekly, true)] {
            assert!(blocks.to_string().contains("🧪"));
        }
        assert!(monitor("A", Frequency::Weekly, true).1.to_string().contains("Adverse news"));
        assert!(!monitor("A", Frequency::Weekly, false).1.to_string().contains("Adverse news"));
    }

    #[test]
    fn decisions_come_only_from_approval_buttons_with_a_complete_value() {
        let owner = Uuid::nil().to_string();
        let value = json!({"o":owner,"b":"ca_bot","c":"C1","t":"1.2","a":"U1","n":"Coolblue","x":1}).to_string();
        let click = |id: &str, value: &str| json!({"type":"block_actions","user":{"id":"U1"},"response_url":"https://hooks.slack.com/x",
            "actions":[{"action_id":id,"value":value}]});
        let d = decision(&click("apiosk_approve", &value)).unwrap();
        assert!(d.approve && d.test && d.user == d.approver && d.thread == "1.2" && d.bot == "ca_bot");
        assert!(!decision(&click("apiosk_reject", &value)).unwrap().approve);
        assert!(decision(&click("apiosk_open_dossier", &value)).is_none());
        assert!(decision(&click("apiosk_approve", "{\"c\":\"C1\"}")).is_none());
    }
}
