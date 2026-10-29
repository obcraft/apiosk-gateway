//! Owner-only test tools for the Slack agent's responses. Each one posts as the
//! owner's Apiosk bot with labelled test data, so every message can be reviewed before
//! any paid call: nothing here screens, quotes, buys, monitors or persists.
//! The approval card's buttons work, and decisions are announced, not stored.
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
use std::{
    collections::HashSet,
    sync::{Mutex, OnceLock},
};
use uuid::Uuid;

const NOTE: &str = "🧪 Test · testdata, geen echte controle en geen kosten";
const TEST_KVK: &str = "00000000";

/// `TRIGGER_TOOL_TEST_OWNER_IDS`, comma-separated owner UUIDs. A partly
/// malformed setting grants nobody access.
pub(super) fn enabled_for(owner: Uuid) -> bool {
    crate::recurring::parse_allowed_owners(std::env::var("TRIGGER_TOOL_TEST_OWNER_IDS").ok().as_deref()).contains(&owner)
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
    RequestApproval { company: String, approver: String, signals: bool },
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
            Self::RequestApproval { company, approver, .. } => valid_company(company) && valid_user(approver),
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
        "🟠 *Sanctielijsten:* mogelijke match op naam, handmatig controleren".into(),
        "🔵 *Handelsregister:* bezoekadres gewijzigd sinds de vorige controle".into(),
        format!("📰 *Nieuws:* “Voorbeeldartikel over {}” (testbron)", esc(company)),
    ]
}
fn profile(findings: &[String]) -> String {
    match findings.len() {
        0 => "🟢 Geen bijzonderheden".into(),
        n => format!("🟠 Aandacht nodig · {n} signalen"),
    }
}

fn screening(company: &str, signals: bool) -> (String, Value) {
    let found = findings(company, signals);
    let mut blocks = vec![
        json!({"type": "header", "text": {"type": "plain_text", "text": format!("Screening: {}", company.trim())}}),
        json!({"type": "section", "fields": [md(format!("*KVK*\n{TEST_KVK}")), md(format!("*Resultaat*\n{}", profile(&found)))]}),
        section("*Gecontroleerd*\n✅ Handelsregister (KVK)   ✅ Sanctielijsten   ✅ Nieuws, laatste 7 dagen"),
    ];
    blocks.push(section(if found.is_empty() {
        "Niets gevonden dat aandacht nodig heeft.".to_owned()
    } else {
        format!("*Bevindingen*\n{}", found.join("\n"))
    }));
    blocks.push(json!({"type": "actions", "elements": [link_button("Open dossier")]}));
    blocks.push(note());
    (format!("Screening van {}: {}", esc(company), profile(&found)), Value::Array(blocks))
}

fn approval_card(company: &str, channel: &str, signals: bool, value: &str) -> (String, Value) {
    let found = findings(company, signals);
    let button = |id: &str, text: &str, style: &str| {
        json!({"type": "button", "action_id": id, "style": style, "value": value, "text": {"type": "plain_text", "text": text}})
    };
    let blocks = json!([
        {"type": "header", "text": {"type": "plain_text", "text": "Goedkeuring gevraagd"}},
        section(format!("*{}* · KVK {TEST_KVK}\nNieuwe leverancier, aangevraagd in <#{channel}>.", esc(company))),
        {"type": "section", "fields": [md(format!("*Risicoprofiel*\n{}", profile(&found))), md("*Gecontroleerd*\nKVK · sancties · nieuws")]},
        {"type": "actions", "block_id": "apiosk_approval", "elements": [
            button("apiosk_approve", "Approve", "primary"), button("apiosk_reject", "Reject", "danger"), link_button("Open dossier")
        ]},
        note()
    ]);
    (format!("Goedkeuring gevraagd voor {}", esc(company)), blocks)
}

fn monitor(company: &str, frequency: Frequency, negative_news: bool) -> (String, Value) {
    let (label, days) = match frequency {
        Frequency::Daily => ("Dagelijks", 1),
        Frequency::Weekly => ("Wekelijks", 7),
        Frequency::Monthly => ("Maandelijks", 30),
    };
    let next = (chrono::Utc::now() + chrono::Duration::days(days)).timestamp();
    let checks = if negative_news { "Handelsregister · Sanctielijsten · Negatief nieuws" } else { "Handelsregister · Sanctielijsten" };
    let blocks = json!([
        section(format!("*Monitoring ingesteld voor {}*", esc(company))),
        {"type": "section", "fields": [md(format!("*Frequentie*\n{label}")), md(format!("*Controles*\n{checks}"))]},
        section(format!("Volgende controle: <!date^{next}^{{date_long}}|binnenkort>. Ik meld het hier zodra er iets verandert.")),
        {"type": "context", "elements": [md("🧪 Demosimulatie · er is niets ingesteld en niets betaald")]}
    ]);
    (format!("Monitoring ingesteld voor {} ({})", esc(company), label.to_lowercase()), blocks)
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
        "The Apiosk Slack app needs the chat:write and im:write scopes."
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
    composio: &'static Composio,
    owner: Uuid,
    bot: &'a str,
}
impl Poster<'_> {
    async fn post(&self, channel: &str, thread: Option<&str>, text: &str, blocks: &Value) -> Result<String, ComposioError> {
        self.composio.apiosk_bot_post(self.owner, self.bot, channel, thread, text, Some(blocks)).await
    }
}

async fn run(to: Poster<'_>, channel: &str, test: ToolTest) -> anyhow::Result<()> {
    match test {
        ToolTest::Chat { message } => {
            let text = reply::compose(&message).await?;
            let blocks = json!([section(esc(&text)), {"type": "context", "elements": [md(format!("🧪 Test · antwoord op “{}”", esc(&message)))]}]);
            to.post(channel, None, &esc(&text), &blocks).await?;
        }
        ToolTest::ScreenCompany { company, signals } => {
            let (text, blocks) = screening(&company, signals);
            to.post(channel, None, &text, &blocks).await?;
        }
        ToolTest::RequestApproval { company, approver, signals } => {
            let line = format!("Ik heb *{}* ter goedkeuring gestuurd naar <@{approver}>.", esc(&company));
            let thread = to.post(channel, None, &line, &json!([section(line.clone()), note()])).await?;
            let value = json!({"o": to.owner, "b": to.bot, "c": channel, "t": thread, "a": approver, "n": company.trim(), "x": 1}).to_string();
            let (text, blocks) = approval_card(&company, channel, signals, &value);
            let dm = to.composio.apiosk_bot_dm(to.owner, to.bot, &approver).await?;
            to.post(&dm, None, &text, &blocks).await?;
        }
        ToolTest::CreateMonitor { company, frequency, negative_news } => {
            let (text, blocks) = monitor(&company, frequency, negative_news);
            to.post(channel, None, &text, &blocks).await?;
        }
    }
    Ok(())
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
    let channel: Option<Option<String>> = match sqlx::query_scalar("SELECT channel_id FROM gateway_v2.inbound_triggers WHERE id=$1 AND owner_id=$2 AND source='slack'")
        .bind(id).bind(owner).fetch_optional(&state.engine.store.pool).await
    {
        Ok(v) => v,
        Err(e) => return db_error(e),
    };
    let Some(channel) = channel.flatten() else {
        return error(StatusCode::NOT_FOUND, "Slack trigger not found.");
    };
    let composio = match client() {
        Ok(c) => c,
        Err(r) => return r,
    };
    let bot = match composio.apiosk_bot(owner, None).await {
        Ok(Some(bot)) => bot,
        Ok(None) => return error(StatusCode::CONFLICT, "Add Apiosk to Slack first."),
        Err(e) => {
            tracing::warn!(surface = "trigger-tool-test", error = %e, "Apiosk bot lookup failed");
            return error(StatusCode::BAD_GATEWAY, "Apiosk's Slack connection could not be checked. Try again.");
        }
    };
    match run(Poster { composio, owner, bot: &bot.account }, &channel, test).await {
        Ok(()) => Json(json!({"posted": true})).into_response(),
        Err(e) => {
            tracing::warn!(surface = "trigger-tool-test", error = %e, "trigger tool test not posted");
            error(StatusCode::BAD_GATEWAY, hint(&e))
        }
    }
}

struct Decision {
    owner: Uuid,
    bot: String,
    approve: bool,
    user: String,
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
        owner: value["o"].as_str().and_then(|s| Uuid::parse_str(s).ok())?,
        bot: id(&value["b"])?,
        approve,
        user: id(&payload["user"]["id"])?,
        approver: id(&value["a"])?,
        channel: id(&value["c"])?,
        thread: id(&value["t"])?,
        company: value["n"].as_str().filter(|s| valid_company(s))?.to_owned(),
        test: value["x"] == 1,
        response_url: payload["response_url"].as_str()?.to_owned(),
    })
}

/// One decision per request in this process; double clicks are ignored.
fn first_decision(key: String) -> bool {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let mut seen = SEEN.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner());
    if seen.len() > 10_000 {
        seen.clear();
    }
    seen.insert(key)
}

async fn decide(composio: &'static Composio, d: Decision) {
    if d.user != d.approver {
        let only = json!({"response_type": "ephemeral", "replace_original": false,
            "text": format!("Alleen <@{}> kan deze aanvraag goedkeuren of afwijzen.", d.approver)});
        if let Err(e) = slack::respond(&d.response_url, &only).await {
            tracing::warn!(surface = "trigger-approval", error = %e, "approval notice not delivered");
        }
        return;
    }
    if !first_decision(format!("{}:{}", d.channel, d.thread)) {
        return;
    }
    let (icon, verb) = if d.approve { ("✅", "goedgekeurd") } else { ("❌", "afgewezen") };
    let line = format!("{icon} *{}* is {verb} door <@{}>.", esc(&d.company), d.user);
    let mut blocks = vec![section(line.clone())];
    if d.test {
        blocks.push(note());
    }
    let blocks = Value::Array(blocks);
    let card = json!({"replace_original": true, "text": line, "blocks": blocks});
    if let Err(e) = slack::respond(&d.response_url, &card).await {
        tracing::warn!(surface = "trigger-approval", error = %e, "approval card not updated");
    }
    if let Err(e) = composio.apiosk_bot_post(d.owner, &d.bot, &d.channel, Some(&d.thread), &line, Some(&blocks)).await {
        tracing::warn!(surface = "trigger-approval", error = %e, "approval confirmation not delivered");
    }
}

fn header<'a>(headers: &'a HeaderMap, key: &str) -> &'a str {
    headers.get(key).and_then(|v| v.to_str().ok()).unwrap_or_default()
}

/// Slack's interactivity request URL. Answered at once; Slack expects that
/// within three seconds, and the decision follows through `response_url`.
pub(in crate::server) async fn slack_interactions(headers: HeaderMap, body: Bytes) -> Response {
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
    if let (Some(d), Ok(composio)) = (decision(&payload), client()) {
        tokio::spawn(decide(composio, d));
    }
    StatusCode::OK.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_are_bounded_and_approvers_are_slack_user_ids() {
        assert!(ToolTest::RequestApproval { company: "Coolblue".into(), approver: "U0123ABCD".into(), signals: false }.valid());
        assert!(!ToolTest::RequestApproval { company: "Coolblue".into(), approver: "C0123ABCD".into(), signals: false }.valid());
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
        assert!(monitor("A", Frequency::Weekly, true).1.to_string().contains("Negatief nieuws"));
        assert!(!monitor("A", Frequency::Weekly, false).1.to_string().contains("Negatief nieuws"));
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

    #[test]
    fn a_request_is_decided_once() {
        assert!(first_decision("C9:1.1".into()));
        assert!(!first_decision("C9:1.1".into()));
    }
}
