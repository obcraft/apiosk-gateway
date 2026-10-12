//! Repeatable delivery contracts. Discovery happens at preparation only; a run
//! addresses the reviewed tool version and account directly. The hash detects
//! damaged stored contracts, not authorization: contracts must come from the
//! server's approved order, never from an untrusted execute request.
use super::{
    action_slug, clipped, composio_user_id, ActionOutcome, Composio, ComposioError, ExecuteError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenAction {
    pub format_version: u32,
    pub owner: Uuid,
    pub slug: String,
    pub toolkit: String,
    pub tool: String,
    pub tool_version: String,
    pub connected_account_id: String,
    pub input_schema: Value,
    pub fixed_arguments: Value,
    pub mutable_content_fields: Vec<String>,
    pub schema_hash: String,
    pub contract_hash: String,
}

/// Deliberately limited to the delivery channels whose result renderer is
/// implemented. This is separate from the broader, interactive action catalog.
fn delivery_tool(slug: &str) -> Option<(&'static str, &'static str)> {
    match slug {
        "app.slack.sendMessage" => Some(("slack", "SLACK_SEND_MESSAGE")),
        "app.slack.chatPostMessage" => Some(("slack", "SLACK_CHAT_POST_MESSAGE")),
        "app.gmail.sendEmail" => Some(("gmail", "GMAIL_SEND_EMAIL")),
        _ => None,
    }
}

pub(super) fn valid_account_id(id: &str) -> bool {
    id.starts_with("ca_")
        && (4..=100).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn valid_version(version: &str) -> bool {
    version.len() == 11
        && version.as_bytes()[8] == b'_'
        && version
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 8 || b.is_ascii_digit())
}

fn hash(value: &Value) -> String {
    // Canonical object order also survives a JSONB round trip.
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(object) => {
                let keys: std::collections::BTreeMap<_, _> = object.iter().collect();
                Value::Object(
                    keys.into_iter()
                        .map(|(k, v)| (k.clone(), sorted(v)))
                        .collect(),
                )
            }
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            value => value.clone(),
        }
    }
    hex::encode(Sha256::digest(
        serde_json::to_vec(&sorted(value)).expect("JSON value"),
    ))
}

fn arguments_size(arguments: &Value) -> Result<(), ExecuteError> {
    if !arguments.is_object()
        || serde_json::to_vec(arguments).map_or(true, |bytes| bytes.len() > 64 * 1024)
    {
        return Err(ExecuteError::InvalidArguments);
    }
    Ok(())
}

fn validate_schema(schema: &Value, arguments: &Value) -> Result<(), ExecuteError> {
    arguments_size(arguments)?;
    if schema["type"] != "object"
        || !schema["properties"].is_object()
        || serde_json::to_vec(schema).map_or(true, |bytes| bytes.len() > 256 * 1024)
    {
        return Err(ExecuteError::InvalidSchema);
    }
    // No network or filesystem resolution: the crate's resolver features are
    // disabled. Local $defs/$ref and nullable anyOf remain fully validated.
    let validator = jsonschema::options()
        .should_validate_formats(true)
        .build(schema)
        .map_err(|_| ExecuteError::InvalidSchema)?;
    if !validator.is_valid(arguments) {
        return Err(ExecuteError::SchemaArguments);
    }
    // A permissive provider schema must not turn typos or unknown recipient
    // fields into silently accepted delivery arguments.
    if arguments
        .as_object()
        .unwrap()
        .keys()
        .any(|key| schema["properties"].get(key).is_none())
    {
        return Err(ExecuteError::SchemaArguments);
    }
    Ok(())
}

fn validate_destination(toolkit: &str, arguments: &Value) -> Result<(), ExecuteError> {
    let valid = match toolkit {
        "slack" => arguments["channel"].as_str().is_some_and(|id| {
            (2..=100).contains(&id.len())
                && id.starts_with(['C', 'G', 'D'])
                && id.bytes().all(|b| b.is_ascii_alphanumeric())
        }),
        "gmail" => arguments["recipient_email"].as_str().is_some_and(|email| {
            let mut parts = email.split('@');
            let local = parts.next().unwrap_or_default();
            let domain = parts.next().unwrap_or_default();
            !local.is_empty()
                && !domain.is_empty()
                && parts.next().is_none()
                && email.len() <= 254
                && !email
                    .chars()
                    .any(|c| c.is_whitespace() || c.is_control() || ",;<>".contains(c))
        }),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(ExecuteError::SchemaArguments)
    }
}

fn content_fields(toolkit: &str, arguments: &Value) -> Result<Vec<String>, ExecuteError> {
    let fields: &[&str] = if toolkit == "slack" {
        &["text", "markdown_text"]
    } else {
        &["subject", "body"]
    };
    let selected: Vec<String> = fields
        .iter()
        .filter(|field| arguments.get(**field).is_some())
        .map(|field| (*field).to_owned())
        .collect();
    if selected.is_empty()
        || selected.iter().any(|field| !arguments[field].is_string())
        || (toolkit == "gmail" && !selected.iter().any(|field| field == "body"))
    {
        return Err(ExecuteError::SchemaArguments);
    }
    Ok(selected)
}

impl FrozenAction {
    fn calculated_hash(&self) -> String {
        let mut value = serde_json::to_value(self).expect("frozen action JSON");
        value.as_object_mut().unwrap().remove("contract_hash");
        hash(&value)
    }

    pub fn validate(&self, owner: Uuid) -> Result<(), ExecuteError> {
        let Some((toolkit, tool)) = delivery_tool(&self.slug) else {
            return Err(ExecuteError::InvalidContract);
        };
        if self.format_version != 1
            || self.owner != owner
            || self.toolkit != toolkit
            || self.tool != tool
            || !valid_version(&self.tool_version)
            || !valid_account_id(&self.connected_account_id)
            || self.schema_hash != hash(&self.input_schema)
            || self.contract_hash != self.calculated_hash()
            || self.mutable_content_fields != content_fields(toolkit, &self.fixed_arguments)?
        {
            return Err(ExecuteError::InvalidContract);
        }
        validate_schema(&self.input_schema, &self.fixed_arguments)?;
        validate_destination(toolkit, &self.fixed_arguments)
    }

    /// Render a new saved result using only the content fields present when
    /// this destination was reviewed. Every other argument is copied intact.
    pub fn arguments(&self, subject: &str, content: &str) -> Value {
        let mut arguments = self.fixed_arguments.clone();
        if let Some(object) = arguments.as_object_mut() {
            for field in &self.mutable_content_fields {
                object.insert(
                    field.clone(),
                    json!(if field == "subject" { subject } else { content }),
                );
            }
        }
        arguments
    }

    fn validate_arguments(&self, arguments: &Value) -> Result<(), ExecuteError> {
        validate_schema(&self.input_schema, arguments)?;
        validate_destination(&self.toolkit, arguments)?;
        let fixed = self
            .fixed_arguments
            .as_object()
            .ok_or(ExecuteError::InvalidContract)?;
        let supplied = arguments
            .as_object()
            .ok_or(ExecuteError::InvalidArguments)?;
        if fixed.len() != supplied.len()
            || fixed
                .iter()
                .any(|(field, value)| match supplied.get(field) {
                    Some(given) if self.mutable_content_fields.contains(field) => {
                        !given.is_string()
                    }
                    Some(given) => given != value,
                    None => true,
                })
        {
            return Err(ExecuteError::SchemaArguments);
        }
        Ok(())
    }
}

fn valid_slack_destination_id(id: &str) -> bool {
    (2..=100).contains(&id.len())
        && id.starts_with(['C', 'G'])
        && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn slack_destination(channel: &Value) -> Option<Value> {
    let id = channel["id"]
        .as_str()
        .filter(|id| valid_slack_destination_id(id))?;
    // Public channels can be listed without membership. Only offer existing
    // conversations this account can participate in; never silently join one.
    if channel["is_archived"] == true
        || channel["is_member"] == false
        || (channel["is_member"] != true && channel["is_mpim"] != true)
    {
        return None;
    }
    let kind = if channel["is_mpim"] == true {
        "group"
    } else if channel["is_private"] == true {
        "private_channel"
    } else {
        "channel"
    };
    let label = channel["name"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(id);
    Some(json!({"id":id,"label":clipped(label, 200),"kind":kind}))
}

impl Composio {
    /// Read conversation metadata only; never join a channel or send a message.
    pub async fn slack_destinations(
        &self,
        owner: Uuid,
        account_id: &str,
        cursor: Option<&str>,
    ) -> Result<Value, ExecuteError> {
        self.require_owned_account(owner, "slack", account_id)
            .await?;
        if cursor.is_some_and(|c| c.len() > 2048 || c.contains(['\r', '\n'])) {
            return Err(ExecuteError::InvalidArguments);
        }
        let mut arguments = json!({"limit":200,"types":"public_channel,private_channel,mpim","exclude_archived":true});
        if let Some(cursor) = cursor.filter(|c| !c.is_empty()) {
            arguments["cursor"] = json!(cursor);
        }
        let data = self
            .self_slack_tool(owner, account_id, "SLACK_LIST_ALL_CHANNELS", arguments)
            .await?;
        let channels = data["channels"]
            .as_array()
            .ok_or(ExecuteError::InvalidContract)?;
        let destinations: Vec<_> = channels.iter().filter_map(slack_destination).collect();
        Ok(
            json!({"destinations":destinations,"next_cursor":data["response_metadata"]["next_cursor"].as_str().unwrap_or("")}),
        )
    }

    pub async fn resolve_slack_destination(
        &self,
        owner: Uuid,
        account_id: &str,
        destination: &str,
    ) -> Result<Value, ExecuteError> {
        if !valid_slack_destination_id(destination) {
            return Err(ExecuteError::InvalidArguments);
        }
        self.require_owned_account(owner, "slack", account_id)
            .await?;
        let data = self
            .self_slack_tool(
                owner,
                account_id,
                "SLACK_RETRIEVE_CONVERSATION_INFORMATION",
                json!({"channel":destination}),
            )
            .await?;
        let channel =
            slack_destination(&data["channel"]).ok_or(ExecuteError::AccountUnavailable)?;
        if channel["id"] != destination {
            return Err(ExecuteError::InvalidContract);
        }
        Ok(channel)
    }

    /// Resolve the selected person's own Slack DM once, before the delivery
    /// contract is frozen. Neither step sends a message or reads conversation
    /// content; provider user IDs never come from the client or an account alias.
    pub async fn resolve_self_slack_recipient(
        &self,
        owner: Uuid,
        account_id: &str,
    ) -> Result<String, ExecuteError> {
        self.require_owned_account(owner, "slack", account_id)
            .await?;
        let identity = self
            .self_slack_tool(owner, account_id, "SLACK_TEST_AUTH", json!({}))
            .await?;
        let user = identity["user_id"]
            .as_str()
            .filter(|id| {
                (2..=100).contains(&id.len())
                    && id.starts_with(['U', 'W'])
                    && id.bytes().all(|b| b.is_ascii_alphanumeric())
            })
            .ok_or(ExecuteError::AccountUnavailable)?;
        // A bot token does not identify the person who connected this account.
        if identity.get("bot_id").is_some_and(|id| !id.is_null()) {
            return Err(ExecuteError::AccountUnavailable);
        }
        let opened = self
            .self_slack_tool(
                owner,
                account_id,
                "SLACK_OPEN_DM",
                json!({"users":user,"return_im":true}),
            )
            .await?;
        let channel = &opened["channel"];
        let id = channel["id"]
            .as_str()
            .filter(|id| {
                (2..=100).contains(&id.len())
                    && id.starts_with('D')
                    && id.bytes().all(|b| b.is_ascii_alphanumeric())
            })
            .ok_or(ExecuteError::InvalidContract)?;
        if channel["user"] != user
            || channel["is_im"] != true
            || channel["is_mpim"] == true
            || channel["is_channel"] == true
        {
            return Err(ExecuteError::InvalidContract);
        }
        Ok(id.to_owned())
    }

    async fn self_slack_tool(
        &self,
        owner: Uuid,
        account_id: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, ExecuteError> {
        let metadata = self
            .call(
                reqwest::Method::GET,
                &format!("/tools/{tool}?version=latest"),
                None,
            )
            .await?;
        if metadata["slug"] != tool
            || metadata["toolkit"]["slug"] != "slack"
            || metadata["is_deprecated"] == true
            || metadata["deprecated"]["is_deprecated"] == true
        {
            return Err(ExecuteError::UnknownAction);
        }
        let version = metadata["version"]
            .as_str()
            .filter(|version| valid_version(version))
            .ok_or(ExecuteError::InvalidContract)?;
        validate_schema(&metadata["input_parameters"], &arguments)?;
        let result = self
            .call(
                reqwest::Method::POST,
                &format!("/tools/execute/{tool}"),
                Some(json!({
                    "user_id":composio_user_id(owner),"connected_account_id":account_id,
                    "version":version,"arguments":arguments,
                })),
            )
            .await?;
        if result["successful"] != true || result["data"]["ok"] != true {
            return Err(ExecuteError::AccountUnavailable);
        }
        Ok(result["data"].clone())
    }

    async fn require_owned_account(
        &self,
        owner: Uuid,
        toolkit: &str,
        account_id: &str,
    ) -> Result<(), ExecuteError> {
        if !valid_account_id(account_id) {
            return Err(ExecuteError::AccountUnavailable);
        }
        let account = match self
            .call(
                reqwest::Method::GET,
                &format!("/connected_accounts/{account_id}"),
                None,
            )
            .await
        {
            Err(ComposioError::Status(404 | 410)) => return Err(ExecuteError::AccountUnavailable),
            result => result?,
        };
        if account["id"].as_str() != Some(account_id)
            || account["user_id"] != composio_user_id(owner)
            || account["toolkit"]["slug"] != toolkit
            || account["status"] != "ACTIVE"
            || account["is_disabled"].as_bool() == Some(true)
        {
            return Err(ExecuteError::AccountUnavailable);
        }
        Ok(())
    }

    /// Read-only preparation: resolves the exact version once and validates
    /// the explicitly selected owner/account. No OAuth or tool action occurs.
    pub async fn prepare_frozen_action(
        &self,
        owner: Uuid,
        slug: &str,
        connected_account_id: &str,
        arguments: &Value,
    ) -> Result<FrozenAction, ExecuteError> {
        arguments_size(arguments)?;
        let (toolkit, tool) = delivery_tool(slug).ok_or(ExecuteError::UnknownAction)?;
        self.require_owned_account(owner, toolkit, connected_account_id)
            .await?;
        let metadata = self
            .call(
                reqwest::Method::GET,
                &format!("/tools/{tool}?version=latest"),
                None,
            )
            .await?;
        if metadata["slug"] != tool
            || metadata["toolkit"]["slug"] != toolkit
            || metadata["is_deprecated"] == true
            || metadata["deprecated"]["is_deprecated"] == true
            || !super::is_write_action(toolkit, tool, &metadata["tags"])
            || action_slug(toolkit, tool).as_deref() != Some(slug)
        {
            return Err(ExecuteError::UnknownAction);
        }
        let version = metadata["version"]
            .as_str()
            .filter(|version| valid_version(version))
            .ok_or(ExecuteError::InvalidContract)?;
        let schema = metadata["input_parameters"].clone();
        let mut arguments = arguments.clone();
        // The current interactive renderer uses `text`; some pinned Slack
        // versions name that same plain content slot `markdown_text`. Resolve
        // this before approval, then freeze the actual field in the contract.
        if toolkit == "slack"
            && schema["properties"].get("text").is_none()
            && schema["properties"].get("markdown_text").is_some()
            && arguments.get("markdown_text").is_none()
        {
            if let Some(value) = arguments.as_object_mut().unwrap().remove("text") {
                arguments["markdown_text"] = value;
            }
        }
        validate_schema(&schema, &arguments)?;
        validate_destination(toolkit, &arguments)?;
        let mut contract = FrozenAction {
            format_version: 1,
            owner,
            slug: slug.to_owned(),
            toolkit: toolkit.to_owned(),
            tool: tool.to_owned(),
            tool_version: version.to_owned(),
            connected_account_id: connected_account_id.to_owned(),
            schema_hash: hash(&schema),
            input_schema: schema,
            mutable_content_fields: content_fields(toolkit, &arguments)?,
            fixed_arguments: arguments,
            contract_hash: String::new(),
        };
        contract.contract_hash = contract.calculated_hash();
        Ok(contract)
    }

    /// Read-only preflight before paid source calls. Confirms local integrity
    /// and the exact account, without discovery, OAuth or tool execution.
    pub async fn check_frozen(
        &self,
        owner: Uuid,
        contract: &FrozenAction,
    ) -> Result<(), ExecuteError> {
        contract.validate(owner)?;
        self.require_owned_account(owner, &contract.toolkit, &contract.connected_account_id)
            .await
    }

    /// Recheck before sending as well as before purchasing source data. A
    /// connection can expire while those source calls are in flight.
    pub async fn execute_frozen(
        &self,
        owner: Uuid,
        contract: &FrozenAction,
        arguments: &Value,
    ) -> Result<ActionOutcome, ExecuteError> {
        contract.validate(owner)?;
        contract.validate_arguments(arguments)?;
        self.require_owned_account(owner, &contract.toolkit, &contract.connected_account_id)
            .await?;
        let result = self.call(reqwest::Method::POST, &format!("/tools/execute/{}", contract.tool), Some(json!({
            "user_id": composio_user_id(owner), "connected_account_id": contract.connected_account_id,
            "version": contract.tool_version, "arguments": arguments,
        }))).await?;
        Ok(ActionOutcome {
            slug: contract.slug.clone(),
            toolkit: delivery_tool(&contract.slug).unwrap().0,
            successful: result["successful"].as_bool().unwrap_or(false),
            data: result["data"].clone(),
            error: result["error"].as_str().map(|e| clipped(e, 500)),
            log_id: result["log_id"].as_str().map(str::to_owned),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{OriginalUri, Path, State},
        routing::{get, post},
        Json, Router,
    };
    use std::sync::{Arc, Mutex};

    const OWNER: &str = "00000000-0000-4000-8000-000000000001";
    fn owner() -> Uuid {
        OWNER.parse().unwrap()
    }

    #[derive(Clone)]
    struct Provider {
        account: Arc<Mutex<Value>>,
        tool: Arc<Mutex<Value>>,
        identity: Arc<Mutex<Value>>,
        self_dm: Arc<Mutex<Value>>,
        conversation: Arc<Mutex<Value>>,
        calls: Arc<Mutex<Vec<(String, Value)>>>,
        execution_status: Arc<Mutex<axum::http::StatusCode>>,
    }

    async fn fake() -> (Composio, Provider) {
        let state = Provider {
            account: Arc::new(Mutex::new(json!({
                "id":"ca_selected", "user_id":composio_user_id(owner()),
                "toolkit":{"slug":"slack"}, "status":"ACTIVE", "is_disabled":false,
            }))),
            tool: Arc::new(Mutex::new(json!({
                "slug":"SLACK_SEND_MESSAGE", "name":"Send message", "version":"20261008_00",
                "toolkit":{"slug":"slack"}, "tags":["createHint"],
                "input_parameters":{
                    "type":"object", "properties":{
                        "channel":{"type":"string"}, "text":{"type":"string","minLength":1},
                        "thread_ts":{"anyOf":[{"type":"string"},{"type":"null"}]},
                    }, "required":["channel","text"], "additionalProperties":false,
                },
            }))),
            calls: Arc::new(Mutex::new(Vec::new())),
            identity: Arc::new(Mutex::new(json!({"ok":true,"user_id":"U12345"}))),
            self_dm: Arc::new(Mutex::new(
                json!({"ok":true,"channel":{"id":"D12345","user":"U12345","is_im":true}}),
            )),
            conversation: Arc::new(Mutex::new(json!({"id":"GTEAM","name":"team-chat","is_mpim":true,"is_member":true}))),
            execution_status: Arc::new(Mutex::new(axum::http::StatusCode::OK)),
        };
        let app = Router::new()
            .route("/connected_accounts/:id", get(|State(s): State<Provider>, Path(id): Path<String>| async move {
                s.calls.lock().unwrap().push((format!("GET /connected_accounts/{id}"), Value::Null));
                Json(s.account.lock().unwrap().clone())
            }))
            .route("/tools/:slug", get(|State(s): State<Provider>, Path(slug): Path<String>, uri: OriginalUri| async move {
                s.calls.lock().unwrap().push((format!("GET {}", uri.0), Value::Null));
                Json(match slug.as_str() {
                    "SLACK_LIST_ALL_CHANNELS" => json!({"slug":slug,"toolkit":{"slug":"slack"},"version":"20261008_00","input_parameters":{"type":"object","properties":{"limit":{"type":"integer"},"types":{"type":"string"},"exclude_archived":{"type":"boolean"},"cursor":{"type":"string"}},"additionalProperties":false}}),
                    "SLACK_RETRIEVE_CONVERSATION_INFORMATION" => json!({"slug":slug,"toolkit":{"slug":"slack"},"version":"20261008_00","input_parameters":{"type":"object","properties":{"channel":{"type":"string"}},"required":["channel"],"additionalProperties":false}}),
                    "SLACK_TEST_AUTH" => json!({"slug":slug,"toolkit":{"slug":"slack"},"version":"20261008_00","input_parameters":{"type":"object","properties":{}}}),
                    "SLACK_OPEN_DM" => json!({"slug":slug,"toolkit":{"slug":"slack"},"version":"20261008_00","input_parameters":{"type":"object","properties":{"users":{"type":"string"},"return_im":{"type":"boolean"}},"required":["users"]}}),
                    _ => s.tool.lock().unwrap().clone(),
                })
            }))
            .route("/tools/execute/:slug", post(|State(s): State<Provider>, Path(slug): Path<String>, Json(body): Json<Value>| async move {
                s.calls.lock().unwrap().push((format!("POST /tools/execute/{slug}"), body));
                let status = *s.execution_status.lock().unwrap();
                let data = match slug.as_str() {
                    "SLACK_LIST_ALL_CHANNELS" => json!({"ok":true,"channels":[s.conversation.lock().unwrap().clone(),{"id":"CARCHIVED","is_member":true,"is_archived":true},{"id":"COTHER","is_member":false}],"response_metadata":{"next_cursor":"page-two"}}),
                    "SLACK_RETRIEVE_CONVERSATION_INFORMATION" => json!({"ok":true,"channel":s.conversation.lock().unwrap().clone()}),
                    "SLACK_TEST_AUTH" => s.identity.lock().unwrap().clone(),
                    "SLACK_OPEN_DM" => s.self_dm.lock().unwrap().clone(),
                    _ => json!({"ts":"saved_message"}),
                };
                (status, Json(json!({"successful":true,"data":data,"log_id":"log_pinned"})))
            })).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (
            Composio::new(
                "fake-key".to_owned(),
                base,
                "https://example.com".to_owned(),
                Default::default(),
            ),
            state,
        )
    }

    #[tokio::test]
    async fn destinations_are_owned_paginated_and_checked_without_sending() {
        let (client, provider) = fake().await;
        let page = client.slack_destinations(owner(), "ca_selected", Some("cursor-one")).await.unwrap();
        assert_eq!(page["destinations"], json!([{"id":"GTEAM","label":"team-chat","kind":"group"}]));
        assert_eq!(page["next_cursor"], "page-two");
        let selected = client.resolve_slack_destination(owner(), "ca_selected", "GTEAM").await.unwrap();
        assert_eq!(selected["id"], "GTEAM");
        assert!(provider.calls.lock().unwrap().iter().any(|(path, body)| path.ends_with("SLACK_LIST_ALL_CHANNELS") && body["arguments"]["cursor"] == "cursor-one"));
        assert!(!provider.calls.lock().unwrap().iter().any(|(path, _)| path.contains("SEND_MESSAGE") || path.contains("OPEN_DM")));
        provider.conversation.lock().unwrap()["is_archived"] = json!(true);
        assert!(client.resolve_slack_destination(owner(), "ca_selected", "GTEAM").await.is_err());
        provider.account.lock().unwrap()["user_id"] = json!("another-owner");
        let count = provider.calls.lock().unwrap().len();
        assert!(client.slack_destinations(owner(), "ca_selected", None).await.is_err());
        assert_eq!(provider.calls.lock().unwrap().len(), count + 1, "ownership checked before provider tools");
    }

    #[tokio::test]
    async fn conversation_resolution_rejects_mismatches_and_nonmembers() {
        let (client, provider) = fake().await;
        assert!(client.resolve_slack_destination(owner(), "ca_selected", "CDIFFERENT").await.is_err());
        assert!(client.resolve_slack_destination(owner(), "ca_selected", "#team-chat").await.is_err());
        assert!(client.resolve_slack_destination(owner(), "ca_selected", "DUSER").await.is_err());
        provider.conversation.lock().unwrap()["is_member"] = json!(false);
        assert!(client.resolve_slack_destination(owner(), "ca_selected", "GTEAM").await.is_err());
    }

    #[tokio::test]
    async fn resolves_only_the_owned_personal_slack_dm_without_sending() {
        let (client, provider) = fake().await;
        assert_eq!(
            client
                .resolve_self_slack_recipient(owner(), "ca_selected")
                .await
                .unwrap(),
            "D12345"
        );
        let calls = provider.calls.lock().unwrap();
        let executions: Vec<_> = calls
            .iter()
            .filter(|(path, _)| path.starts_with("POST"))
            .collect();
        assert_eq!(executions.len(), 2);
        assert_eq!(executions[0].0, "POST /tools/execute/SLACK_TEST_AUTH");
        assert_eq!(executions[1].0, "POST /tools/execute/SLACK_OPEN_DM");
        assert_eq!(
            executions[1].1["arguments"],
            json!({"users":"U12345","return_im":true})
        );
        for (_, body) in executions {
            assert_eq!(body["user_id"], composio_user_id(owner()));
            assert_eq!(body["connected_account_id"], "ca_selected");
            assert_eq!(body["version"], "20261008_00");
        }
    }

    #[tokio::test]
    async fn self_delivery_rejects_bots_other_accounts_and_nonself_destinations() {
        let (client, provider) = fake().await;
        provider.account.lock().unwrap()["user_id"] = json!("someone-else");
        assert!(client
            .resolve_self_slack_recipient(owner(), "ca_selected")
            .await
            .is_err());
        assert_eq!(provider.calls.lock().unwrap().len(), 1);
        provider.account.lock().unwrap()["user_id"] = json!(composio_user_id(owner()));
        provider.identity.lock().unwrap()["bot_id"] = json!("B12345");
        assert!(client
            .resolve_self_slack_recipient(owner(), "ca_selected")
            .await
            .is_err());
        assert!(!provider
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(path, _)| path.contains("OPEN_DM")));
        provider
            .identity
            .lock()
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("bot_id");
        for invalid in [
            json!({"id":"C12345","user":"U12345","is_im":true}),
            json!({"id":"D12345","user":"UOTHER","is_im":true}),
            json!({"id":"D12345","user":"U12345","is_im":false}),
            json!({"id":"D12345","user":"U12345","is_im":true,"is_mpim":true}),
        ] {
            provider.self_dm.lock().unwrap()["channel"] = invalid;
            assert!(client
                .resolve_self_slack_recipient(owner(), "ca_selected")
                .await
                .is_err());
        }
    }

    fn args() -> Value {
        json!({"channel":"C123", "text":"Initial saved result"})
    }

    async fn prepared(client: &Composio) -> FrozenAction {
        client
            .prepare_frozen_action(owner(), "app.slack.sendMessage", "ca_selected", &args())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn frozen_execution_pins_version_account_and_schema_without_rediscovery() {
        let (client, provider) = fake().await;
        let contract = prepared(&client).await;
        assert_eq!(contract.tool_version, "20261008_00");
        assert!(!provider
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(path, _)| path.starts_with("POST")));
        provider.tool.lock().unwrap()["version"] = json!("20261101_00");
        provider.tool.lock().unwrap()["input_parameters"]["required"] = json!(["new_field"]);
        provider.calls.lock().unwrap().clear();
        let roundtrip: FrozenAction =
            serde_json::from_value(serde_json::to_value(contract).unwrap()).unwrap();
        let rendered = roundtrip.arguments("unused subject", "New saved result");
        let result = client
            .execute_frozen(owner(), &roundtrip, &rendered)
            .await
            .unwrap();
        assert!(result.successful);
        assert_eq!(result.log_id.as_deref(), Some("log_pinned"));
        let calls = provider.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "GET /connected_accounts/ca_selected");
        assert_eq!(calls[1].0, "POST /tools/execute/SLACK_SEND_MESSAGE");
        assert_eq!(
            calls[1].1,
            json!({"user_id":composio_user_id(owner()), "connected_account_id":"ca_selected", "version":"20261008_00", "arguments":{"channel":"C123","text":"New saved result"}})
        );
    }

    #[tokio::test]
    async fn preparation_rejects_foreign_inactive_disabled_or_wrong_toolkit_accounts() {
        for (field, value) in [
            ("user_id", json!("someone_else")),
            ("status", json!("EXPIRED")),
            ("is_disabled", json!(true)),
            ("toolkit", json!({"slug":"gmail"})),
            ("id", json!("ca_different")),
        ] {
            let (client, provider) = fake().await;
            provider.account.lock().unwrap()[field] = value;
            assert!(
                matches!(
                    client
                        .prepare_frozen_action(
                            owner(),
                            "app.slack.sendMessage",
                            "ca_selected",
                            &args()
                        )
                        .await,
                    Err(ExecuteError::AccountUnavailable)
                ),
                "{field}"
            );
            assert_eq!(provider.calls.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn reconnection_never_selects_another_active_account() {
        let (client, provider) = fake().await;
        let contract = prepared(&client).await;
        provider.account.lock().unwrap()["id"] = json!("ca_new_connection");
        provider.calls.lock().unwrap().clear();
        assert!(matches!(
            client.execute_frozen(owner(), &contract, &args()).await,
            Err(ExecuteError::AccountUnavailable)
        ));
        let calls = provider.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "GET /connected_accounts/ca_selected");
    }

    #[tokio::test]
    async fn paid_run_preflight_checks_exact_account_without_sending() {
        let (client, provider) = fake().await;
        let contract = prepared(&client).await;
        provider.calls.lock().unwrap().clear();
        client.check_frozen(owner(), &contract).await.unwrap();
        provider.account.lock().unwrap()["status"] = json!("EXPIRED");
        assert!(matches!(
            client.check_frozen(owner(), &contract).await,
            Err(ExecuteError::AccountUnavailable)
        ));
        let calls = provider.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls
            .iter()
            .all(|(path, _)| path == "GET /connected_accounts/ca_selected"));
    }

    #[tokio::test]
    async fn gmail_rendering_changes_only_subject_and_body() {
        let (client, provider) = fake().await;
        provider.account.lock().unwrap()["toolkit"]["slug"] = json!("gmail");
        *provider.tool.lock().unwrap() = json!({
            "slug":"GMAIL_SEND_EMAIL", "version":"20261008_00", "toolkit":{"slug":"gmail"},
            "input_parameters":{"type":"object", "properties":{
                "recipient_email":{"type":"string","format":"email"}, "subject":{"type":"string"},
                "body":{"type":"string"}, "from_email":{"type":"string","format":"email"}
            },"required":["recipient_email","subject","body"]}
        });
        let contract = client.prepare_frozen_action(owner(), "app.gmail.sendEmail", "ca_selected", &json!({
            "recipient_email":"recipient@example.com", "subject":"Before", "body":"Old saved result", "from_email":"sender@example.com"
        })).await.unwrap();
        let arguments = contract.arguments("New subject", "New saved result");
        assert_eq!(
            arguments,
            json!({"recipient_email":"recipient@example.com", "subject":"New subject", "body":"New saved result", "from_email":"sender@example.com"})
        );
        client
            .execute_frozen(owner(), &contract, &arguments)
            .await
            .unwrap();
        let mut redirected = arguments;
        redirected["recipient_email"] = json!("other@example.com");
        assert!(matches!(
            client.execute_frozen(owner(), &contract, &redirected).await,
            Err(ExecuteError::SchemaArguments)
        ));
        let calls = provider.calls.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .filter(|(path, _)| path.starts_with("POST"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn writes_are_not_automatically_retried_after_provider_errors() {
        for status in [
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            axum::http::StatusCode::BAD_GATEWAY,
        ] {
            let (client, provider) = fake().await;
            let contract = prepared(&client).await;
            provider.calls.lock().unwrap().clear();
            *provider.execution_status.lock().unwrap() = status;
            assert!(
                matches!(client.execute_frozen(owner(), &contract, &args()).await, Err(ExecuteError::Provider(ComposioError::Status(code))) if code == status.as_u16())
            );
            assert_eq!(
                provider
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(path, _)| path.starts_with("POST"))
                    .count(),
                1
            );
        }
    }

    #[tokio::test]
    async fn changed_destination_unapproved_fields_and_bad_content_never_reach_provider() {
        let (client, provider) = fake().await;
        let contract = prepared(&client).await;
        provider.calls.lock().unwrap().clear();
        for arguments in [
            json!({"channel":"C_OTHER","text":"new"}),
            json!({"channel":"C123","text":"new","thread_ts":"123"}),
            json!({"channel":"C123","text":22}),
            json!({"channel":"C123"}),
        ] {
            assert!(matches!(
                client.execute_frozen(owner(), &contract, &arguments).await,
                Err(ExecuteError::SchemaArguments)
            ));
        }
        assert!(provider.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn contract_owner_or_hash_mismatch_never_reaches_provider() {
        let (client, provider) = fake().await;
        let mut contract = prepared(&client).await;
        provider.calls.lock().unwrap().clear();
        assert!(matches!(
            client
                .execute_frozen(Uuid::new_v4(), &contract, &args())
                .await,
            Err(ExecuteError::InvalidContract)
        ));
        contract.fixed_arguments["channel"] = json!("C_OTHER");
        assert!(matches!(
            client.execute_frozen(owner(), &contract, &args()).await,
            Err(ExecuteError::InvalidContract)
        ));
        assert!(provider.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn concrete_versions_and_valid_complete_schemas_are_required() {
        for version in ["latest", "", "2026/10/08", "../version"] {
            let (client, provider) = fake().await;
            provider.tool.lock().unwrap()["version"] = json!(version);
            assert!(matches!(
                client
                    .prepare_frozen_action(owner(), "app.slack.sendMessage", "ca_selected", &args())
                    .await,
                Err(ExecuteError::InvalidContract)
            ));
        }
        let (client, provider) = fake().await;
        provider.tool.lock().unwrap()["input_parameters"]["required"] =
            json!(["channel", "text", "thread_ts"]);
        assert!(matches!(
            client
                .prepare_frozen_action(owner(), "app.slack.sendMessage", "ca_selected", &args())
                .await,
            Err(ExecuteError::SchemaArguments)
        ));
        provider.tool.lock().unwrap()["input_parameters"] =
            json!({"type":"object", "properties":{"text":{"$ref":"file:///etc/passwd"}}});
        assert!(matches!(
            client
                .prepare_frozen_action(owner(), "app.slack.sendMessage", "ca_selected", &args())
                .await,
            Err(ExecuteError::InvalidSchema)
        ));
    }

    #[tokio::test]
    async fn slack_content_alias_is_resolved_once_from_the_schema_before_approval() {
        let (client, provider) = fake().await;
        let mut metadata = provider.tool.lock().unwrap();
        metadata["input_parameters"]["properties"]
            .as_object_mut()
            .unwrap()
            .remove("text");
        metadata["input_parameters"]["properties"]["markdown_text"] = json!({"type":"string"});
        metadata["input_parameters"]["required"] = json!(["channel", "markdown_text"]);
        drop(metadata);
        let contract = prepared(&client).await;
        assert_eq!(contract.mutable_content_fields, vec!["markdown_text"]);
        assert_eq!(
            contract.arguments("subject", "saved"),
            json!({"channel":"C123", "markdown_text":"saved"})
        );
    }

    #[test]
    fn local_refs_nullable_fields_and_invalid_types_are_validated() {
        let schema = json!({"type":"object", "$defs":{"nullable_text":{"anyOf":[{"type":"string"},{"type":"null"}]}}, "properties":{"value":{"$ref":"#/$defs/nullable_text"}}, "required":["value"]});
        assert!(validate_schema(&schema, &json!({"value":null})).is_ok());
        assert!(validate_schema(&schema, &json!({"value":"text"})).is_ok());
        assert!(matches!(
            validate_schema(&schema, &json!({"value":7})),
            Err(ExecuteError::SchemaArguments)
        ));
    }

    #[test]
    fn invalid_destinations_and_hosted_accounts_are_refused() {
        for destination in ["", "#general", "C12\n3", "C12 3", "user@example.com"] {
            assert!(validate_destination("slack", &json!({"channel":destination})).is_err());
        }
        for email in ["", "user", "a@@b", "a@b,c@d", "a@b\r\nBcc:x@y", "a@ b"] {
            assert!(validate_destination("gmail", &json!({"recipient_email":email})).is_err());
        }
        for id in ["", "hosted_account", "ca_../x", "ca_foo?user_id=x"] {
            assert!(!valid_account_id(id));
        }
    }
}
