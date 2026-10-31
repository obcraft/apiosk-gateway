//! Recording demo for allow-listed owners (`TRIGGER_DEMO_OWNER_IDS`). Apiosk
//! answers dossier mentions in Slack with simulated responses: a priced
//! screening, its result after Approve, an approver's review and a monitoring
//! confirmation. Nothing is looked up, priced, bought or scheduled, and every
//! simulated result says so. Other owners keep the real dossier flow.
use super::{activity, dossiers, reply, slack::{self, esc}, tools};
use crate::{company_checks::Company, integrations::Composio};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

const PRICE: &str = "€0.18";
const LABEL: &str = "Demo · sample results, no sources were called and nothing was charged";
const APPROVE: &str = "apiosk_demo_approve";

pub(super) fn enabled_for(owner: Uuid) -> bool {
    crate::recurring::parse_allowed_owners(std::env::var("TRIGGER_DEMO_OWNER_IDS").ok().as_deref()).contains(&owner)
}

fn section(text: impl Into<String>) -> Value {
    json!({"type":"section","text":{"type":"mrkdwn","text":text.into()}})
}
fn label() -> Value {
    json!({"type":"context","elements":[{"type":"mrkdwn","text":LABEL}]})
}
fn open_button() -> Value {
    let origin = std::env::var("APP_ORIGIN").unwrap_or_else(|_| "https://app.apiosk.com".into());
    json!({"type":"button","action_id":"apiosk_open_dossier","text":{"type":"plain_text","text":"Open in Apiosk"},"url":format!("{}/triggers", origin.trim_end_matches('/'))})
}
fn kvk(company: &Company) -> &str {
    if company.kvk_number.is_empty() { "Not given" } else { &company.kvk_number }
}

/// The price card, posted as its own channel message. The Approve value names
/// only the conversation and company; the callback re-checks the owner.
fn price_card(company: &Company, asker: Option<&str>, value: &str) -> (String, Value) {
    let requested = asker.map(|a| format!("Requested by <@{a}> · ")).unwrap_or_default();
    let blocks = json!([
        {"type":"header","text":{"type":"plain_text","text":format!("Screening: {}", company.name)}},
        {"type":"section","fields":[
            {"type":"mrkdwn","text":format!("*KVK*\n{}", esc(kvk(company)))},
            {"type":"mrkdwn","text":format!("*First-check price*\n{PRICE}")}
        ]},
        section("*Sources*\n• KVK business register\n• Sanctions lists\n• News from the last 7 days"),
        {"type":"actions","elements":[
            {"type":"button","action_id":APPROVE,"style":"primary","value":value,"text":{"type":"plain_text","text":format!("Approve {PRICE}")}},
            open_button()
        ]},
        {"type":"context","elements":[{"type":"mrkdwn","text":format!("{requested}{LABEL}")}]}
    ]);
    (format!("{}: first check {PRICE}. Approve to run it.", esc(&company.name)), blocks)
}

fn result_card(company: &Company) -> (String, Value) {
    let blocks = json!([
        {"type":"header","text":{"type":"plain_text","text":format!("Screening saved: {}", company.name)}},
        {"type":"section","fields":[
            {"type":"mrkdwn","text":format!("*KVK*\n{}", esc(kvk(company)))},
            {"type":"mrkdwn","text":"*Result*\n🟢 No issues identified"}
        ]},
        section("*Checks performed*\n✅ Business register (KVK)   ✅ Sanctions lists   ✅ News from the last 7 days"),
        section("Mention me with `request approval` to send it to the approver, or `monitor` to keep it up to date."),
        {"type":"actions","elements":[open_button()]},
        label()
    ]);
    (format!("Screening saved for {}: no issues identified.", esc(&company.name)), blocks)
}

fn frequency(lower: &str) -> tools::Frequency {
    if lower.contains("daily") || lower.contains("dagelijks") {
        tools::Frequency::Daily
    } else if lower.contains("month") || lower.contains("maand") {
        tools::Frequency::Monthly
    } else {
        tools::Frequency::Weekly
    }
}

/// The channel's latest demo screening, as saved in the response activity.
async fn latest(pool: &PgPool, owner: Uuid, trigger: Uuid, channel: &str) -> anyhow::Result<Option<(Company, String)>> {
    let row: Option<Value> = sqlx::query_scalar("SELECT activity FROM gateway_v2.inbound_trigger_events WHERE owner_id=$1 AND trigger_id=$2 AND activity->>'demo'='true' AND activity->>'kind'='screen_company' AND activity->>'channel_id'=$3 ORDER BY id DESC LIMIT 1")
        .bind(owner).bind(trigger).bind(channel).fetch_optional(pool).await?;
    Ok(row.map(|a| (Company { name: a["company"].as_str().unwrap_or("the company").to_owned(), kvk_number: a["kvk_number"].as_str().unwrap_or_default().to_owned() },
        a["status"].as_str().unwrap_or("prepared").to_owned())))
}

/// Answers a mention in demo mode. False leaves the text to the chat reply.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle(pool: &PgPool, c: &Composio, owner: Uuid, trigger: Uuid, bot: &str, channel: &str, asker: Option<&str>, text: &str) -> anyhow::Result<bool> {
    let lower = text.to_lowercase();
    let to = asker.map(|a| format!("<@{a}> ")).unwrap_or_default();
    let post = |line: String, blocks: Option<Value>| async move { c.apiosk_bot_post(owner, bot, channel, None, &line, blocks.as_ref()).await };
    if dossiers::screening_request(text) {
        let company = match crate::company_checks::single_company(text) {
            Some(company) => Some(company),
            None => reply::company_name(text).await.map(|name| Company { name, kvk_number: String::new() }),
        };
        let Some(company) = company else {
            post(format!("{to}Which company should I screen? For example: `@Apiosk screen Mollie B.V. KVK 30204462`."), None).await?;
            return Ok(true);
        };
        let value = json!({"o":owner,"g":trigger,"b":bot,"c":channel,"n":company.name,"k":company.kvk_number}).to_string();
        let (line, blocks) = price_card(&company, asker, &value);
        let ts = post(line.clone(), Some(blocks.clone())).await?;
        activity::record(pool, trigger, owner, &format!("response:{channel}:{ts}"), &line, false, &json!({"kind":"screen_company","status":"prepared","demo":true,
            "company":company.name,"kvk_number":company.kvk_number,"channel_id":channel,"message_ts":ts,"preview":activity::preview(&blocks)})).await?;
        return Ok(true);
    }
    let review = lower.contains("request approval") || lower.contains("goedkeuring") || lower.contains("approve");
    let monitor = lower.contains("monitor") || lower.contains("volg");
    if !review && !monitor && !lower.contains("status") {
        return Ok(false);
    }
    let Some((company, status)) = latest(pool, owner, trigger, channel).await? else {
        post(format!("{to}Screen a company first, for example: `@Apiosk screen Mollie B.V. KVK 30204462`."), None).await?;
        return Ok(true);
    };
    if review {
        if status != "checked" {
            post(format!("{to}Approve the first-check price for *{}* first, then ask me again.", esc(&company.name)), None).await?;
            return Ok(true);
        }
        let approver: Option<(String, String)> = sqlx::query_as("SELECT approver_id,approver_name FROM gateway_v2.inbound_triggers WHERE id=$1 AND owner_id=$2 AND approver_id IS NOT NULL")
            .bind(trigger).bind(owner).fetch_optional(pool).await?;
        let Some((user, name)) = approver else {
            post(format!("{to}Choose an approver for this trigger on the Triggers page first."), None).await?;
            return Ok(true);
        };
        let message = format!("Review the screening for {} (KVK {}): no issues identified in the business register, sanctions lists and news from the last 7 days.\n_{LABEL}_", company.name, kvk(&company));
        tools::request_named_review(pool, c, owner, bot, trigger, channel, &company.name, &message, &user, &name).await?;
        return Ok(true);
    }
    if monitor {
        let (line, blocks) = tools::monitor(&company.name, frequency(&lower), true);
        let ts = post(line.clone(), Some(blocks.clone())).await?;
        activity::record(pool, trigger, owner, &format!("response:{channel}:{ts}"), &line, false, &json!({"kind":"create_monitor","status":"sent","demo":true,
            "company":company.name,"channel_id":channel,"message_ts":ts,"preview":activity::preview(&blocks)})).await?;
        return Ok(true);
    }
    let state = if status == "checked" { "Screening saved, no issues identified." } else { "The first-check price is waiting for approval." };
    post(format!("{to}*{}*: {state}", esc(&company.name)), Some(json!([section(format!("{to}*{}*: {state}", esc(&company.name))), label()]))).await?;
    Ok(true)
}

/// Approve on a demo price card: the card shows the approval, then the result
/// follows as its own message, once per card however often it is clicked.
pub(super) struct Approval {
    owner: Uuid,
    trigger: Uuid,
    bot: String,
    channel: String,
    company: Company,
    user: String,
    card_ts: String,
    response_url: String,
}
pub(super) fn approval(payload: &Value) -> Option<Approval> {
    let action = &payload["actions"][0];
    if payload["type"] != "block_actions" || action["action_id"] != APPROVE {
        return None;
    }
    let value: Value = serde_json::from_str(action["value"].as_str()?).ok()?;
    let id = |v: &Value| v.as_str().filter(|s| !s.is_empty() && s.len() <= 150).map(str::to_owned);
    let name = value["n"].as_str().filter(|s| !s.trim().is_empty() && s.chars().count() <= 120)?.to_owned();
    let kvk_number = value["k"].as_str().filter(|k| k.len() <= 8 && k.bytes().all(|b| b.is_ascii_digit())).unwrap_or_default().to_owned();
    Some(Approval {
        owner: Uuid::parse_str(value["o"].as_str()?).ok()?,
        trigger: Uuid::parse_str(value["g"].as_str()?).ok()?,
        bot: id(&value["b"])?,
        channel: id(&value["c"])?,
        company: Company { name, kvk_number },
        user: id(&payload["user"]["id"])?,
        card_ts: id(&payload["message"]["ts"])?,
        response_url: payload["response_url"].as_str()?.to_owned(),
    })
}

pub(super) async fn approve(pool: PgPool, c: &'static Composio, a: Approval) {
    let Approval { owner, trigger, bot, channel, company, user, card_ts, response_url } = a;
    if !enabled_for(owner) {
        return;
    }
    let line = format!("✅ {PRICE} approved by <@{user}>. Checking the business register, sanctions lists and news…");
    let key = format!("demo-approval:{channel}:{card_ts}");
    match activity::record(&pool, trigger, owner, &key, &line, false, &json!({"kind":"approval_decision","status":"approved","demo":true,"company":company.name,
        "approver_id":user,"channel_id":channel,"message_ts":card_ts,"request_key":key})).await {
        Ok(true) => {},
        Ok(false) => return, // Already approved; ownership is enforced on insertion.
        Err(e) => return tracing::warn!(error = %e, "demo approval not saved"),
    }
    let updated = json!({"replace_original": true, "text": line, "blocks": [
        {"type":"header","text":{"type":"plain_text","text":format!("Screening: {}", company.name)}},
        section(line.clone()), label()
    ]});
    if let Err(e) = slack::respond(&response_url, &updated).await {
        tracing::warn!(error = %e, "demo price card not updated");
    }
    tokio::time::sleep(Duration::from_secs(4)).await;
    let (line, blocks) = result_card(&company);
    match c.apiosk_bot_post(owner, &bot, &channel, None, &line, Some(&blocks)).await {
        Ok(ts) => if let Err(e) = activity::record(&pool, trigger, owner, &format!("response:{channel}:{ts}"), &line, false, &json!({"kind":"screen_company","status":"checked","demo":true,
            "company":company.name,"kvk_number":company.kvk_number,"channel_id":channel,"message_ts":ts,"preview":activity::preview(&blocks)})).await {
            tracing::warn!(error = %e, "demo result activity not saved");
        },
        Err(e) => tracing::warn!(error = %e, "demo result not posted"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mollie() -> Company {
        Company { name: "Mollie B.V.".into(), kvk_number: "30204462".into() }
    }

    #[test]
    fn every_simulated_response_is_labelled_and_mentions_are_escaped() {
        let hostile = Company { name: "<!channel> B.V.".into(), kvk_number: String::new() };
        for (text, blocks) in [price_card(&hostile, Some("U1"), "{}"), result_card(&hostile)] {
            assert!(blocks.to_string().contains("nothing was charged"));
            let parsed = format!("{text}{}", blocks.as_array().unwrap().iter().filter(|b| b["type"] != "header").map(Value::to_string).collect::<String>());
            assert!(!parsed.contains("<!channel>"), "{parsed}");
        }
        assert!(tools::monitor("A", frequency("monitor weekly"), true).1.to_string().contains("Demo simulation"));
    }

    #[test]
    fn approve_reads_only_its_own_button_with_a_complete_value() {
        let owner = Uuid::new_v4();
        let trigger = Uuid::new_v4();
        let (_, blocks) = price_card(&mollie(), None, &json!({"o":owner,"g":trigger,"b":"ca_bot","c":"C1","n":"Mollie B.V.","k":"30204462"}).to_string());
        let button = blocks[3]["elements"][0].clone();
        assert_eq!(button["text"]["text"], format!("Approve {PRICE}"));
        let click = |action: &Value| json!({"type":"block_actions","user":{"id":"U9"},"message":{"ts":"1.5"},"response_url":"https://hooks.slack.com/x","actions":[action]});
        let a = approval(&click(&button)).unwrap();
        assert_eq!((a.owner, a.trigger, a.bot.as_str(), a.channel.as_str(), a.user.as_str(), a.card_ts.as_str()), (owner, trigger, "ca_bot", "C1", "U9", "1.5"));
        assert_eq!(a.company, mollie());
        assert!(approval(&click(&json!({"action_id":"apiosk_approve","value":button["value"]}))).is_none());
        assert!(approval(&click(&json!({"action_id":APPROVE,"value":"{\"c\":\"C1\"}"}))).is_none());
    }

    #[test]
    fn monitoring_follows_the_requested_frequency() {
        assert!(matches!(frequency("@apiosk monitor daily"), tools::Frequency::Daily));
        assert!(matches!(frequency("@apiosk volg maandelijks"), tools::Frequency::Monthly));
        assert!(matches!(frequency("@apiosk monitor"), tools::Frequency::Weekly));
    }
}
