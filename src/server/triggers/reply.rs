//! A short written reply in Slack when a trigger message mentions Apiosk.
//! Conversation only: it cannot plan, quote, look anything up or execute, so a
//! data question gets a pointer to the App instead of an answer.
//! It is posted by the workspace's Apiosk bot connection, never from the
//! person's own account; without that connection there is no reply.
use super::slack;
use crate::{
    integrations::Composio,
    telemetry::{self, ErrorClass, Outcome},
};
use anyhow::{anyhow, ensure, Result};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use uuid::Uuid;

const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
/// Cheap and quick enough for a chat reply ($0.10/$0.50 per million tokens on
/// 2026-10-29). `TRIGGER_REPLY_MODEL` overrides it.
const DEFAULT_MODEL: &str = "anthropic/claude-haiku-5.5";
const DEADLINE: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_MESSAGE_CHARS: usize = 2000;
const MAX_REPLY_CHARS: usize = 600;
/// A hard stop per conversation, whatever gets past the self-mention guard.
const MAX_REPLIES_PER_MINUTE: usize = 6;
const PROMPT: &str = "You reply in a Slack conversation where someone mentioned Apiosk, a service that checks companies, suppliers and invoices against official data sources.
Write like a friendly colleague: natural, short and to the point. Usually one sentence, at most three. Always reply in English, even when the message is in Dutch or another language, or asks you to use another language.
Answer greetings and small talk normally, for example \"How are you?\" -> \"I'm fine, thanks for asking! How about you?\".
Never state facts about a company, person, invoice, price or result, and never claim you checked or looked anything up. If the message asks for a check, lookup or research, explain briefly that they can mention you with `screen` and the company name or KVK number to start a dossier, then review the real sources and approve the price in the app. Never say a check or monitoring has started without execution evidence.
Plain text only: no headings or lists. Never write the word Apiosk.";
const COMPANY_PROMPT: &str = "Find the company that this Slack message asks to screen, check or onboard. Reply with only that company's name exactly as it is written in the message, including a legal form such as B.V., or with NONE when the message names no company. No other words.";

/// One kind of model call: its instructions and limits.
struct Prompt {
    surface: &'static str,
    system: &'static str,
    max_tokens: u32,
    temperature: f32,
    deadline: Duration,
}
const REPLY: Prompt = Prompt { surface: "trigger-reply", system: PROMPT, max_tokens: 200, temperature: 0.3, deadline: DEADLINE };
/// Short deadline: the dossier card waits for the company name.
const COMPANY: Prompt = Prompt { surface: "trigger-company", system: COMPANY_PROMPT, max_tokens: 40, temperature: 0.0, deadline: Duration::from_secs(5) };

/// "apiosk" as its own word, so "@apiosk", "Apiosk," and "*apiosk*" count but
/// a link or address such as app.apiosk.com does not.
pub(super) fn mentions(text: &str) -> bool {
    let text = text.replace(['*', '_', '~'], "").to_lowercase();
    text.match_indices("apiosk").any(|(at, word)| {
        let before = text[..at].chars().next_back();
        let rest = &text[at + word.len()..];
        let mut after = rest.chars();
        let next = after.next();
        let domain = next == Some('.') && after.next().is_some_and(char::is_alphanumeric);
        !before.is_some_and(|c| c.is_alphanumeric() || matches!(c, '.' | '/' | '-'))
            && !next.is_some_and(|c| c.is_alphanumeric() || c == '-')
            && !domain
    })
}

/// Replies sent per conversation in the last minute.
#[derive(Default)]
struct Limiter(Mutex<HashMap<String, Vec<Instant>>>);
impl Limiter {
    fn allow(&self, channel: &str, now: Instant) -> bool {
        let mut sent = self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        sent.retain(|_, times| {
            times.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
            !times.is_empty()
        });
        let times = sent.entry(channel.to_owned()).or_default();
        if times.len() >= MAX_REPLIES_PER_MINUTE {
            return false;
        }
        times.push(now);
        true
    }
}

struct Model {
    http: reqwest::Client,
    key: String,
    name: String,
}
fn model() -> Option<&'static Model> {
    static MODEL: OnceLock<Option<Model>> = OnceLock::new();
    MODEL
        .get_or_init(|| {
            let env = |name| std::env::var(name).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
            Some(Model {
                http: reqwest::Client::new(),
                key: env("OPENROUTER_API_KEY").or_else(|| env("INTENT_API_KEY"))?,
                name: env("TRIGGER_REPLY_MODEL").unwrap_or_else(|| DEFAULT_MODEL.into()),
            })
        })
        .as_ref()
}

/// Writes the reply and posts it as Apiosk, tagging the asker. Runs after the
/// event is committed, so a retried delivery is a duplicate and never replied
/// to twice.
pub(super) async fn send(
    composio: &'static Composio,
    pool: sqlx::PgPool,
    triggers: Vec<Uuid>,
    owner: Uuid,
    bot: String,
    channel: String,
    thread: Option<String>,
    asker: Option<String>,
    message: String,
) {
    static LIMITER: OnceLock<Limiter> = OnceLock::new();
    let Some(model) = model() else {
        tracing::warn!(surface = "trigger-reply", reason = "unconfigured", "trigger reply skipped");
        return;
    };
    if !LIMITER.get_or_init(Limiter::default).allow(&channel, Instant::now()) {
        tracing::warn!(surface = "trigger-reply", reason = "rate_limited", "trigger reply skipped");
        return;
    }
    let text = match write(model, &message).await {
        Ok(text) => text,
        Err(error) => {
            tracing::warn!(surface = "trigger-reply", reason = %error, "trigger reply unavailable");
            return;
        }
    };
    // Escaped: a model reply could otherwise ping @channel or disguise a link.
    let text = match asker {
        Some(asker) => format!("<@{asker}> {}", slack::esc(&text)),
        None => slack::esc(&text),
    };
    match composio.apiosk_bot_post(owner, &bot, &channel, thread.as_deref(), &text, None).await {
        Ok(ts) => for trigger in triggers {
            let activity = json!({"kind":"chat","status":"sent","channel_id":channel,"message_ts":ts});
            if let Err(error) = super::activity::record(&pool, trigger, owner, &format!("response:{channel}:{ts}"), &text, false, &activity).await {
                tracing::warn!(surface = "trigger-reply", error = %error, "reply activity not saved");
            }
        },
        Err(error) => tracing::warn!(surface = "trigger-reply", error = %error, "trigger reply not delivered"),
    }
}

/// The chat reply alone, for the owner's test tools.
pub(super) async fn compose(message: &str) -> Result<String> {
    write(model().ok_or_else(|| anyhow!("unconfigured"))?, message).await
}

/// The company a screening request names, exactly as written in it, to
/// prefill the dossier's company search. None without a model or a name.
pub(super) async fn company_name(message: &str) -> Option<String> {
    match ask(model()?, &COMPANY, message, |body| company_text(body, message)).await {
        Ok(name) => name,
        Err(error) => {
            tracing::warn!(surface = "trigger-company", reason = %error, "company name not detected");
            None
        }
    }
}

async fn write(model: &Model, message: &str) -> Result<String> {
    ask(model, &REPLY, message, reply_text).await
}

/// One bounded attempt. Errors carry a fixed reason, never the prompt, reply,
/// key or provider detail.
async fn ask<T>(model: &Model, prompt: &Prompt, message: &str, check: impl Fn(&Value) -> Result<T>) -> Result<T> {
    let payload = json!({
        "model": model.name, "temperature": prompt.temperature, "max_tokens": prompt.max_tokens,
        "messages": [{"role":"system","content":prompt.system},{"role":"user","content":clip(message, MAX_MESSAGE_CHARS)}]
    });
    let mut call = telemetry::model_call(prompt.surface, 1, &model.name, serde_json::to_vec(&payload)?.len());
    let fetched = async {
        telemetry::record_model_http_request();
        let response = model.http.post(ENDPOINT).bearer_auth(&model.key)
            .header("X-Title", "Apiosk trigger reply")
            .timeout(prompt.deadline).json(&payload).send().await
            .map_err(|error| if error.is_timeout() { (ErrorClass::Timeout, "timeout") } else { (ErrorClass::Transport, "transport_failure") })?;
        if !response.status().is_success() {
            return Err((ErrorClass::upstream(response.status().as_u16()), "http_status"));
        }
        crate::parser::bounded_json_sized(response, MAX_RESPONSE_BYTES).await
            .map_err(|_| (ErrorClass::Validation, "invalid_envelope"))
    }
    .await;
    let (body, output_bytes) = match fetched {
        Ok(fetched) => fetched,
        Err((class, reason)) => {
            call.finish(Outcome::Failed(class), reason);
            return Err(anyhow!(reason));
        }
    };
    call.received(&body, output_bytes);
    match check(&body) {
        Ok(text) => {
            call.finish(Outcome::Ok, "validated");
            Ok(text)
        }
        Err(error) => {
            call.finish(Outcome::Failed(ErrorClass::Validation), "invalid_output");
            Err(error)
        }
    }
}

fn reply_text(body: &Value) -> Result<String> {
    ensure!(body["error"].is_null(), "provider_error");
    let choice = body.pointer("/choices/0").ok_or_else(|| anyhow!("missing_choice"))?;
    ensure!(!matches!(choice["finish_reason"].as_str(), Some("length" | "content_filter")), "incomplete_reply");
    let text = choice["message"]["content"].as_str().unwrap_or_default().trim();
    ensure!(!text.is_empty() && text.chars().count() <= MAX_REPLY_CHARS, "invalid_length");
    // Slack may deliver the posted reply back as a new message; one that names
    // Apiosk would then answer itself.
    ensure!(!mentions(text), "self_mention");
    Ok(text.to_owned())
}

fn company_text(body: &Value, message: &str) -> Result<Option<String>> {
    ensure!(body["error"].is_null(), "provider_error");
    let choice = body.pointer("/choices/0").ok_or_else(|| anyhow!("missing_choice"))?;
    ensure!(!matches!(choice["finish_reason"].as_str(), Some("length" | "content_filter")), "incomplete_reply");
    let name = choice["message"]["content"].as_str().unwrap_or_default()
        .trim().trim_matches(|c| matches!(c, '"' | '\'' | '`' | '*')).trim();
    if name.is_empty() || name.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    // The model only selects: a name that is not in the message is rejected.
    let plain = |s: &str| s.replace(['*', '_'], "").split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    ensure!((2..=160).contains(&name.chars().count()) && !name.contains(['\n', '<', '>', '@']), "invalid_name");
    ensure!(plain(message).contains(&plain(name)), "name_not_in_message");
    Ok(Some(name.to_owned()))
}

fn clip(text: &str, chars: usize) -> &str {
    let text = text.trim();
    text.char_indices().nth(chars).map_or(text, |(end, _)| &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mention_is_the_word_not_a_link() {
        for text in ["@apiosk How are you?", "Apiosk, hoe gaat het?", "hey *apiosk*", "thanks apiosk.", "vraagje voor _Apiosk_: hoi", "<@U123> apiosk"] {
            assert!(mentions(text), "{text}");
        }
        for text in ["see app.apiosk.com", "<https://apiosk.com|apiosk.com>", "mail hi@apiosk.com", "apioskapp", "my-apiosk-test", "How are you?"] {
            assert!(!mentions(text), "{text}");
        }
    }

    #[test]
    fn limiter_caps_each_conversation_per_minute() {
        let limiter = Limiter::default();
        let start = Instant::now();
        for _ in 0..MAX_REPLIES_PER_MINUTE {
            assert!(limiter.allow("C1", start));
        }
        assert!(!limiter.allow("C1", start));
        assert!(limiter.allow("C2", start));
        assert!(limiter.allow("C1", start + Duration::from_secs(61)));
    }

    #[test]
    fn reply_text_rejects_incomplete_long_and_self_mentioning_output() {
        let body = |content: &str, finish: &str| json!({"choices":[{"finish_reason":finish,"message":{"content":content}}]});
        assert_eq!(reply_text(&body("  I'm fine, thanks for asking!  ", "stop")).unwrap(), "I'm fine, thanks for asking!");
        assert!(reply_text(&body("I'm fine", "length")).is_err());
        assert!(reply_text(&body("", "stop")).is_err());
        assert!(reply_text(&body(&"a".repeat(MAX_REPLY_CHARS + 1), "stop")).is_err());
        assert!(reply_text(&body("Apiosk is doing great!", "stop")).is_err());
        assert!(reply_text(&json!({"error":{"message":"x"},"choices":[]})).is_err());
    }

    #[test]
    fn company_name_is_only_what_the_message_says() {
        let body = |content: &str| json!({"choices":[{"finish_reason":"stop","message":{"content":content}}]});
        let message = "<@U0BOT> Can we screen *Coolblue B.V.* to make sure we can trust them ?";
        assert_eq!(company_text(&body("\"Coolblue B.V.\""), message).unwrap().as_deref(), Some("Coolblue B.V."));
        assert_eq!(company_text(&body("coolblue b.v."), message).unwrap().as_deref(), Some("coolblue b.v."));
        assert_eq!(company_text(&body("NONE"), "status").unwrap(), None);
        assert!(company_text(&body("Coolblue Holding B.V."), message).is_err());
        assert!(company_text(&body("<@U0BOT>"), message).is_err());
    }

    #[test]
    fn clip_keeps_whole_characters() {
        assert_eq!(clip("  héllo  ", 3), "hél");
        assert_eq!(clip("hi", 10), "hi");
    }
}
