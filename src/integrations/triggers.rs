//! Explicit trigger setup and the Apiosk bot's replies only. Does not add
//! tools to Ask or run its actions.
use super::{composio_user_id, frozen, owns_account, Composio, ComposioError};
use reqwest::Method;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use uuid::Uuid;

pub const SLACK_TRIGGER: &str = "SLACK_CHANNEL_MESSAGE_RECEIVED";

/// Bot connection lookups reused for a minute. Each costs a Composio round
/// trip, and a Slack reply otherwise pays them again before every post.
const RECENT_FOR: Duration = Duration::from_secs(60);
type Recent<T> = Mutex<HashMap<String, (Instant, T)>>;
fn recent<T: Clone>(cache: &Recent<T>, key: &str) -> Option<T> {
    let cache = cache.lock().unwrap_or_else(|p| p.into_inner());
    cache.get(key).filter(|(at, _)| at.elapsed() < RECENT_FOR).map(|(_, v)| v.clone())
}
fn remember<T>(cache: &Recent<T>, key: String, value: T) {
    cache.lock().unwrap_or_else(|p| p.into_inner()).insert(key, (Instant::now(), value));
}

/// The Apiosk Slack app as a bot connection, one per workspace: Composio keeps
/// each workspace's bot token, and the auth config holds our own app's OAuth
/// client, so replies come from Apiosk and never from the person's account.
pub const APIOSK_BOT: &str = "slackbot";

/// `APIOSK_SLACKBOT_AUTH_CONFIG`. Never created or defaulted here: a
/// Composio-managed config would post as Composio's own app.
pub fn apiosk_bot_config() -> Option<String> {
    std::env::var("APIOSK_SLACKBOT_AUTH_CONFIG")
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|id| {
            id.starts_with("ac_")
                && (4..=67).contains(&id.len())
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
}

/// The Apiosk bot in one workspace. `user` is its Slack user, for recognising
/// "@Apiosk"; both are empty when Slack could not be asked.
#[derive(Clone, Debug)]
pub struct ApioskBot {
    pub account: String,
    pub team: String,
    pub user: String,
}

fn apiosk_bot_account(account: &Value, owner: Uuid, id: &str, config: &str) -> bool {
    owns_account(account, owner, APIOSK_BOT, id)
        && account["status"] == "ACTIVE"
        && account["is_disabled"] != true
        && account["auth_config"]["id"].as_str().is_none_or(|c| c == config)
}

/// Slack's own error code for a failed action, when it is one we explain.
fn slack_failure(value: &Value) -> &'static str {
    let said = [&value["data"]["error"], &value["error"]]
        .iter()
        .filter_map(|v| v.as_str().or(v["message"].as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    [
        "already_in_channel",
        "not_in_channel",
        "channel_not_found",
        "missing_scope",
        "user_not_found",
        "invalid_auth",
        "account_inactive",
        "token_revoked",
    ]
    .into_iter()
    .find(|code| said.contains(code))
    .unwrap_or("slack_action_failed")
}

impl Composio {
    pub async fn trigger_connect(
        &self,
        owner: Uuid,
        toolkit: &'static str,
        alias: Option<&str>,
    ) -> Result<Value, ComposioError> {
        if alias.is_some_and(|s| !super::valid_alias(s)) {
            return Err(ComposioError::InvalidAlias);
        }
        let config = self.auth_config(toolkit).await?;
        self.consent_link(owner, config, alias).await
    }

    async fn consent_link(&self, owner: Uuid, config: String, alias: Option<&str>) -> Result<Value, ComposioError> {
        let mut body = json!({
            "auth_config_id": config, "user_id": composio_user_id(owner),
            "callback_url": format!("{}/triggers", self.app_origin)
        });
        if let Some(alias) = alias {
            body["alias"] = json!(alias.trim());
        }
        let link = self
            .call(Method::POST, "/connected_accounts/link", Some(body))
            .await?;
        let redirect = link["redirect_url"]
            .as_str()
            .and_then(|s| url::Url::parse(s).ok())
            .filter(|u| u.scheme() == "https")
            .ok_or(ComposioError::Invalid("redirect_url"))?;
        Ok(json!({"redirect_url": redirect.as_str()}))
    }

    pub async fn trigger_account(&self, owner: Uuid, id: &str) -> Result<(), ComposioError> {
        if !frozen::valid_account_id(id) {
            return Err(ComposioError::AccountNotFound);
        }
        let account = self
            .call(Method::GET, &format!("/connected_accounts/{id}"), None)
            .await
            .map_err(|e| match e {
                ComposioError::Status(404) => ComposioError::AccountNotFound,
                other => other,
            })?;
        if !owns_account(&account, owner, "slack", id)
            || account["status"] != "ACTIVE"
            || account["is_disabled"] == true
        {
            return Err(ComposioError::AccountNotFound);
        }
        Ok(())
    }

    async fn trigger_tool(
        &self,
        owner: Uuid,
        id: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, ComposioError> {
        self.trigger_account(owner, id).await?;
        self.execute_slack(owner, id, tool, arguments).await
    }

    async fn execute_slack(
        &self,
        owner: Uuid,
        id: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, ComposioError> {
        let value = self
            .call(
                Method::POST,
                &format!("/tools/execute/{tool}"),
                Some(json!({
                    "user_id": composio_user_id(owner), "connected_account_id": id,
                    "arguments": arguments, "version": "20261023_00"
                })),
            )
            .await?;
        if value["successful"] != true || value["data"]["ok"] == false {
            return Err(ComposioError::Invalid(slack_failure(&value)));
        }
        Ok(value["data"].clone())
    }

    /// Consent to add Apiosk to the person's workspace.
    pub async fn apiosk_bot_connect(&self, owner: Uuid) -> Result<Value, ComposioError> {
        let config = apiosk_bot_config().ok_or(ComposioError::NeedsSetup)?;
        self.consent_link(owner, config, None).await
    }

    /// The owner's active Apiosk bot connections, newest first.
    pub async fn apiosk_bot_accounts(&self, owner: Uuid) -> Result<Vec<String>, ComposioError> {
        static RECENT: OnceLock<Recent<Vec<String>>> = OnceLock::new();
        let Some(config) = apiosk_bot_config() else {
            return Ok(Vec::new());
        };
        let cache = RECENT.get_or_init(Default::default);
        if let Some(ids) = recent(cache, &owner.to_string()) {
            return Ok(ids);
        }
        let ids: Vec<String> = self
            .accounts(&composio_user_id(owner))
            .await?
            .iter()
            .filter_map(|a| a["id"].as_str().filter(|id| apiosk_bot_account(a, owner, id, &config)))
            .map(str::to_owned)
            .collect();
        // None found is not kept, so a bot that was just added is seen at once.
        if !ids.is_empty() {
            remember(cache, owner.to_string(), ids.clone());
        }
        Ok(ids)
    }

    async fn apiosk_bot_tool(&self, owner: Uuid, id: &str, tool: &str, arguments: Value) -> Result<Value, ComposioError> {
        static VERIFIED: OnceLock<Recent<()>> = OnceLock::new();
        let config = apiosk_bot_config().ok_or(ComposioError::NeedsSetup)?;
        if !frozen::valid_account_id(id) {
            return Err(ComposioError::AccountNotFound);
        }
        let verified = VERIFIED.get_or_init(Default::default);
        let key = format!("{owner}:{id}");
        if recent(verified, &key).is_none() {
            let account = self.call(Method::GET, &format!("/connected_accounts/{id}"), None).await?;
            if !apiosk_bot_account(&account, owner, id, &config) {
                return Err(ComposioError::AccountNotFound);
            }
            remember(verified, key, ());
        }
        self.execute_slack(owner, id, tool, arguments).await
    }

    /// `auth.test` through Composio's proxy, cached per connection. Only called
    /// for connections `apiosk_bot_accounts` already matched to the owner.
    async fn apiosk_bot_identity(&self, id: &str) -> Result<ApioskBot, ComposioError> {
        static SEEN: OnceLock<Mutex<HashMap<String, ApioskBot>>> = OnceLock::new();
        let seen = SEEN.get_or_init(Default::default);
        if let Some(bot) = seen.lock().unwrap_or_else(|p| p.into_inner()).get(id) {
            return Ok(bot.clone());
        }
        let value = self
            .call(
                Method::POST,
                "/tools/execute/proxy",
                Some(json!({"endpoint": "https://slack.com/api/auth.test", "method": "POST", "connected_account_id": id})),
            )
            .await?;
        let data = if value["data"].is_object() { &value["data"] } else { &value };
        let field = |key: &str| {
            data[key]
                .as_str()
                .filter(|s| (3..=24).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric()))
                .map(str::to_owned)
        };
        let (Some(team), Some(user)) = (field("team_id"), field("user_id")) else {
            return Err(ComposioError::Invalid("bot_identity"));
        };
        let bot = ApioskBot { account: id.to_owned(), team, user };
        seen.lock().unwrap_or_else(|p| p.into_inner()).insert(id.to_owned(), bot.clone());
        Ok(bot)
    }

    /// The owner's Apiosk bot for a workspace, or the only one when the
    /// workspace is unknown or Slack cannot be asked.
    pub async fn apiosk_bot(&self, owner: Uuid, team: Option<&str>) -> Result<Option<ApioskBot>, ComposioError> {
        let accounts = self.apiosk_bot_accounts(owner).await?;
        for id in &accounts {
            match self.apiosk_bot_identity(id).await {
                Ok(bot) if team.is_none_or(|t| t == bot.team) => return Ok(Some(bot)),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "Apiosk bot identity unavailable"),
            }
        }
        Ok(match accounts.as_slice() {
            [only] if team.is_none() || self.apiosk_bot_identity(only).await.is_err() => {
                Some(ApioskBot { account: only.clone(), team: String::new(), user: String::new() })
            }
            _ => None,
        })
    }

    /// Posts as Apiosk without link previews and returns the message `ts`.
    /// `text` is mrkdwn; without `blocks` it becomes the only section.
    pub async fn apiosk_bot_post(
        &self,
        owner: Uuid,
        bot: &str,
        channel: &str,
        thread: Option<&str>,
        text: &str,
        blocks: Option<&Value>,
    ) -> Result<String, ComposioError> {
        let blocks = blocks
            .cloned()
            .unwrap_or_else(|| json!([{"type": "section", "text": {"type": "mrkdwn", "text": text}}]));
        let mut arguments = json!({
            "channel": channel, "blocks": blocks, "fallback_text": text,
            "unfurl_links": false, "unfurl_media": false
        });
        if let Some(thread) = thread {
            arguments["thread_ts"] = json!(thread);
        }
        let sent = self.apiosk_bot_tool(owner, bot, "SLACKBOT_SEND_MESSAGE", arguments).await?;
        sent["ts"].as_str().or(sent["message"]["ts"].as_str())
            .filter(|ts| !ts.is_empty()).map(str::to_owned)
            .ok_or(ComposioError::Invalid("missing_message_ts"))
    }

    /// Update our own channel notice after DM delivery. Uses only chat:write.
    pub async fn apiosk_bot_update(&self, owner: Uuid, bot: &str, channel: &str, ts: &str, text: &str, blocks: &Value) -> Result<(), ComposioError> {
        let config = apiosk_bot_config().ok_or(ComposioError::NeedsSetup)?;
        if !frozen::valid_account_id(bot) { return Err(ComposioError::AccountNotFound); }
        let account = self.call(Method::GET, &format!("/connected_accounts/{bot}"), None).await?;
        if !apiosk_bot_account(&account, owner, bot, &config) { return Err(ComposioError::AccountNotFound); }
        let value = self.call(Method::POST, "/tools/execute/proxy", Some(json!({
            "endpoint":"https://slack.com/api/chat.update", "method":"POST", "connected_account_id":bot,
            "body":{"channel":channel,"ts":ts,"text":text,"blocks":blocks}
        }))).await?;
        if value["data"]["ok"] != true { return Err(ComposioError::Invalid(slack_failure(&value))); }
        Ok(())
    }

    pub async fn trigger_channels(
        &self,
        owner: Uuid,
        id: &str,
        cursor: &str,
    ) -> Result<Value, ComposioError> {
        let data = self
            .trigger_tool(
                owner,
                id,
                "SLACK_LIST_CONVERSATIONS",
                json!({
                    "types": "public_channel,private_channel,mpim", "exclude_archived": true,
                    "limit": 200, "cursor": cursor
                }),
            )
            .await?;
        let channels: Vec<Value> = data["channels"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                json!({
                    "id": c["id"], "name": c["name"].as_str().unwrap_or("Group conversation"),
                    "is_private": c["is_private"].as_bool().unwrap_or(false),
                    "is_mpim": c["is_mpim"].as_bool().unwrap_or(false)
                })
            })
            .collect();
        Ok(
            json!({"channels": channels, "next_cursor": data["response_metadata"]["next_cursor"].as_str().unwrap_or("")}),
        )
    }

    pub async fn trigger_channel(
        &self,
        owner: Uuid,
        account: &str,
        channel: &str,
    ) -> Result<Value, ComposioError> {
        self.trigger_tool(
            owner,
            account,
            "SLACK_RETRIEVE_CONVERSATION_INFORMATION",
            json!({"channel":channel}),
        )
        .await
    }

    /// Invites the Apiosk bot of the channel's workspace with the person's own
    /// Slack connection, which also reaches their private channels. False when
    /// the bot was already a member.
    pub async fn trigger_invite_bot(&self, owner: Uuid, account: &str, channel: &str) -> Result<bool, ComposioError> {
        let info = self.trigger_channel(owner, account, channel).await?;
        let team = info["channel"]["context_team_id"].as_str().or(info["channel"]["team_id"].as_str());
        let bot = self
            .apiosk_bot(owner, team)
            .await?
            .filter(|b| !b.user.is_empty())
            .ok_or(ComposioError::NeedsSetup)?;
        match self.trigger_tool(owner, account, "SLACK_INVITE_USERS_TO_A_SLACK_CHANNEL", json!({"channel":channel,"users":bot.user})).await {
            Ok(_) => Ok(true),
            Err(ComposioError::Invalid("already_in_channel")) => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub async fn trigger_members(
        &self,
        owner: Uuid,
        account: &str,
        cursor: &str,
    ) -> Result<Value, ComposioError> {
        let data = self
            .trigger_tool(
                owner,
                account,
                "SLACK_LIST_ALL_USERS",
                json!({"limit":200,"cursor":cursor}),
            )
            .await?;
        let members: Vec<Value> = data["members"].as_array().into_iter().flatten()
            .filter(|m|m["deleted"]!=true && m["is_bot"]!=true && m["id"]!="USLACKBOT")
            .map(|m|json!({"id":m["id"],"name":m["profile"]["display_name"].as_str().filter(|s|!s.is_empty())
                .or_else(||m["real_name"].as_str()).or_else(||m["name"].as_str()).unwrap_or("Slack member")})).collect();
        Ok(
            json!({"members":members,"next_cursor":data["response_metadata"]["next_cursor"].as_str().unwrap_or("")}),
        )
    }

    /// A private channel (with an optional description and invitations) or a
    /// group chat. Setup after creation is best effort: the channel exists, so
    /// what failed is reported in `incomplete` instead of failing the request.
    pub async fn trigger_create_conversation(
        &self,
        owner: Uuid,
        account: &str,
        name: &str,
        users: &[String],
        description: &str,
        members: &[String],
    ) -> Result<Value, ComposioError> {
        let data = if users.is_empty() {
            self.trigger_tool(
                owner,
                account,
                "SLACK_CREATE_CHANNEL",
                json!({"name":name,"is_private":true}),
            )
            .await?
        } else {
            self.trigger_tool(
                owner,
                account,
                "SLACK_OPEN_DM",
                json!({"users":users.join(","),"return_im":true}),
            )
            .await?
        };
        let id = data["channel"]["id"]
            .as_str()
            .ok_or(ComposioError::Invalid("channel_id"))?;
        let mut incomplete = Vec::new();
        if users.is_empty() && !description.is_empty() {
            let set = self.trigger_tool(owner, account, "SLACK_SET_CONVERSATION_PURPOSE", json!({"channel":id,"purpose":description})).await;
            if let Err(e) = set {
                tracing::warn!(error = %e, "trigger channel description not set");
                incomplete.push("description");
            }
        }
        if users.is_empty() && !members.is_empty() {
            let invited = self.trigger_tool(owner, account, "SLACK_INVITE_USERS_TO_A_SLACK_CHANNEL", json!({"channel":id,"users":members.join(","),"force":true})).await;
            if let Err(e) = invited {
                tracing::warn!(error = %e, "trigger channel invitations not sent");
                incomplete.push("members");
            }
        }
        Ok(
            json!({"id":id, "name":data["channel"]["name"].as_str().unwrap_or(name), "is_private":true,"is_mpim":!users.is_empty(),"incomplete":incomplete}),
        )
    }

    pub async fn trigger_subscribe(
        &self,
        owner: Uuid,
        account: &str,
        channel: &str,
    ) -> Result<String, ComposioError> {
        self.trigger_account(owner, account).await?;
        let value = self
            .call(
                Method::POST,
                &format!("/trigger_instances/{SLACK_TRIGGER}/upsert"),
                Some(json!({
                    "user_id": composio_user_id(owner), "connected_account_id": account,
                    "trigger_config": {"channel_id": channel, "is_bot_message": false},
                    "toolkit_versions": {"slack":"20261023_00"}
                })),
            )
            .await?;
        value["trigger_id"]
            .as_str()
            .map(str::to_owned)
            .ok_or(ComposioError::Invalid("trigger_id"))
    }
}
