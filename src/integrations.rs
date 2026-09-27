//! Business-app integrations, brokered by Composio.
//!
//! A person connects their own communication channels: Slack, Microsoft Teams,
//! Discord, Telegram, WhatsApp, Google Chat, Gmail, Outlook (Hotmail) or Notion. The OAuth consent happens on Composio's hosted page
//! and the tokens stay with Composio. The gateway holds one project key and
//! the right to ask about ONE Composio user per caller — `composio_user_id` of
//! the authenticated owner — and nothing a caller sends can name another one.
//! The caller names a toolkit; this module supplies the identity, the auth
//! config and the callback URL.
//!
//! Any authenticated person can start the flow: the App, or an agent they
//! connected (the agent hands the returned link to its person). Either way the
//! browser comes back to the App's Integrations page.
//!
//! Nothing is stored in Postgres. Composio keeps the connected accounts keyed
//! by user id, and a second copy would have to be kept in step with a provider
//! that can expire a token without telling us.
//!
//! Env:
//!   COMPOSIO_API_KEY       an `ak_…` project key. A `ck_…` consumer key is
//!                          for MCP clients and every REST call rejects it.
//!   COMPOSIO_AUTH_CONFIGS  optional JSON map, toolkit → `ac_…`, to pin a
//!                          custom OAuth app instead of the Composio-managed
//!                          one this module finds or creates.
//!   COMPOSIO_BASE_URL      optional, for tests.
//!   APP_ORIGIN             where Composio sends the browser back to.

use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::OnceLock, time::Duration};
use tokio::sync::Mutex;
use uuid::Uuid;

/// The apps a person may connect, in the order they are shown.
/// Communication channels only. The slugs are Composio's: Discord is
/// `discordbot` because the plain `discord` toolkit cannot post a message.
pub const TOOLKITS: [(&str, &str); 9] = [
    ("slack", "Slack"),
    ("microsoft_teams", "Microsoft Teams"),
    ("discordbot", "Discord"),
    ("telegram", "Telegram"),
    ("whatsapp", "WhatsApp"),
    ("google_chat", "Google Chat"),
    ("gmail", "Gmail"),
    ("outlook", "Outlook"),
    ("notion", "Notion"),
];

/// The allow-listed slug, or `None` for anything else — refused before
/// Composio hears about it.
pub fn toolkit(slug: &str) -> Option<&'static str> {
    TOOLKITS.iter().map(|(slug, _)| *slug).find(|known| *known == slug)
}

/// The Composio user an Apiosk account maps to. Prefixed so an organisation
/// scope can be added later without the two ever colliding.
pub fn composio_user_id(owner: Uuid) -> String {
    format!("apiosk_user_{owner}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntegrationRow {
    pub toolkit: &'static str,
    pub name: &'static str,
    /// `connected`, `expired` (it used to work; reconnect) or `not_connected`.
    pub status: &'static str,
    pub connected_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Link {
    pub toolkit: &'static str,
    pub redirect_url: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ComposioError {
    /// The project key was refused: an operator problem, never the caller's.
    #[error("composio rejected the project key")]
    Unauthorized,
    #[error("composio returned {0}")]
    Status(u16),
    #[error("composio unreachable: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("unexpected composio response: {0}")]
    Invalid(&'static str),
    /// Composio has no managed login for this app (Telegram's bot token, a
    /// Google Chat OAuth app) and none was set up in the Composio dashboard.
    #[error("no auth config set up for this toolkit")]
    NeedsSetup,
}

/// Accounts that name this user and an allowed toolkit. `user_ids` asks
/// Composio to filter; this comparison is what keeps it safe if that filter
/// were ever ignored — an account for somebody else is never shown to this
/// caller and never deleted on their behalf.
fn owned<'a>(user_id: &'a str, items: &'a [Value]) -> impl Iterator<Item = &'a Value> + 'a {
    items.iter().filter(move |account| {
        account["user_id"].as_str() == Some(user_id)
            && account["toolkit"]["slug"].as_str().and_then(toolkit).is_some()
    })
}

/// One row per allowed toolkit, from Composio's connected accounts.
pub fn statuses(user_id: &str, items: &[Value]) -> Vec<IntegrationRow> {
    let mine: Vec<&Value> = owned(user_id, items).filter(|a| a["is_disabled"].as_bool() != Some(true)).collect();
    TOOLKITS
        .iter()
        .map(|(slug, name)| {
            let of = || mine.iter().filter(|a| a["toolkit"]["slug"].as_str() == Some(slug));
            if let Some(active) = of().find(|a| a["status"] == "ACTIVE") {
                return IntegrationRow { toolkit: slug, name, status: "connected", connected_at: active["created_at"].as_str().map(str::to_owned) };
            }
            // EXPIRED used to work. FAILED and INITIATED are attempts somebody
            // abandoned; they read as never connected.
            let status = if of().any(|a| a["status"] == "EXPIRED") { "expired" } else { "not_connected" };
            IntegrationRow { toolkit: slug, name, status, connected_at: None }
        })
        .collect()
}

/// `{"google_chat":"ac_…"}`, keeping only allowed toolkits and well-formed ids.
pub fn pinned_auth_configs(raw: Option<&str>) -> HashMap<&'static str, String> {
    let Some(parsed) = raw.and_then(|raw| serde_json::from_str::<HashMap<String, String>>(raw).ok()) else {
        return HashMap::new();
    };
    parsed
        .into_iter()
        .filter_map(|(key, id)| {
            let well_formed = id.starts_with("ac_") && (4..=67).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
            let slug = toolkit(&key)?;
            well_formed.then_some((slug, id))
        })
        .collect()
}

/// The capability domain every connected-app action lives under, so the
/// planner's slugs read `app.slack.sendMessage`.
pub const ACTION_DOMAIN: &str = "app";
/// Actions offered per connected app. Every one is a known slug in the
/// planner's prompt, so the cap keeps planning as fast as without apps.
const ACTIONS_PER_TOOLKIT: usize = 8;
/// An action must write something, and nothing destructive. Reads (get, list,
/// search…) are left out on purpose: offered to the planner, "what is Acme's
/// revenue" could plan a CRM lookup instead of a source search, and the data
/// would not be found. Deletes are never run from Ask.
const WRITE_VERBS: [&str; 17] = ["SEND", "CREATE", "ADD", "APPEND", "UPDATE", "POST", "UPLOAD", "INSERT", "REPLY", "FORWARD", "UPSERT", "WRITE", "INVITE", "SHARE", "DRAFT", "SCHEDULE", "LOG"];
const READ_VERBS: [&str; 13] = ["GET", "LIST", "FETCH", "FIND", "SEARCH", "RETRIEVE", "READ", "LOOKUP", "QUERY", "CHECK", "COUNT", "DESCRIBE", "EXPORT"];
const DESTRUCTIVE_VERBS: [&str; 10] = ["DELETE", "REMOVE", "ARCHIVE", "TRASH", "CLEAR", "PURGE", "DESTROY", "REVOKE", "KICK", "LEAVE"];

/// Whether a tool writes without destroying, from its slug and Composio's hints.
pub fn is_write_action(toolkit: &str, tool: &str, tags: &Value) -> bool {
    let hinted = |hint: &str| tags.as_array().is_some_and(|tags| tags.iter().any(|tag| tag.as_str() == Some(hint)));
    if hinted("readOnlyHint") || hinted("destructiveHint") {
        return false;
    }
    let Some(rest) = tool.strip_prefix(&format!("{}_", toolkit.to_ascii_uppercase())) else { return false };
    let words: Vec<&str> = rest.split('_').filter(|w| !w.is_empty()).collect();
    !words.first().is_some_and(|first| READ_VERBS.contains(first))
        && !words.iter().any(|w| DESTRUCTIVE_VERBS.contains(w))
        && words.iter().any(|w| WRITE_VERBS.contains(w))
}
const ACTION_CACHE: Duration = Duration::from_secs(60 * 60);

/// One thing a connected app can do, in the planner's capability shape plus
/// what execution needs. `inputs` are the tool's required parameters.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AppAction {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub domain: &'static str,
    pub toolkit: &'static str,
    pub tool: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub input_schema: Value,
}

/// `SLACK_SEND_MESSAGE` → `app.slack.sendMessage`. The toolkit prefix is
/// required: a tool that does not carry it is not this toolkit's to offer.
pub fn action_slug(toolkit: &str, tool: &str) -> Option<String> {
    let rest = tool.strip_prefix(&format!("{}_", toolkit.to_ascii_uppercase()))?;
    let words: Vec<String> = rest.split('_').filter(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_alphanumeric())).map(str::to_ascii_lowercase).collect();
    if words.is_empty() || words.len() != rest.split('_').filter(|w| !w.is_empty()).count() {
        return None;
    }
    let action: String = words.iter().enumerate().map(|(i, w)| if i == 0 { w.clone() } else { format!("{}{}", w[..1].to_ascii_uppercase(), &w[1..]) }).collect();
    Some(format!("{ACTION_DOMAIN}.{toolkit}.{action}"))
}

/// The action with this slug in `actions`, if it is one.
pub fn find_action<'a>(actions: &'a [AppAction], slug: &str) -> Option<&'a AppAction> {
    actions.iter().find(|action| action.slug == slug)
}

fn snake(value: &str) -> String {
    let mut out = String::new();
    for (i, c) in value.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 && !out.ends_with('_') {
            out.push('_');
        }
        if c.is_ascii_alphanumeric() { out.push(c.to_ascii_lowercase()) } else if !out.ends_with('_') { out.push('_') }
    }
    out.trim_matches('_').to_owned()
}

fn clipped(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", text[..cut].trim_end()),
        None => text,
    }
}

/// A Composio tool as an action, or `None` when it is deprecated, belongs to
/// another toolkit, or has no usable name.
pub fn action_from_tool(toolkit: &'static str, tool: &Value) -> Option<AppAction> {
    if tool["is_deprecated"].as_bool() == Some(true) || tool["toolkit"]["slug"].as_str().is_some_and(|slug| slug != toolkit) {
        return None;
    }
    let slug_raw = tool["slug"].as_str()?;
    if !is_write_action(toolkit, slug_raw, &tool["tags"]) {
        return None;
    }
    let slug = action_slug(toolkit, slug_raw)?;
    let schema = tool["input_parameters"].clone();
    let names = |schema: &Value, required_only: bool| -> Vec<String> {
        let required: Vec<&str> = schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        schema["properties"].as_object().into_iter().flatten()
            .map(|(key, _)| key.as_str())
            .filter(|key| !required_only || required.contains(key))
            .map(snake).filter(|key| !key.is_empty()).take(16).collect()
    };
    Some(AppAction {
        name: clipped(tool["name"].as_str().filter(|n| !n.trim().is_empty())?, 80),
        description: clipped(tool["human_description"].as_str().or(tool["description"].as_str()).unwrap_or_default(), 280),
        domain: ACTION_DOMAIN,
        toolkit,
        tool: slug_raw.to_owned(),
        inputs: names(&schema, true),
        outputs: names(&tool["output_parameters"]["properties"]["data"], false),
        input_schema: if schema.is_object() { schema } else { json!({"type": "object", "properties": {}}) },
        slug,
    })
}

/// One of an app's Composio tools, as the Integrations page lists it. Every
/// tool is listed; `in_ask` marks the ones Ask can plan and run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolkitAction {
    pub tool: String,
    pub name: String,
    pub description: String,
    /// The Ask capability slug (`app.slack.sendMessage`) when `in_ask`.
    pub slug: Option<String>,
    /// False for reads, deletes and anything else Ask leaves out.
    pub in_ask: bool,
}

/// Every non-deprecated tool of `toolkit`, the ones Ask offers first in their
/// own order, the rest by name.
pub fn toolkit_catalog(toolkit: &'static str, tools: &[Value], offered: &[AppAction]) -> Vec<ToolkitAction> {
    let mut seen = std::collections::HashSet::new();
    let mut listed: Vec<ToolkitAction> = tools
        .iter()
        .filter(|tool| tool["is_deprecated"].as_bool() != Some(true) && tool["toolkit"]["slug"].as_str().is_none_or(|slug| slug == toolkit))
        .filter_map(|tool| {
            let slug = tool["slug"].as_str().filter(|slug| !slug.is_empty() && seen.insert(slug.to_owned()))?;
            let asked = offered.iter().find(|action| action.tool == slug);
            Some(ToolkitAction {
                tool: slug.to_owned(),
                name: clipped(tool["name"].as_str().filter(|n| !n.trim().is_empty()).unwrap_or(slug), 80),
                description: clipped(tool["human_description"].as_str().or(tool["description"].as_str()).unwrap_or_default(), 280),
                slug: asked.map(|action| action.slug.clone()),
                in_ask: asked.is_some(),
            })
        })
        .collect();
    let rank = |item: &ToolkitAction| offered.iter().position(|action| action.tool == item.tool).unwrap_or(usize::MAX);
    listed.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    listed
}

pub struct Composio {
    http: reqwest::Client,
    base: String,
    api_key: String,
    app_origin: String,
    pinned: HashMap<&'static str, String>,
    /// Found or created once per process. Held across the lookup so two
    /// first connects cannot both create one.
    auth_configs: Mutex<HashMap<&'static str, String>>,
    /// Each toolkit's actions, shared by every caller for an hour.
    actions: Mutex<HashMap<&'static str, (std::time::Instant, Vec<AppAction>)>>,
    /// Each toolkit's full tool list, for the Integrations page, also an hour.
    catalogs: Mutex<HashMap<&'static str, (std::time::Instant, Vec<ToolkitAction>)>>,
}

impl Composio {
    pub fn new(api_key: String, base: String, app_origin: String, pinned: HashMap<&'static str, String>) -> Self {
        Self {
            http: reqwest::Client::builder().timeout(Duration::from_secs(15)).build().expect("reqwest client"),
            base: base.trim_end_matches('/').to_owned(),
            api_key,
            app_origin: app_origin.trim_end_matches('/').to_owned(),
            pinned,
            auth_configs: Mutex::new(HashMap::new()),
            actions: Mutex::new(HashMap::new()),
            catalogs: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("COMPOSIO_API_KEY").ok().map(|key| key.trim().to_owned()).filter(|key| !key.is_empty())?;
        let app_origin = std::env::var("APP_ORIGIN").unwrap_or_else(|_| "https://app.apiosk.com".into());
        let base = std::env::var("COMPOSIO_BASE_URL").unwrap_or_else(|_| "https://backend.composio.dev/api/v3.1".into());
        Some(Self::new(api_key, base, app_origin, pinned_auth_configs(std::env::var("COMPOSIO_AUTH_CONFIGS").ok().as_deref())))
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value, ComposioError> {
        let mut request = self.http.request(method, format!("{}{path}", self.base)).header("x-api-key", &self.api_key).header("accept", "application/json");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ComposioError::Unauthorized);
        }
        if !status.is_success() {
            return Err(ComposioError::Status(status.as_u16()));
        }
        let text = response.text().await?;
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|_| ComposioError::Invalid("body"))
    }

    async fn auth_config(&self, toolkit: &'static str) -> Result<String, ComposioError> {
        if let Some(id) = self.pinned.get(toolkit) {
            return Ok(id.clone());
        }
        let mut cache = self.auth_configs.lock().await;
        if let Some(id) = cache.get(toolkit) {
            return Ok(id.clone());
        }
        let found = self.call(reqwest::Method::GET, &format!("/auth_configs?toolkit_slug={toolkit}&limit=50"), None).await?;
        let usable: Vec<&Value> = found["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| item["status"] != "DISABLED" && item["id"].is_string() && item["toolkit"]["slug"].as_str().is_none_or(|slug| slug == toolkit))
            .collect();
        // Composio's own login first; otherwise the one set up in the Composio
        // dashboard, which is how Telegram (a bot token) and Google Chat (an
        // OAuth app of our own) get connected.
        let existing = usable.iter().find(|item| item["is_composio_managed"] == true).or(usable.first()).and_then(|item| item["id"].as_str()).map(str::to_owned);
        let id = match existing {
            Some(id) => id,
            None => match self.call(reqwest::Method::POST, "/auth_configs", Some(json!({"toolkit": {"slug": toolkit}, "auth_config": {"type": "use_composio_managed_auth"}}))).await {
                Ok(created) => created["auth_config"]["id"].as_str().ok_or(ComposioError::Invalid("auth_config.id"))?.to_owned(),
                Err(ComposioError::Status(400..=499)) => return Err(ComposioError::NeedsSetup),
                Err(error) => return Err(error),
            },
        };
        cache.insert(toolkit, id.clone());
        Ok(id)
    }

    async fn accounts(&self, user_id: &str) -> Result<Vec<Value>, ComposioError> {
        let page = self
            .call(reqwest::Method::GET, &format!("/connected_accounts?user_ids={user_id}&limit=100&order_by=updated_at&order_direction=desc"), None)
            .await?;
        Ok(page["items"].as_array().cloned().unwrap_or_default())
    }

    pub async fn list(&self, owner: Uuid) -> Result<Vec<IntegrationRow>, ComposioError> {
        let user_id = composio_user_id(owner);
        Ok(statuses(&user_id, &self.accounts(&user_id).await?))
    }

    /// A hosted consent link. The callback is built from `APP_ORIGIN` and never
    /// taken from the caller: Composio redirects a browser to it, so accepting
    /// one would be an open redirect.
    pub async fn connect(&self, owner: Uuid, toolkit: &'static str) -> Result<Link, ComposioError> {
        let auth_config_id = self.auth_config(toolkit).await?;
        let callback = format!("{}/integrations?integration={toolkit}", self.app_origin);
        let session = self
            .call(reqwest::Method::POST, "/connected_accounts/link", Some(json!({"auth_config_id": auth_config_id, "user_id": composio_user_id(owner), "callback_url": callback})))
            .await?;
        let redirect = session["redirect_url"].as_str().and_then(|raw| url::Url::parse(raw).ok()).ok_or(ComposioError::Invalid("redirect_url"))?;
        if redirect.scheme() != "https" {
            return Err(ComposioError::Invalid("redirect_url"));
        }
        Ok(Link { toolkit, redirect_url: redirect.to_string(), expires_at: session["expires_at"].as_str().map(str::to_owned) })
    }

    /// Deletes every account this person has for the toolkit, and nobody else's.
    pub async fn disconnect(&self, owner: Uuid, toolkit: &'static str) -> Result<usize, ComposioError> {
        let user_id = composio_user_id(owner);
        let items = self.accounts(&user_id).await?;
        let ids: Vec<&str> = owned(&user_id, &items)
            .filter(|a| a["toolkit"]["slug"].as_str() == Some(toolkit))
            .filter_map(|a| a["id"].as_str())
            .filter(|id| !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
            .collect();
        for id in &ids {
            self.call(reqwest::Method::DELETE, &format!("/connected_accounts/{id}"), None).await?;
        }
        Ok(ids.len())
    }

    /// A toolkit's actions: Composio's featured tools, cached for an hour.
    async fn toolkit_actions(&self, toolkit: &'static str) -> Result<Vec<AppAction>, ComposioError> {
        if let Some((at, cached)) = self.actions.lock().await.get(toolkit) {
            if at.elapsed() < ACTION_CACHE {
                return Ok(cached.clone());
            }
        }
        // Featured tools first; when too few of them write, fill up from the rest.
        let mut found: Vec<AppAction> = Vec::new();
        for query in ["important=true&limit=100", "limit=200"] {
            let page = self.call(reqwest::Method::GET, &format!("/tools?toolkit_slug={toolkit}&{query}"), None).await?;
            for action in page["items"].as_array().into_iter().flatten().filter_map(|tool| action_from_tool(toolkit, tool)) {
                if found.len() < ACTIONS_PER_TOOLKIT && !found.iter().any(|known| known.slug == action.slug) {
                    found.push(action);
                }
            }
            if found.len() >= ACTIONS_PER_TOOLKIT / 2 {
                break;
            }
        }
        self.actions.lock().await.insert(toolkit, (std::time::Instant::now(), found.clone()));
        Ok(found)
    }

    /// Everything an app can do through Composio, marked with what Ask uses.
    /// Tool metadata only: nothing here reads or depends on a person's account.
    pub async fn catalog(&self, toolkit: &'static str) -> Result<Vec<ToolkitAction>, ComposioError> {
        if let Some((at, cached)) = self.catalogs.lock().await.get(toolkit) {
            if at.elapsed() < ACTION_CACHE {
                return Ok(cached.clone());
            }
        }
        let offered = self.toolkit_actions(toolkit).await?;
        let mut tools: Vec<Value> = Vec::new();
        let mut cursor: Option<String> = None;
        // A few pages at most: the largest toolkits have a few hundred tools.
        for _ in 0..5 {
            let after = cursor.as_deref().map(|c| format!("&cursor={}", url::form_urlencoded::byte_serialize(c.as_bytes()).collect::<String>())).unwrap_or_default();
            let page = self.call(reqwest::Method::GET, &format!("/tools?toolkit_slug={toolkit}&limit=1000{after}"), None).await?;
            tools.extend(page["items"].as_array().cloned().unwrap_or_default());
            cursor = page["next_cursor"].as_str().filter(|c| !c.is_empty()).map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        let listed = toolkit_catalog(toolkit, &tools, &offered);
        self.catalogs.lock().await.insert(toolkit, (std::time::Instant::now(), listed.clone()));
        Ok(listed)
    }

    /// Every action of every app this person has connected. A toolkit whose
    /// actions cannot be read is left out rather than failing the rest.
    pub async fn actions(&self, owner: Uuid) -> Result<Vec<AppAction>, ComposioError> {
        let connected: Vec<&'static str> = self.list(owner).await?.into_iter().filter(|row| row.status == "connected").map(|row| row.toolkit).collect();
        let mut actions = Vec::new();
        for toolkit in connected {
            match self.toolkit_actions(toolkit).await {
                Ok(found) => actions.extend(found),
                Err(ComposioError::Unauthorized) => return Err(ComposioError::Unauthorized),
                Err(error) => tracing::warn!(toolkit, error = %error, "composio actions unavailable"),
            }
        }
        Ok(actions)
    }

    /// Runs one action as this person. Only an action found for an app they
    /// have connected can run — never an arbitrary Composio tool slug.
    pub async fn execute(&self, owner: Uuid, slug: &str, arguments: &Value) -> Result<ActionOutcome, ExecuteError> {
        if !arguments.is_object() || serde_json::to_vec(arguments).map_or(true, |bytes| bytes.len() > 64 * 1024) {
            return Err(ExecuteError::InvalidArguments);
        }
        let actions = self.actions(owner).await?;
        let action = find_action(&actions, slug).ok_or(ExecuteError::UnknownAction)?;
        let result = self
            .call(reqwest::Method::POST, &format!("/tools/execute/{}", action.tool), Some(json!({"user_id": composio_user_id(owner), "arguments": arguments})))
            .await?;
        Ok(ActionOutcome {
            slug: action.slug.clone(),
            toolkit: action.toolkit,
            successful: result["successful"].as_bool().unwrap_or(false),
            data: result["data"].clone(),
            error: result["error"].as_str().map(|e| clipped(e, 500)),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActionOutcome {
    pub slug: String,
    pub toolkit: &'static str,
    pub successful: bool,
    pub data: Value,
    pub error: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecuteError {
    #[error("arguments must be a JSON object under 64 KiB")]
    InvalidArguments,
    #[error("not an action of a connected app")]
    UnknownAction,
    #[error(transparent)]
    Provider(#[from] ComposioError),
}

static CLIENT: OnceLock<Option<Composio>> = OnceLock::new();

/// The process-wide client, or `None` while no project key is configured.
pub fn configured() -> Option<&'static Composio> {
    CLIENT.get_or_init(Composio::from_env).as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "apiosk_user_00000000-0000-4000-8000-000000000001";

    fn account(id: &str, user: &str, slug: &str, status: &str) -> Value {
        json!({"id": id, "user_id": user, "toolkit": {"slug": slug}, "status": status, "created_at": "2026-09-27T10:00:00Z"})
    }

    #[test]
    fn only_the_six_toolkits_are_allowed() {
        assert_eq!(toolkit("slack"), Some("slack"));
        assert_eq!(toolkit("discordbot"), Some("discordbot"));
        assert_eq!(toolkit("google_chat"), Some("google_chat"));
        for slug in ["github", "hubspot", "salesforce", "googlesheets", "discord", "Slack", "", "gmail/../x"] {
            assert_eq!(toolkit(slug), None);
        }
    }

    #[test]
    fn statuses_show_only_this_users_accounts() {
        let items = vec![
            account("ca_1", ME, "slack", "ACTIVE"),
            account("ca_2", "apiosk_user_someone-else", "telegram", "ACTIVE"),
            account("ca_3", ME, "gmail", "EXPIRED"),
            account("ca_4", ME, "outlook", "INITIATED"),
            account("ca_5", ME, "notion", "FAILED"),
            account("ca_6", ME, "github", "ACTIVE"),
        ];
        let rows = statuses(ME, &items);
        let by = |slug: &str| rows.iter().find(|row| row.toolkit == slug).unwrap().status;
        assert_eq!(rows.len(), 9);
        assert_eq!(by("slack"), "connected");
        assert_eq!(by("telegram"), "not_connected");
        assert_eq!(by("gmail"), "expired");
        assert_eq!(by("outlook"), "not_connected");
        assert_eq!(by("notion"), "not_connected");
        assert_eq!(rows[0].connected_at.as_deref(), Some("2026-09-27T10:00:00Z"));
    }

    #[test]
    fn an_active_account_wins_over_an_expired_one_and_disabled_ones_do_not_count() {
        let mut disabled = account("ca_9", ME, "telegram", "ACTIVE");
        disabled["is_disabled"] = json!(true);
        let rows = statuses(ME, &[account("ca_1", ME, "slack", "EXPIRED"), account("ca_2", ME, "slack", "ACTIVE"), disabled]);
        assert_eq!(rows[0].status, "connected");
        assert_eq!(rows[3].status, "not_connected");
    }

    /// A stand-in for Composio that records what it was asked.
    async fn fake_composio(key: &'static str) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use axum::{extract::{OriginalUri, Path}, http::{HeaderMap, StatusCode}, routing::{delete, get, post}, Json, Router};
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let authorised = move |headers: &HeaderMap| headers.get("x-api-key").and_then(|v| v.to_str().ok()) == Some(key);
        let app = Router::new()
            .route("/auth_configs", get({ let log = log.clone(); move |headers: HeaderMap, uri: OriginalUri| async move {
                log.lock().unwrap().push(format!("GET {}", uri.0));
                if !authorised(&headers) { return Err(StatusCode::UNAUTHORIZED) }
                // Slack has a managed one, Telegram one set up by hand; the rest must be created.
                let query = uri.0.query().unwrap_or("").to_owned();
                Ok(Json(if query.contains("toolkit_slug=slack") { json!({"items":[{"id":"ac_custom","status":"ENABLED","is_composio_managed":false,"toolkit":{"slug":"slack"}},{"id":"ac_slack","status":"ENABLED","is_composio_managed":true,"toolkit":{"slug":"slack"}}]}) }
                    else if query.contains("toolkit_slug=telegram") { json!({"items":[{"id":"ac_bot_token","status":"ENABLED","is_composio_managed":false,"toolkit":{"slug":"telegram"}}]}) }
                    else { json!({"items":[]}) }))
            }}).post({ let log = log.clone(); move |Json(body): Json<Value>| async move {
                log.lock().unwrap().push(format!("POST /auth_configs {body}"));
                // No managed login for Google Chat: Composio refuses to create one.
                if body["toolkit"]["slug"] == "google_chat" { return Err(StatusCode::BAD_REQUEST) }
                Ok(Json(json!({"auth_config":{"id":format!("ac_{}", body["toolkit"]["slug"].as_str().unwrap())}})))
            }}))
            .route("/connected_accounts", get({ let log = log.clone(); move |uri: OriginalUri| async move {
                log.lock().unwrap().push(format!("GET {}", uri.0));
                // Deliberately ignores the user filter, like a regressed API would.
                Json(json!({"items":[
                    account("ca_mine", ME, "notion", "ACTIVE"),
                    account("ca_old", ME, "notion", "EXPIRED"),
                    account("ca_theirs", "apiosk_user_other", "notion", "ACTIVE"),
                ]}))
            }}))
            .route("/connected_accounts/link", post({ let log = log.clone(); move |Json(body): Json<Value>| async move {
                log.lock().unwrap().push(format!("POST /connected_accounts/link {body}"));
                Json(json!({"redirect_url":"https://connect.composio.dev/link/lt_123","expires_at":"2026-09-27T11:00:00Z","connected_account_id":"ca_new"}))
            }}))
            .route("/connected_accounts/:id", delete({ let log = log.clone(); move |Path(id): Path<String>| async move {
                log.lock().unwrap().push(format!("DELETE {id}"));
                Json(json!({"success":true}))
            }}))
            .route("/tools", get({ let log = log.clone(); move |uri: OriginalUri| async move {
                log.lock().unwrap().push(format!("GET {}", uri.0));
                Json(json!({"items":[
                    {"slug":"NOTION_CREATE_NOTION_PAGE","name":"Create page","toolkit":{"slug":"notion"},"description":"Creates a page.","input_parameters":{"type":"object","properties":{"title":{"type":"string"}},"required":["title"]}},
                    {"slug":"NOTION_OLD_THING","name":"Old","toolkit":{"slug":"notion"},"is_deprecated":true},
                    {"slug":"NOTION_FETCH_DATA","name":"Fetch data","toolkit":{"slug":"notion"}},
                    {"slug":"NOTION_DELETE_BLOCK","name":"Delete block","toolkit":{"slug":"notion"}},
                ]}))
            }}))
            .route("/tools/execute/:tool", post({ let log = log.clone(); move |Path(tool): Path<String>, Json(body): Json<Value>| async move {
                log.lock().unwrap().push(format!("EXECUTE {tool} {body}"));
                Json(json!({"successful":true,"data":{"id":"501"},"error":null}))
            }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, log)
    }

    #[tokio::test]
    async fn the_client_scopes_every_call_to_the_callers_composio_user() {
        let (base, log) = fake_composio("ak_test").await;
        let composio = Composio::new("ak_test".into(), base, "https://app.apiosk.com/".into(), HashMap::new());
        let owner: Uuid = "00000000-0000-4000-8000-000000000001".parse().unwrap();

        let rows = composio.list(owner).await.unwrap();
        assert!(rows.iter().all(|row| row.status == if row.toolkit == "notion" { "connected" } else { "not_connected" }));

        let link = composio.connect(owner, "notion").await.unwrap();
        assert_eq!(link.redirect_url, "https://connect.composio.dev/link/lt_123");
        composio.connect(owner, "notion").await.unwrap();
        composio.connect(owner, "slack").await.unwrap();
        composio.connect(owner, "telegram").await.unwrap();
        assert!(matches!(composio.connect(owner, "google_chat").await, Err(ComposioError::NeedsSetup)));

        assert_eq!(composio.disconnect(owner, "notion").await.unwrap(), 2);

        let log = log.lock().unwrap().clone();
        assert!(log.iter().all(|line| !line.starts_with("GET /connected_accounts") || line.contains("user_ids=apiosk_user_00000000-0000-4000-8000-000000000001")));
        // Found or created once per toolkit, then cached; Google Chat's refused create is the second.
        assert_eq!(log.iter().filter(|l| l.starts_with("POST /auth_configs")).count(), 2);
        assert!(log.iter().any(|l| l.starts_with("POST /auth_configs") && l.contains("use_composio_managed_auth") && l.contains("notion")));
        let links: Vec<&String> = log.iter().filter(|l| l.starts_with("POST /connected_accounts/link")).collect();
        assert_eq!(links.len(), 4);
        assert!(links[0].contains(r#""auth_config_id":"ac_notion""#) && links[0].contains(r#""user_id":"apiosk_user_00000000-0000-4000-8000-000000000001""#));
        assert!(links[0].contains(r#""callback_url":"https://app.apiosk.com/integrations?integration=notion""#));
        // Composio's own login wins over a hand-made one; Telegram uses the one set up by hand.
        assert!(links[2].contains(r#""auth_config_id":"ac_slack""#));
        assert!(links[3].contains(r#""auth_config_id":"ac_bot_token""#));
        // Never somebody else's account, even when the filter was ignored.
        assert_eq!(log.iter().filter(|l| l.starts_with("DELETE")).cloned().collect::<Vec<_>>(), ["DELETE ca_mine", "DELETE ca_old"]);
    }

    #[tokio::test]
    async fn only_actions_of_connected_apps_are_offered_and_run() {
        let (base, log) = fake_composio("ak_test").await;
        let composio = Composio::new("ak_test".into(), base, "https://app.apiosk.com".into(), HashMap::new());
        let owner: Uuid = "00000000-0000-4000-8000-000000000001".parse().unwrap();

        // Notion is the only connected app in the fake; its deprecated, read and delete tools are dropped.
        let actions = composio.actions(owner).await.unwrap();
        assert_eq!(actions.iter().map(|a| a.slug.as_str()).collect::<Vec<_>>(), ["app.notion.createNotionPage"]);
        composio.actions(owner).await.unwrap();

        let outcome = composio.execute(owner, "app.notion.createNotionPage", &json!({"title": "Coolblue"})).await.unwrap();
        assert!(outcome.successful);
        assert_eq!(outcome.data["id"], "501");
        assert!(matches!(composio.execute(owner, "app.slack.sendMessage", &json!({})).await, Err(ExecuteError::UnknownAction)));
        assert!(matches!(composio.execute(owner, "app.notion.createNotionPage", &json!("text")).await, Err(ExecuteError::InvalidArguments)));

        let log = log.lock().unwrap().clone();
        // Too few featured write tools, so the rest is read once too; then the cache serves.
        assert_eq!(log.iter().filter(|l| l.starts_with("GET /tools")).count(), 2);
        let runs: Vec<&String> = log.iter().filter(|l| l.starts_with("EXECUTE")).collect();
        assert_eq!(runs.len(), 1);
        assert!(runs[0].starts_with("EXECUTE NOTION_CREATE_NOTION_PAGE"));
        assert!(runs[0].contains(r#""user_id":"apiosk_user_00000000-0000-4000-8000-000000000001""#));
    }

    #[tokio::test]
    async fn a_refused_project_key_is_reported_as_unauthorized() {
        let (base, _) = fake_composio("ak_test").await;
        let composio = Composio::new("ck_consumer".into(), base, "https://app.apiosk.com".into(), HashMap::new());
        let owner = Uuid::new_v4();
        assert!(matches!(composio.connect(owner, "gmail").await, Err(ComposioError::Unauthorized)));
    }

    #[test]
    fn action_slugs_are_planner_slugs_under_the_app_domain() {
        assert_eq!(action_slug("slack", "SLACK_SEND_MESSAGE").as_deref(), Some("app.slack.sendMessage"));
        assert_eq!(action_slug("google_chat", "GOOGLE_CHAT_CREATE_MESSAGE").as_deref(), Some("app.google_chat.createMessage"));
        assert_eq!(action_slug("microsoft_teams", "MICROSOFT_TEAMS_SEND_MESSAGE_TO_CHANNEL").as_deref(), Some("app.microsoft_teams.sendMessageToChannel"));
        // Another toolkit's tool, or a slug the planner could not reuse exactly.
        assert_eq!(action_slug("slack", "GMAIL_SEND_EMAIL"), None);
        assert_eq!(action_slug("slack", "SLACK_"), None);
        assert_eq!(action_slug("slack", "SLACK_SEND-MESSAGE"), None);
    }

    #[test]
    fn only_non_destructive_write_tools_become_actions() {
        let none = Value::Null;
        for tool in ["SLACK_SEND_MESSAGE", "SLACK_CREATE_CHANNEL", "SLACK_UPLOAD_OR_CREATE_A_FILE_IN_SLACK"] {
            assert!(is_write_action("slack", tool, &none), "{tool}");
        }
        assert!(is_write_action("telegram", "TELEGRAM_SEND_MESSAGE", &none));
        assert!(is_write_action("discordbot", "DISCORDBOT_CREATE_MESSAGE", &none));
        assert!(is_write_action("notion", "NOTION_APPEND_TEXT_BLOCKS", &none));
        assert!(is_write_action("notion", "NOTION_INSERT_ROW_DATABASE", &none));
        assert!(is_write_action("microsoft_teams", "MICROSOFT_TEAMS_SEND_MESSAGE_TO_CHAT", &none));
        assert!(is_write_action("whatsapp", "WHATSAPP_SEND_MESSAGE", &none));
        assert!(is_write_action("gmail", "GMAIL_CREATE_EMAIL_DRAFT", &none));
        assert!(is_write_action("outlook", "OUTLOOK_OUTLOOK_SEND_EMAIL", &none));
        // Reads would compete with source search; deletes never run from Ask.
        for (toolkit, tool) in [("slack", "SLACK_FETCH_CONVERSATION_HISTORY"), ("slack", "SLACK_FIND_USERS"), ("notion", "NOTION_FETCH_DATA"),
            ("notion", "NOTION_SEARCH_NOTION_PAGE"), ("notion", "NOTION_DELETE_BLOCK"), ("gmail", "GMAIL_MOVE_TO_TRASH"), ("gmail", "SLACK_SEND_MESSAGE")] {
            assert!(!is_write_action(toolkit, tool, &none), "{tool}");
        }
        assert!(!is_write_action("slack", "SLACK_SEND_MESSAGE", &json!(["important", "destructiveHint"])));
        assert!(!is_write_action("notion", "NOTION_CREATE_COMMENT", &json!(["readOnlyHint"])));
    }

    #[test]
    fn the_catalog_lists_every_tool_and_marks_what_ask_uses() {
        let tools = vec![
            json!({"slug":"SLACK_FIND_USERS","name":"Find users","toolkit":{"slug":"slack"},"description":"Finds users."}),
            json!({"slug":"SLACK_SEND_MESSAGE","name":"Send message","toolkit":{"slug":"slack"},"human_description":"Posts a message."}),
            json!({"slug":"SLACK_OLD","name":"Old","toolkit":{"slug":"slack"},"is_deprecated":true}),
            json!({"slug":"SLACK_ARCHIVE_CHANNEL","name":"archive channel","toolkit":{"slug":"slack"}}),
            json!({"slug":"SLACK_SEND_MESSAGE","name":"Send message","toolkit":{"slug":"slack"}}),
        ];
        let offered = vec![action_from_tool("slack", &tools[1]).unwrap()];
        let listed = toolkit_catalog("slack", &tools, &offered);
        assert_eq!(listed.iter().map(|a| a.tool.as_str()).collect::<Vec<_>>(), ["SLACK_SEND_MESSAGE", "SLACK_ARCHIVE_CHANNEL", "SLACK_FIND_USERS"]);
        assert!(listed[0].in_ask && listed[0].slug.as_deref() == Some("app.slack.sendMessage"));
        assert_eq!(listed[0].description, "Posts a message.");
        assert!(listed[1..].iter().all(|a| !a.in_ask && a.slug.is_none()));
    }

    #[test]
    fn a_tool_becomes_an_action_with_its_required_inputs() {
        let tool = json!({
            "slug": "SLACK_SEND_MESSAGE", "name": "Send message", "toolkit": {"slug": "slack"},
            "description": "Posts a message to a channel.",
            "input_parameters": {"type": "object", "properties": {"channel": {"type": "string"}, "markdown_text": {"type": "string"}, "thread_ts": {"type": "string"}}, "required": ["channel", "markdown_text"]},
            "output_parameters": {"properties": {"data": {"properties": {"ts": {}, "channel": {}}}}}
        });
        let action = action_from_tool("slack", &tool).unwrap();
        assert_eq!(action.slug, "app.slack.sendMessage");
        assert_eq!(action.domain, "app");
        assert_eq!(action.tool, "SLACK_SEND_MESSAGE");
        assert_eq!(action.inputs, ["channel", "markdown_text"]);
        assert_eq!(action.outputs, ["channel", "ts"]);
        assert_eq!(action.input_schema["required"], json!(["channel", "markdown_text"]));

        let mut deprecated = tool.clone();
        deprecated["is_deprecated"] = json!(true);
        assert!(action_from_tool("slack", &deprecated).is_none());
        assert!(action_from_tool("gmail", &tool).is_none());
    }

    #[test]
    fn proposed_arguments_keep_schema_properties_and_report_what_is_missing() {
        let schema = json!({"type":"object","properties":{"channel":{},"markdown_text":{},"thread_ts":{}},"required":["channel","markdown_text"]});
        let checked = crate::parser::action_arguments(r#"{"arguments":{"markdown_text":"KvK 12345678: Mollie B.V.","channel":"","invented":"x"},"missing":[]}"#, &schema).unwrap();
        assert_eq!(checked, json!({"arguments":{"markdown_text":"KvK 12345678: Mollie B.V."},"missing":["channel"]}));
        assert!(crate::parser::action_arguments("not json", &schema).is_err());
        assert!(crate::parser::action_arguments(r#"{"arguments":"x"}"#, &schema).is_err());
    }

    #[test]
    fn pinned_auth_configs_keep_only_known_toolkits_and_ids() {
        let pinned = pinned_auth_configs(Some(r#"{"google_chat":"ac_abc123","hubspot":"ac_x1234","slack":"nope","gmail":"ac_bad id"}"#));
        assert_eq!(pinned.len(), 1);
        assert_eq!(pinned.get("google_chat").map(String::as_str), Some("ac_abc123"));
        assert!(pinned_auth_configs(Some("not json")).is_empty());
        assert!(pinned_auth_configs(None).is_empty());
    }
}
