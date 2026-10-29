//! Explicit trigger setup and trigger replies only. Does not add tools to Ask
//! or run its actions.
use super::{composio_user_id, frozen, owns_account, Composio, ComposioError};
use reqwest::Method;
use serde_json::{json, Value};
use uuid::Uuid;

pub const SLACK_TRIGGER: &str = "SLACK_CHANNEL_MESSAGE_RECEIVED";

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
            .await?;
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
            return Err(ComposioError::Invalid("slack_action_failed"));
        }
        Ok(value["data"].clone())
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

    pub async fn trigger_create_conversation(
        &self,
        owner: Uuid,
        account: &str,
        name: &str,
        users: &[String],
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
        Ok(
            json!({"id":id, "name":data["channel"]["name"].as_str().unwrap_or(name), "is_private":true,"is_mpim":!users.is_empty()}),
        )
    }

    /// A plain reply in the triggering message's thread, without link previews.
    pub async fn trigger_reply(
        &self,
        owner: Uuid,
        account: &str,
        channel: &str,
        thread_ts: &str,
        text: &str,
    ) -> Result<(), ComposioError> {
        self.trigger_tool(
            owner,
            account,
            "SLACK_SEND_MESSAGE",
            json!({
                "channel": channel, "markdown_text": text, "thread_ts": thread_ts,
                "unfurl_links": false, "unfurl_media": false
            }),
        )
        .await
        .map(|_| ())
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
