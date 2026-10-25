//! Immutable recurring workflows: ordered purchased calls, finite or per-run
//! funding, durable occurrences, and an independent delivery outbox.
use crate::{auth::Principal, engine::Engine, state::digest};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

mod dates;
pub mod delivery_edit;
mod worker;
pub mod manual;

mod external_inputs;
mod processing;
mod recipe;
mod schedule;
pub use schedule::Schedule;

fn prepaid() -> String {
    "prepaid".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedWorkflow {
    pub version: u32,
    pub fingerprint: String,
    pub recipe: recipe::Recipe,
    pub delivery: crate::integrations::FrozenAction,
    pub processing: String,
}
impl LockedWorkflow {
    fn fingerprint(&self, configuration: &Value, sources: &[Source], question: &str) -> String {
        digest(
            &json!({"version":self.version,"recipe":self.recipe,"delivery":self.delivery,"processing":self.processing,"configuration":configuration,"sources":sources,"question":question}),
        )
    }
    fn validate(
        &self,
        owner: Uuid,
        configuration: &Value,
        sources: &[Source],
        question: &str,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.version == 1 && matches!(self.processing.as_str(), processing::VERSION | crate::monitoring::VERSION),
            "Unsupported saved workflow version."
        );
        anyhow::ensure!(
            self.fingerprint == self.fingerprint(configuration, sources, question),
            "The approved workflow changed."
        );
        self.recipe.validate()?;
        if self.processing == crate::monitoring::VERSION {
            crate::monitoring::validate(configuration, sources)?;
        } else {
            anyhow::ensure!(configuration.get("monitoring").is_none(), "Unexpected monitoring configuration.");
        }
        self.delivery.validate(owner)?;
        anyhow::ensure!(
            sources.len() == self.recipe.steps.len()
                && sources
                    .iter()
                    .zip(&self.recipe.steps)
                    .all(|(source, step)| source.selection["id"]
                        == step.source_selection_id.to_string()),
            "The approved source order changed."
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    pub toolkit: String,
    pub destination: String,
    pub connected_account_id: String,
    pub recipient: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_label: Option<String>,
}
impl Delivery {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.recipient == "self"
                || (self.toolkit == "slack" && self.recipient == "conversation"),
            "Choose your own account or a Slack conversation."
        );
        anyhow::ensure!(
            matches!(self.toolkit.as_str(), "slack" | "gmail"),
            "Choose Slack or Gmail."
        );
        anyhow::ensure!(
            !self.destination.trim().is_empty()
                && self.destination.len() <= 254
                && !self.destination.contains(['\r', '\n']),
            "Enter a destination."
        );
        if self.toolkit == "slack" {
            anyhow::ensure!(
                (if self.recipient == "self" {
                    self.destination.starts_with('D')
                } else {
                    self.destination.starts_with(['C', 'G'])
                }) && (2..=100).contains(&self.destination.len())
                    && self.destination.chars().all(|c| c.is_ascii_alphanumeric()),
                "Choose an available Slack conversation."
            );
        } else {
            anyhow::ensure!(
                self.destination.contains('@')
                    && !self.destination.contains([' ', ',', ';', '<', '>']),
                "Enter one email address."
            );
        }
        Ok(())
    }
    pub fn slug(&self) -> &'static str {
        if self.toolkit == "slack" {
            "app.slack.sendMessage"
        } else {
            "app.gmail.sendEmail"
        }
    }
    pub fn arguments(&self, subject: &str, content: &str) -> Value {
        if self.toolkit == "slack" {
            json!({"channel":self.destination,"text":content})
        } else {
            json!({"recipient_email":self.destination,"subject":subject,"body":content})
        }
    }

    fn validate_frozen(&self, action: &crate::integrations::FrozenAction) -> anyhow::Result<()> {
        self.validate()?;
        anyhow::ensure!(
            self.toolkit == action.toolkit
                && self.connected_account_id == action.connected_account_id
                && if self.toolkit == "slack" {
                    action.fixed_arguments["channel"] == self.destination
                } else {
                    action.fixed_arguments["recipient_email"] == self.destination
                },
            "The approved delivery destination changed."
        );
        Ok(())
    }
}

/// Self delivery is derived from account ownership. An explicit conversation
/// must be verified through that same owned account before it can be frozen.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryRequest {
    pub toolkit: String,
    pub connected_account_id: String,
    pub recipient: String,
    #[serde(default)]
    pub destination: Option<String>,
}

impl DeliveryRequest {
    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (self.recipient == "self"
                && self.destination.is_none()
                && matches!(self.toolkit.as_str(), "gmail" | "slack"))
                || (self.recipient == "conversation"
                    && self.toolkit == "slack"
                    && self
                        .destination
                        .as_deref()
                        .is_some_and(|id| (2..=100).contains(&id.len())
                            && id.starts_with(['C', 'G'])
                            && id.bytes().all(|b| b.is_ascii_alphanumeric()))),
            "Choose a connected account and delivery destination."
        );
        Ok(())
    }

    fn matches(&self, delivery: &Delivery) -> bool {
        self.recipient == delivery.recipient
            && self.toolkit == delivery.toolkit
            && self.connected_account_id == delivery.connected_account_id
            && (self.recipient == "self"
                || self.destination.as_deref() == Some(&delivery.destination))
    }
}

async fn validate_self_delivery(
    engine: &Engine,
    owner: Uuid,
    delivery: &Delivery,
    action: &crate::integrations::FrozenAction,
) -> anyhow::Result<()> {
    delivery.validate_frozen(action)?;
    if delivery.recipient == "conversation" {
        crate::integrations::configured()
            .ok_or_else(|| anyhow::anyhow!("Integrations unavailable."))?
            .resolve_slack_destination(owner, &delivery.connected_account_id, &delivery.destination)
            .await?;
    }
    if delivery.toolkit == "gmail" {
        let current: Option<String> = sqlx::query_scalar("select billing.recurring_self_email($1)")
            .bind(owner)
            .fetch_one(&engine.store.pool)
            .await?;
        anyhow::ensure!(
            current.as_deref() == Some(&delivery.destination),
            "Your account email changed. Review a new delivery plan."
        );
    }
    Ok(())
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub selection: Value,
    pub service: Option<crate::catalog::Service>,
    pub date_rules: Vec<dates::DateRule>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DateOverride {
    pub selection_id: Uuid,
    pub pointer: String,
    pub offset_seconds: Option<i64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuoteRequest {
    pub request_id: Uuid,
    pub task_ids: Vec<Uuid>,
    pub question: String,
    pub schedule: Schedule,
    pub delivery: DeliveryRequest,
    #[serde(default)]
    pub date_overrides: Vec<DateOverride>,
    #[serde(default = "prepaid")]
    pub billing_mode: String,
    #[serde(default)]
    pub change_only: bool,
}

pub fn enabled() -> bool {
    std::env::var("V2_EXECUTION_ENABLED").as_deref() == Ok("1")
        && std::env::var("RECURRING_ORDERS_ENABLED").as_deref() == Ok("1")
        && std::env::var("RECURRING_WORKER_SECRET").is_ok_and(|s| s.len() >= 32)
}

/// A private rollout is granted to immutable authenticated owners, never an
/// email or a browser flag. A partly malformed setting grants nobody access.
fn parse_allowed_owners(setting: Option<&str>) -> Vec<Uuid> {
    let Some(setting) = setting else {
        return Vec::new();
    };
    let parsed: Option<Vec<Uuid>> = setting
        .split(',')
        .map(|part| {
            let part = part.trim();
            if part.len() != 36 {
                return None;
            }
            part.parse::<Uuid>().ok().filter(|id| !id.is_nil())
        })
        .collect();
    parsed.unwrap_or_default()
}

pub(crate) fn allowed_owners() -> Vec<Uuid> {
    parse_allowed_owners(std::env::var("RECURRING_ALLOWED_OWNER_IDS").ok().as_deref())
}

pub fn owner_allowed(owner: Uuid) -> bool {
    allowed_owners().contains(&owner)
}

pub fn enabled_for(owner: Uuid) -> bool {
    enabled() && owner_allowed(owner)
}

/// Allowed owners can view and stop existing orders during an execution pause,
/// including in production. Spending and workers retain their enabled() checks;
/// reads and controls still enforce the stored owner and workspace.
pub fn management_enabled_for(owner: Uuid) -> bool {
    owner_allowed(owner)
}

fn require_owner(owner: Uuid) -> anyhow::Result<()> {
    anyhow::ensure!(
        owner_allowed(owner),
        "Recurring orders are not available for this account."
    );
    Ok(())
}

pub async fn quote(engine: &Engine, p: &Principal, b: QuoteRequest) -> anyhow::Result<Value> {
    quote_configured(engine, p, b, None).await
}

pub async fn quote_monitor(engine: &Engine, p: &Principal, b: QuoteRequest, company: crate::company_checks::Company, selected_fields: Vec<String>, source_ids: Vec<String>) -> anyhow::Result<Value> {
    anyhow::ensure!(b.task_ids.len() == 1 && b.change_only, "Choose one company and a supported recurring schedule.");
    let source_ids=crate::monitoring::sources(source_ids)?;
    let all_fields=selected_fields.is_empty();
    let mut baseline = crate::monitoring::baseline(engine, p, b.task_ids[0], &company,&source_ids).await?;
    let selected_fields:Vec<String> = crate::monitoring::selected_fields(selected_fields)?.into_iter()
        .filter(|field|!all_fields || baseline["field_coverage"].as_array().is_some_and(|rows|rows.iter().any(|c|c["field"]==*field && c["status"]=="checked"))).collect();
    anyhow::ensure!(!selected_fields.is_empty() && selected_fields.iter().all(|field|source_ids.iter().any(|source|source==crate::monitoring::field_source(field))),"Select available fields from the retrieved sources.");
    anyhow::ensure!(selected_fields.iter().all(|field|baseline["field_coverage"].as_array().is_some_and(|rows|rows.iter().any(|c|c["field"]==*field && c["status"]=="checked"))),"A selected field is unavailable in the purchased baseline. Select only verified available fields.");
    let source_ids:Vec<String>=source_ids.into_iter().filter(|source|source==crate::monitoring::PROVIDER || selected_fields.iter().any(|f|source==crate::monitoring::field_source(f))).collect();
    baseline["source_ids"]=json!(source_ids);
    if let Some(coverage)=baseline["coverage"].as_array_mut() {coverage.retain(|c| c["category"]=="registry" || source_ids.iter().any(|source|(source==crate::monitoring::SANCTIONS && c["category"]=="sanctions") || (source==crate::monitoring::GLEIF && c["category"]=="gleif")));}
    crate::monitoring::select(&mut baseline, &selected_fields);
    let task_id = b.task_ids[0];
    quote_configured(engine, p, b, Some(json!({"company":company,"baseline":baseline,"task_id":task_id,"selected_fields":selected_fields,"sources":source_ids}))).await
}

async fn quote_configured(engine: &Engine, p: &Principal, b: QuoteRequest, monitoring: Option<Value>) -> anyhow::Result<Value> {
    require_owner(p.owner)?;
    engine.store.check_spending(p).await?;
    anyhow::ensure!(
        matches!(b.billing_mode.as_str(), "prepaid" | "per_run"),
        "Invalid budget mode."
    );
    let per_run = b.billing_mode == "per_run";
    b.schedule.validate(per_run)?;
    b.delivery.validate()?;
    anyhow::ensure!(
        !b.task_ids.is_empty() && b.task_ids.len() <= 20 && b.question.len() <= 16000,
        "Choose a completed order."
    );
    anyhow::ensure!(b.date_overrides.len() <= 100, "Too many date rules.");
    let mut config = json!({"schedule":b.schedule,"task_ids":b.task_ids,"date_overrides":b.date_overrides,"billing_mode":b.billing_mode,"change_only":b.change_only});
    if let Some(monitoring) = &monitoring { config["monitoring"] = monitoring.clone(); }
    let prior: Option<(Value,Value,String)>=sqlx::query_as("select configuration,delivery,question from billing.recurring_orders where id=$1 and owner_id=$2 and workspace_id is not distinct from $3")
  .bind(b.request_id).bind(p.owner).bind(p.workspace).fetch_optional(&engine.store.pool).await?;
    if let Some((old, delivery, question)) = prior {
        let delivery: Delivery = serde_json::from_value(delivery)?;
        anyhow::ensure!(
            old == config && b.delivery.matches(&delivery) && question == b.question,
            "Request ID already used."
        );
        return read(engine, p, b.request_id).await;
    }
    let valid_zone: bool =
        sqlx::query_scalar("select exists(select 1 from pg_timezone_names where name=$1)")
            .bind(&b.schedule.timezone)
            .fetch_one(&engine.store.pool)
            .await?;
    anyhow::ensure!(valid_zone, "Unknown timezone.");
    let today: NaiveDate = sqlx::query_scalar("select (now() at time zone $1)::date")
        .bind(&b.schedule.timezone)
        .fetch_one(&engine.store.pool)
        .await?;
    let calendar = b.schedule.dates(today, if per_run { 32 } else { 367 })?;
    anyhow::ensure!(
        per_run || calendar.len() <= 366,
        "Choose at most 366 occurrences."
    );
    let dates = utc_dates(engine, &b.schedule, &calendar).await?;
    anyhow::ensure!(!dates.is_empty(), "No future runs in this period.");
    if let Some(count) = b.schedule.count.filter(|_| !per_run) {
        anyhow::ensure!(
            dates.len() == count,
            "The start time has passed; choose a future start."
        );
    }
    let composio = crate::integrations::configured()
        .ok_or_else(|| anyhow::anyhow!("Integrations are unavailable."))?;
    let conversation = if b.delivery.recipient == "conversation" {
        Some(
            composio
                .resolve_slack_destination(
                    p.owner,
                    &b.delivery.connected_account_id,
                    b.delivery.destination.as_deref().unwrap_or_default(),
                )
                .await?,
        )
    } else {
        None
    };
    let destination = if let Some(conversation) = &conversation {
        conversation["id"].as_str().unwrap().to_owned()
    } else if b.delivery.toolkit == "gmail" {
        sqlx::query_scalar::<_, Option<String>>("select billing.recurring_self_email($1)")
            .bind(p.owner)
            .fetch_one(&engine.store.pool)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("Verify your account email before scheduling delivery.")
            })?
    } else {
        composio
            .resolve_self_slack_recipient(p.owner, &b.delivery.connected_account_id)
            .await?
    };
    let delivery = Delivery {
        toolkit: b.delivery.toolkit,
        connected_account_id: b.delivery.connected_account_id,
        recipient: b.delivery.recipient,
        destination,
        destination_label: conversation
            .as_ref()
            .and_then(|c| c["label"].as_str())
            .map(str::to_owned),
    };
    delivery.validate()?;
    let frozen_delivery = composio
        .prepare_frozen_action(
            p.owner,
            delivery.slug(),
            &delivery.connected_account_id,
            &delivery.arguments("Recurring result", "Result"),
        )
        .await?;
    let catalog = engine.catalog.fresh().await?;
    let mut sources = Vec::new();
    let mut recipes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for task_id in &b.task_ids {
        let (_, task, connection) = engine.store.load_scoped(p, *task_id).await?;
        anyhow::ensure!(
            connection.is_none(),
            "Only purchases made in Ask can recur."
        );
        let mut saved_plan = None;
        let mut completed_rows: Vec<(usize, Uuid, Value)> = Vec::new();
        let source_offset = sources.len();
        let ids: Vec<Uuid> = if let Some(ext) = &task.external_purchase {
            anyhow::ensure!(
                ext.approved_at.is_some() && ext.receipt.is_some(),
                "Wait for the result to finish."
            );
            vec![ext.selection_id]
        } else {
            let mut plan: crate::resolver::Plan = serde_json::from_value(
                task.proposal
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("Missing saved order."))?,
            )?;
            anyhow::ensure!(
                !task.cancelled
                    && (monitoring.is_some() || task.execution_error.is_none())
                    && (monitoring.is_some() || task.failed_steps.is_empty())
                    && task.pending_candidates.is_empty()
                    && task.pending_dispatches.is_empty()
                    && !task.result_refs.is_empty()
                    && plan
                        .steps
                        .iter()
                        .filter(|step| monitoring.as_ref().is_none_or(|m|m["selected_fields"].as_array().is_some_and(|fields|step.service.contract.capability_slug=="company.registry-profile" || fields.iter().filter_map(Value::as_str).any(|f|crate::monitoring::field_operation(f)==step.service.contract.capability_slug))))
                        .all(|step| task.step_finished(&step.key())),
                "Wait for all results to finish."
            );
            let mut rows=sqlx::query("select e.selection_id,e.response,s.candidate_id,s.capability,r.document #>> '{subject,entity_ref}' as entity_ref from executions e join authorizations a on a.id=e.authorization_id join actions.user_selected_apis s on s.id=e.selection_id join results r on r.execution_id=e.id and r.task_id=e.task_id and r.owner_id=e.owner_id where e.task_id=$1 and e.owner_id=$2 and a.quote_ref=$3 and e.status='succeeded'")
                .bind(task_id).bind(p.owner).bind(plan.quote_ref).fetch_all(&engine.store.pool).await?;
            if let Some(monitor)=&monitoring {
                let fields:Vec<String>=serde_json::from_value(monitor["selected_fields"].clone())?;
                let required=crate::monitoring::operations(&fields);
                plan.steps.retain(|step|required.contains(&step.service.contract.capability_slug));
                rows.retain(|row|row.get::<Option<String>,_>("capability").is_some_and(|cap|required.contains(&cap)));
            }
            let mut ids = Vec::new();
            for (index, step) in plan
                .steps
                .iter()
                .enumerate()
                .filter(|(_, s)| s.browser_fallback.is_none())
            {
                let matches: Vec<_> = rows
                    .iter()
                    .filter(|row| {
                        row.get::<Option<Uuid>, _>("candidate_id") == Some(step.service.endpoint_id)
                            && row.get::<Option<String>, _>("capability").as_deref()
                                == Some(step.service.contract.capability_slug.as_str())
                            && row.get::<Option<String>, _>("entity_ref")
                                == Some(step.entity_ref.to_string())
                    })
                    .collect();
                anyhow::ensure!(
                    matches.len() == 1,
                    "Every approved source needs one exact completed receipt."
                );
                let id: Uuid = matches[0].get("selection_id");
                let response: Value = matches[0].get("response");
                ids.push(id);
                completed_rows.push((index, id, response));
            }
            anyhow::ensure!(
                ids.len() == rows.len(),
                "An alternative source was executed; create a new explicit plan before repeating."
            );
            saved_plan = Some(plan);
            ids
        };
        anyhow::ensure!(!ids.is_empty(), "No completed source calls found.");
        for id in ids {
            anyhow::ensure!(seen.insert(id), "The same purchased step appears twice.");
            let mut selection:Value=sqlx::query_scalar("select jsonb_build_object('id',id,'user_id',user_id,'prompt',prompt,'kind',kind,'source',source,'name',name,'capability',capability,'api_slug',api_slug,'candidate_id',candidate_id,'resource_url',resource_url,'method',method,'provider_price_micro_usd',provider_price_micro_usd,'charged_micro_usd',charged_micro_usd,'metadata',metadata,'created_at',created_at) from actions.user_selected_apis where id=$1 and user_id=$2")
    .bind(id).bind(p.owner).fetch_one(&engine.store.pool).await?;
            if let Some(purchase) = &task.external_purchase {
                external_inputs::capture(&mut selection, purchase)?;
            }
            anyhow::ensure!(
                selection["metadata"]["inputs"].is_object(),
                "The saved source inputs are unavailable."
            );
            let service = if let Some(endpoint) = selection["candidate_id"].as_str() {
                let current = catalog
                    .services
                    .iter()
                    .find(|s| {
                        s.endpoint_id.to_string() == endpoint
                            && selection["capability"] == s.contract.capability_slug
                    })
                    .ok_or_else(|| anyhow::anyhow!("A source is no longer available."))?;
                let fee = selection["metadata"]["fee_bps"].as_i64().unwrap_or(1500);
                anyhow::ensure!(
                    current.list_atomic
                        == selection["provider_price_micro_usd"].as_i64().unwrap_or(-1)
                        && current.total_with(fee)
                            == selection["charged_micro_usd"].as_i64().unwrap_or(-1),
                    "A source price changed. Run and review the order again."
                );
                Some(current.clone())
            } else {
                None
            };
            let captured = selection["created_at"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing source timestamp."))?
                .parse::<DateTime<Utc>>()?;
            let local: NaiveDate =
                sqlx::query_scalar("select ($1::timestamptz at time zone $2)::date")
                    .bind(captured)
                    .bind(&b.schedule.timezone)
                    .fetch_one(&engine.store.pool)
                    .await?;
            let mut date_rules = dates::infer(
                &selection["metadata"]["inputs"],
                &b.question,
                captured,
                local,
            );
            for change in b
                .date_overrides
                .iter()
                .filter(|change| change.selection_id == id)
            {
                let mut candidate = dates::candidates(&selection["metadata"]["inputs"])
                    .into_iter()
                    .find(|v| v.pointer == change.pointer)
                    .ok_or_else(|| anyhow::anyhow!("Date rule does not match a saved input."))?;
                date_rules.retain(|v| v.pointer != change.pointer);
                if let Some(offset) = change.offset_seconds {
                    anyhow::ensure!(
                        (-31_622_400..=31_622_400).contains(&offset)
                            && offset % if candidate.date_only { 86400 } else { 60 } == 0,
                        "Invalid date offset."
                    );
                    candidate.offset_seconds = offset;
                    date_rules.push(candidate);
                }
            }
            sources.push(Source {
                selection,
                service,
                date_rules,
            });
        }
        if let Some(plan) = saved_plan {
            let completed: Vec<_> = completed_rows
                .into_iter()
                .map(|(plan_step, id, response)| recipe::CompletedStep {
                    plan_step,
                    source: sources[source_offset..]
                        .iter()
                        .find(|s| s.selection["id"] == id.to_string())
                        .unwrap()
                        .clone(),
                    response,
                })
                .collect();
            recipes.push(recipe::Recipe::compile(
                &plan,
                &task,
                &completed,
                &catalog.vocabulary,
            )?);
        } else {
            recipes.push(recipe::Recipe::fixed(&sources[source_offset..])?);
        }
    }
    anyhow::ensure!(
        b.date_overrides
            .iter()
            .all(|rule| seen.contains(&rule.selection_id)),
        "Date rule names an unknown source."
    );
    anyhow::ensure!(sources.len() <= 100, "Too many source calls.");
    let mut provider = 0i64;
    let mut total = 0i64;
    for s in &sources {
        let a = s.selection["charged_micro_usd"].as_i64().unwrap_or(0);
        let pr = s.selection["provider_price_micro_usd"]
            .as_i64()
            .unwrap_or(-1);
        anyhow::ensure!(a > 0 && pr >= 0 && a >= pr, "Invalid saved price.");
        provider = provider
            .checked_add(pr)
            .ok_or_else(|| anyhow::anyhow!("Price overflow."))?;
        total = total
            .checked_add(a)
            .ok_or_else(|| anyhow::anyhow!("Price overflow."))?;
    }
    let approved = if per_run {
        0
    } else {
        total
            .checked_mul(dates.len() as i64)
            .ok_or_else(|| anyhow::anyhow!("Price overflow."))?
    };
    let mut workflow = LockedWorkflow {
        version: 1,
        fingerprint: String::new(),
        recipe: recipe::Recipe::combine(&recipes)?,
        delivery: frozen_delivery,
        processing: if monitoring.is_some() {crate::monitoring::VERSION}else{processing::VERSION}.into(),
    };
    workflow.fingerprint = workflow.fingerprint(&config, &sources, &b.question);
    workflow.validate(p.owner, &config, &sources, &b.question)?;
    let baseline_hash = monitoring.as_ref().map(|m| crate::monitoring::comparison_hash(&m["baseline"]));
    let baseline_at = monitoring.as_ref().and_then(|m| m["baseline"]["checked_at"].as_str()).map(str::parse::<DateTime<Utc>>).transpose()?;
    sqlx::query("insert into billing.recurring_orders(id,owner_id,workspace_id,question,timezone,configuration,sources,delivery,dates,provider_per_run,total_per_run,approved_micro_usd,billing_mode,change_only,workflow,baseline_hash,baseline_at) values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)")
        .bind(b.request_id).bind(p.owner).bind(p.workspace).bind(b.question).bind(b.schedule.timezone).bind(config).bind(serde_json::to_value(sources)?).bind(serde_json::to_value(delivery)?).bind(dates).bind(provider).bind(total).bind(approved).bind(b.billing_mode).bind(b.change_only).bind(serde_json::to_value(workflow)?).bind(baseline_hash).bind(baseline_at).execute(&engine.store.pool).await?;
    read(engine, p, b.request_id).await
}

pub async fn read(engine: &Engine, p: &Principal, id: Uuid) -> anyhow::Result<Value> {
    require_owner(p.owner)?;
    engine.store.check_workspace(p).await?;
    // Aggregate the complete charge journal, independently of the 100-row run
    // history page. Keep balances and totals in the same database snapshot.
    // Keep the prepared result shape stable when a migration adds order columns.
    let row=sqlx::query(r#"select o.state,o.question,o.configuration,o.delivery,o.dates,
      o.billing_mode,o.change_only,o.workflow,o.provider_per_run,o.total_per_run,
      o.approved_micro_usd,o.reserved_micro_usd,o.released_micro_usd,o.expires_at,o.sources,
      (select balance_micro_usd::text from user_balances where account_id=coalesce(o.workspace_id,o.owner_id)) available,
      money.spent, money.pending, money.refunded
      from billing.recurring_orders o
      cross join lateral (
        select coalesce(sum(case when b.status='charged' then b.charged_micro_usd else 0 end),0)::text spent,
          coalesce(sum(case when b.status in ('charged','refunded') then 0 else c.allocated_micro_usd end),0)::text pending,
          coalesce(sum(case when b.status='refunded' then c.allocated_micro_usd when b.status='charged' then greatest(0,c.allocated_micro_usd-b.charged_micro_usd) else 0 end),0)::text refunded
        from billing.recurring_runs r join billing.recurring_calls c on c.run_id=r.id
        left join balance_charges b on b.user_id=o.owner_id and b.idempotency_key=c.id::text
        where r.order_id=o.id
      ) money
      where o.id=$1 and o.owner_id=$2 and o.workspace_id is not distinct from $3"#)
  .bind(id).bind(p.owner).bind(p.workspace).fetch_one(&engine.store.pool).await?;
    let runs:Vec<Value>=sqlx::query_scalar("select jsonb_build_object('id',r.id,'scheduled_at',r.scheduled_at,'state',r.state,'error',r.error,'has_result',r.result is not null and r.result<>'null'::jsonb,'delivery_state',coalesce(d.state,case when r.state='succeeded' and (select change_only from billing.recurring_orders where id=r.order_id) then 'unchanged' end),'spent_micro_usd',coalesce((select sum(b.charged_micro_usd) from billing.recurring_calls c join balance_charges b on b.user_id=$2 and b.idempotency_key=c.id::text and b.status='charged' where c.run_id=r.id),0)::text) from billing.recurring_runs r left join billing.recurring_deliveries d on d.run_id=r.id where r.order_id=$1 order by r.scheduled_at desc limit 100")
  .bind(id).bind(p.owner).fetch_all(&engine.store.pool).await?;
    let dates: Vec<DateTime<Utc>> = row.get("dates");
    let workflow: Option<Value> = row.get("workflow");
    let per_run = row.get::<String, _>("billing_mode") == "per_run";
    let snapshots: Vec<Source> = serde_json::from_value(row.get("sources"))?;
    let date_inputs:Vec<Value>=snapshots.iter().flat_map(|source|dates::candidates(&source.selection["metadata"]["inputs"]).into_iter().map(|input|json!({"selection_id":source.selection["id"],"source_name":source.selection["name"],"pointer":input.pointer,"original":input.original,"date_only":input.date_only,"offset_seconds":source.date_rules.iter().find(|r|r.pointer==input.pointer).map(|r|r.offset_seconds)}))).collect();
    let manual_enabled = manual::enabled();
    // Independent of the latest-100 history: finite schedules can be longer.
    let manual_run_id: Option<Uuid> = if manual_enabled && row.get::<String, _>("state") == "active" {
        sqlx::query_scalar("select id from (select id,state,scheduled_at from billing.recurring_runs where order_id=$1 and finished_at is null order by scheduled_at limit 1) next_run where state='queued' and scheduled_at>=now()-interval '1 hour'")
            .bind(id).fetch_optional(&engine.store.pool).await?
    } else { None };
    let manual_trigger = json!({"enabled":manual_enabled,"run_id":manual_run_id});
    Ok(
        json!({"id":id,"manual_trigger":manual_trigger,"state":row.get::<String,_>("state"),"question":row.get::<String,_>("question"),"configuration":row.get::<Value,_>("configuration"),"delivery":row.get::<Value,_>("delivery"),"dates":dates,"run_count":if per_run {Value::Null}else{json!(dates.len())},"billing_mode":row.get::<String,_>("billing_mode"),"change_only":row.get::<bool,_>("change_only"),"workflow":workflow.as_ref().map(|w|json!({"version":w["version"],"fingerprint":w["fingerprint"],"execution_mode":"sequential","processing":w["processing"]})),"provider_per_run":row.get::<i64,_>("provider_per_run").to_string(),"total_per_run":row.get::<i64,_>("total_per_run").to_string(),"total":if per_run {"0".to_owned()}else{row.get::<i64,_>("approved_micro_usd").to_string()},"reserved":row.get::<i64,_>("reserved_micro_usd").to_string(),"released":row.get::<i64,_>("released_micro_usd").to_string(),"spent":row.get::<String,_>("spent"),"pending":row.get::<String,_>("pending"),"refunded":row.get::<String,_>("refunded"),"funded":row.get::<i64,_>("approved_micro_usd").to_string(),"balance_available":row.get::<Option<String>,_>("available"),"expires_at":row.get::<DateTime<Utc>,_>("expires_at"),"sources":snapshots.iter().enumerate().map(|(ordinal,source)|json!({"ordinal":ordinal,"step_id":source.selection["id"],"api_slug":source.selection["api_slug"],"endpoint_id":source.selection["candidate_id"],"dependencies":workflow.as_ref().and_then(|w|w["recipe"]["steps"][ordinal]["bindings"].as_array()).map(|bindings|bindings.iter().map(|b|json!({"step":b["from_step"].as_u64().unwrap_or(0)+1,"field":b["fact_type"]})).collect::<Vec<_>>()).unwrap_or_default(),"name":source.selection["name"],"total_per_run":source.selection["charged_micro_usd"].as_i64().unwrap_or(0).to_string(),"usage_notice":source.service.as_ref().and_then(|service|service.usage_notice_view())})).collect::<Vec<_>>(),"date_inputs":date_inputs,"date_rules":row.get::<Value,_>("sources").as_array().map(|sources| sources.iter().flat_map(|source|source["date_rules"].as_array().into_iter().flatten().cloned()).collect::<Vec<_>>()).unwrap_or_default(),"runs":runs}),
    )
}

pub fn start(engine: Arc<Engine>) {
    if enabled() {
        worker::start(engine);
    }
}

pub async fn verify(engine: &Engine, p: &Principal, id: Uuid) -> anyhow::Result<()> {
    require_owner(p.owner)?;
    engine.store.check_workspace(p).await?;
    let row=sqlx::query("select workflow,configuration,sources,question,delivery from billing.recurring_orders where id=$1 and owner_id=$2 and workspace_id is not distinct from $3")
        .bind(id).bind(p.owner).bind(p.workspace).fetch_one(&engine.store.pool).await?;
    let workflow: LockedWorkflow = serde_json::from_value(
        row.get::<Option<Value>, _>("workflow")
            .ok_or_else(|| anyhow::anyhow!("Create a new reviewed workflow."))?,
    )?;
    let sources: Vec<Source> = serde_json::from_value(row.get("sources"))?;
    let delivery: Delivery = serde_json::from_value(row.get("delivery"))?;
    validate_self_delivery(engine, p.owner, &delivery, &workflow.delivery).await?;
    workflow.validate(
        p.owner,
        &row.get("configuration"),
        &sources,
        &row.get::<String, _>("question"),
    )?;
    let ids: Vec<Uuid> = workflow
        .recipe
        .steps
        .iter()
        .filter_map(|s| s.service.as_ref().map(|s| s.id))
        .collect();
    let catalog = engine.catalog.fresh_execution(&ids).await?;
    workflow.recipe.validate_catalog(&catalog.services)?;
    let composio = crate::integrations::configured()
        .ok_or_else(|| anyhow::anyhow!("Integrations unavailable."))?;
    composio.check_frozen(p.owner, &workflow.delivery).await?;
    Ok(())
}

#[doc(hidden)]
pub async fn run_one_pass(engine: &Arc<Engine>) -> anyhow::Result<()> {
    worker::pass(engine).await
}

#[doc(hidden)]
pub async fn deliver_one_pass(engine: &Engine) -> anyhow::Result<()> {
    worker::deliver(engine).await
}

pub async fn read_run(
    engine: &Engine,
    p: &Principal,
    order: Uuid,
    run: Uuid,
) -> anyhow::Result<Value> {
    require_owner(p.owner)?;
    engine.store.check_workspace(p).await?;
    let result:Value=sqlx::query_scalar("select r.result from billing.recurring_runs r join billing.recurring_orders o on o.id=r.order_id where r.id=$1 and o.id=$2 and o.owner_id=$3 and o.workspace_id is not distinct from $4 and r.result is not null").bind(run).bind(order).bind(p.owner).bind(p.workspace).fetch_one(&engine.store.pool).await?;
    Ok(result)
}

async fn utc_dates(
    engine: &Engine,
    schedule: &Schedule,
    dates: &[NaiveDate],
) -> anyhow::Result<Vec<DateTime<Utc>>> {
    Ok(sqlx::query_scalar("select distinct (d+$2::time) at time zone $3 from unnest($1::date[]) d where (d+$2::time) at time zone $3 > now()+interval '2 minutes' order by 1")
        .bind(dates).bind(schedule.time).bind(&schedule.timezone).fetch_all(&engine.store.pool).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_rollout_allowlist_fails_closed() {
        let owner = Uuid::new_v4();
        assert!(parse_allowed_owners(None).is_empty());
        for setting in [
            "",
            " ",
            "not-a-uuid",
            "00000000-0000-0000-0000-000000000000",
        ] {
            assert!(parse_allowed_owners(Some(setting)).is_empty());
        }
        for setting in [
            format!("{owner},invalid"),
            format!("{owner},"),
            format!(",{owner}"),
            owner.simple().to_string(),
            owner.urn().to_string(),
        ] {
            assert!(parse_allowed_owners(Some(&setting)).is_empty());
        }
        assert_eq!(
            parse_allowed_owners(Some(&format!(" {owner} "))),
            vec![owner]
        );
        let other = Uuid::new_v4();
        assert_eq!(
            parse_allowed_owners(Some(&format!("{owner},{other}"))),
            vec![owner, other]
        );
    }
    #[test]
    fn delivery_request_cannot_supply_another_recipient() {
        assert!(serde_json::from_value::<DeliveryRequest>(json!({
            "toolkit":"gmail","connected_account_id":"ca_self","recipient":"self",
            "destination":"someone-else@example.com"
        }))
        .unwrap()
        .validate()
        .is_err());
        let request: DeliveryRequest = serde_json::from_value(json!({
            "toolkit":"gmail","connected_account_id":"ca_self","recipient":"someone-else"
        }))
        .unwrap();
        assert!(request.validate().is_err());
        let request: DeliveryRequest = serde_json::from_value(json!({
            "toolkit":"gmail","connected_account_id":"ca_self","recipient":"self"
        }))
        .unwrap();
        assert!(request.validate().is_ok());
    }
    #[test]
    fn finite_schedule_and_destination() {
        let s = Schedule {
            start: "2026-10-01".parse().unwrap(),
            end: Some("2026-10-31".parse().unwrap()),
            frequency: "weekly".into(),
            interval: 1,
            count: None,
            time: "09:00:00".parse().unwrap(),
            timezone: "Europe/Amsterdam".into(),
            weekdays: vec![1, 2, 3, 4, 5],
        };
        assert!(s.validate(false).is_ok());
        let mut bad = s.clone();
        bad.weekdays = vec![0];
        assert!(bad.validate(false).is_err());
        bad = s;
        bad.end = Some("2026-01-01".parse().unwrap());
        assert!(bad.validate(false).is_err());
        assert!(Delivery {
            toolkit: "gmail".into(),
            destination: "a@b.com\nbcc:other".into(),
            connected_account_id: "ca_test".into(),
            recipient: "self".into(),
            destination_label: None,
        }
        .validate()
        .is_err());
    }
}
