use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryEdit {
    pub expected_workflow_hash: String,
    pub delivery: DeliveryRequest,
}

/// Resolve a caller-owned destination and prepare its exact delivery contract.
/// This discovers metadata; it does not execute a source or send a message.
async fn prepare(e: &Engine, p: &Principal, request: DeliveryRequest) -> anyhow::Result<(Delivery, crate::integrations::FrozenAction)> {
    request.validate()?;
    let composio = crate::integrations::configured()
        .ok_or_else(|| anyhow::anyhow!("Integrations are unavailable."))?;
    let conversation = if request.recipient == "conversation" {
        Some(composio.resolve_slack_destination(p.owner, &request.connected_account_id, request.destination.as_deref().unwrap_or_default()).await?)
    } else { None };
    let destination = if let Some(conversation) = &conversation {
        conversation["id"].as_str().ok_or_else(|| anyhow::anyhow!("Destination unavailable."))?.to_owned()
    } else if request.toolkit == "gmail" {
        sqlx::query_scalar::<_, Option<String>>("select billing.recurring_self_email($1)")
            .bind(p.owner).fetch_one(&e.store.pool).await?
            .ok_or_else(|| anyhow::anyhow!("Verify your account email before scheduling delivery."))?
    } else {
        composio.resolve_self_slack_recipient(p.owner, &request.connected_account_id).await?
    };
    let delivery = Delivery {
        toolkit: request.toolkit,
        connected_account_id: request.connected_account_id,
        recipient: request.recipient,
        destination,
        destination_label: conversation.as_ref().and_then(|c| c["label"].as_str()).map(str::to_owned),
    };
    delivery.validate()?;
    let action = composio.prepare_frozen_action(p.owner, delivery.slug(), &delivery.connected_account_id, &delivery.arguments("Recurring result", "Result")).await?;
    delivery.validate_frozen(&action)?;
    Ok((delivery, action))
}

pub async fn update(e: &Engine, p: &Principal, id: Uuid, request: DeliveryEdit) -> anyhow::Result<Value> {
    require_owner(p.owner)?;
    anyhow::ensure!(enabled_for(p.owner), "Recurring orders are not available yet.");
    e.store.check_spending(p).await?;
    let row = sqlx::query("select state,delivery,workflow,configuration,sources,question from billing.recurring_orders where id=$1 and owner_id=$2 and workspace_id is not distinct from $3 and approved_at is not null")
        .bind(id).bind(p.owner).bind(p.workspace).fetch_one(&e.store.pool).await?;
    anyhow::ensure!(matches!(row.get::<String, _>("state").as_str(), "active" | "paused"), "Delivery can only be changed on an active or paused order.");
    let mut workflow: LockedWorkflow = serde_json::from_value(row.get::<Option<Value>, _>("workflow").ok_or_else(|| anyhow::anyhow!("Create a new reviewed workflow."))?)?;
    anyhow::ensure!(workflow.fingerprint == request.expected_workflow_hash, "The order changed. Refresh it before saving delivery.");
    let configuration: Value = row.get("configuration");
    let sources: Vec<Source> = serde_json::from_value(row.get("sources"))?;
    let question: String = row.get("question");
    workflow.validate(p.owner, &configuration, &sources, &question)?;
    let (delivery, action) = prepare(e, p, request.delivery).await?;
    // Saving an unchanged delivery must not create another revision.
    if serde_json::to_value(&delivery)? == row.get::<Value, _>("delivery") && workflow.delivery == action {
        return read(e, p, id).await;
    }
    workflow.delivery = action;
    workflow.fingerprint = workflow.fingerprint(&configuration, &sources, &question);
    workflow.validate(p.owner, &configuration, &sources, &question)?;
    e.store.check_spending(p).await?;
    let changed = sqlx::query("update billing.recurring_orders set delivery=$4,workflow=$5,delivery_revision=delivery_revision+1,baseline_hash=null,baseline_at=null,updated_at=now() where id=$1 and owner_id=$2 and workspace_id is not distinct from $3 and workflow->>'fingerprint'=$6 and state in ('active','paused')")
        .bind(id).bind(p.owner).bind(p.workspace).bind(serde_json::to_value(delivery)?)
        .bind(serde_json::to_value(workflow)?).bind(request.expected_workflow_hash)
        .execute(&e.store.pool).await?.rows_affected();
    anyhow::ensure!(changed == 1, "The order changed. Refresh it before saving delivery.");
    read(e, p, id).await
}
