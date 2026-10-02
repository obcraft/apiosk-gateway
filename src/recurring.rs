//! Finite recurring orders: immutable purchased calls, one funded mandate,
//! durable occurrences, and an independent delivery outbox.
use crate::{auth::Principal, engine::Engine, state::digest};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

mod dates;
mod worker;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub time: NaiveTime,
    pub timezone: String,
    pub weekdays: Vec<i32>,
}
impl Schedule {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.end >= self.start && (self.end - self.start).num_days() < 366,
            "Choose a period of at most one year."
        );
        anyhow::ensure!(
            !self.weekdays.is_empty()
                && self.weekdays.len() <= 7
                && self.weekdays.iter().all(|d| (1..=7).contains(d)),
            "Choose weekdays."
        );
        anyhow::ensure!(
            !self.timezone.is_empty() && self.timezone.len() <= 100,
            "Choose a timezone."
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    pub toolkit: String,
    pub destination: String,
}
impl Delivery {
    pub fn validate(&self) -> anyhow::Result<()> {
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
                self.destination.starts_with(['C', 'G', 'D'])
                    && self.destination.chars().all(|c| c.is_ascii_alphanumeric()),
                "Enter a Slack channel ID."
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
    pub delivery: Delivery,
    #[serde(default)]
    pub date_overrides: Vec<DateOverride>,
}

pub fn enabled() -> bool {
    std::env::var("V2_EXECUTION_ENABLED").as_deref() == Ok("1")
        && std::env::var("RECURRING_ORDERS_ENABLED").as_deref() == Ok("1")
        && std::env::var("RECURRING_WORKER_SECRET").is_ok_and(|s| s.len() >= 32)
}

pub async fn quote(engine: &Engine, p: &Principal, b: QuoteRequest) -> anyhow::Result<Value> {
    engine.store.check_spending(p).await?;
    b.schedule.validate()?;
    b.delivery.validate()?;
    anyhow::ensure!(
        !b.task_ids.is_empty() && b.task_ids.len() <= 20 && b.question.len() <= 16000,
        "Choose a completed order."
    );
    anyhow::ensure!(b.date_overrides.len() <= 100, "Too many date rules.");
    let config =
        json!({"schedule":b.schedule,"task_ids":b.task_ids,"date_overrides":b.date_overrides});
    let prior: Option<(Value,Value,String)>=sqlx::query_as("select configuration,delivery,question from billing.recurring_orders where id=$1 and owner_id=$2 and workspace_id is not distinct from $3")
  .bind(b.request_id).bind(p.owner).bind(p.workspace).fetch_optional(&engine.store.pool).await?;
    if let Some((old, delivery, question)) = prior {
        anyhow::ensure!(
            old == config
                && delivery == serde_json::to_value(&b.delivery)?
                && question == b.question,
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
    let dates:Vec<DateTime<Utc>>=sqlx::query_scalar("select distinct (d::date+$3::time) at time zone $4 from generate_series($1::date::timestamp,$2::date::timestamp,interval '1 day') d where extract(isodow from d)::int=any($5) and (d::date+$3::time) at time zone $4 > now()+interval '2 minutes' order by 1")
  .bind(b.schedule.start).bind(b.schedule.end).bind(b.schedule.time).bind(&b.schedule.timezone).bind(&b.schedule.weekdays).fetch_all(&engine.store.pool).await?;
    anyhow::ensure!(
        !dates.is_empty() && dates.len() <= 366,
        "No future runs in this period."
    );
    let composio = crate::integrations::configured()
        .ok_or_else(|| anyhow::anyhow!("Integrations are unavailable."))?;
    let actions = composio.actions(p.owner).await?;
    let action = crate::integrations::find_action(&actions, b.delivery.slug())
        .ok_or_else(|| anyhow::anyhow!("Connect the selected delivery app first."))?;
    // A changed integration contract requires implementation review, never guessed arguments.
    let args = b.delivery.arguments("Recurring result", "Result");
    if let Some(required) = action.input_schema["required"].as_array() {
        anyhow::ensure!(
            required
                .iter()
                .filter_map(Value::as_str)
                .all(|k| args.get(k).is_some()),
            "This delivery app requires additional setup."
        );
    }
    let catalog = engine.catalog.fresh().await?;
    let mut sources = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for task_id in &b.task_ids {
        let (_, task, connection) = engine.store.load_scoped(p, *task_id).await?;
        anyhow::ensure!(
            connection.is_none(),
            "Only purchases made in Ask can recur."
        );
        let ids: Vec<Uuid> = if let Some(ext) = &task.external_purchase {
            anyhow::ensure!(
                ext.approved_at.is_some() && ext.receipt.is_some(),
                "Wait for the result to finish."
            );
            vec![ext.selection_id]
        } else {
            let plan: crate::resolver::Plan = serde_json::from_value(
                task.proposal
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("Missing saved order."))?,
            )?;
            anyhow::ensure!(
                !task.cancelled
                    && task.execution_error.is_none()
                    && task.failed_steps.is_empty()
                    && task.pending_candidates.is_empty()
                    && task.pending_dispatches.is_empty()
                    && !task.result_refs.is_empty()
                    && plan
                        .steps
                        .iter()
                        .all(|step| task.step_finished(&step.key())),
                "Wait for all results to finish."
            );
            sqlx::query_scalar("select e.selection_id from executions e join authorizations a on a.id=e.authorization_id where e.task_id=$1 and e.owner_id=$2 and a.quote_ref=$3 and e.status='succeeded' order by e.created_at,e.id")
    .bind(task_id).bind(p.owner).bind(plan.quote_ref).fetch_all(&engine.store.pool).await?
        };
        anyhow::ensure!(!ids.is_empty(), "No completed source calls found.");
        for id in ids {
            if !seen.insert(id) {
                continue;
            }
            let selection:Value=sqlx::query_scalar("select jsonb_build_object('id',id,'user_id',user_id,'prompt',prompt,'kind',kind,'source',source,'name',name,'capability',capability,'api_slug',api_slug,'candidate_id',candidate_id,'resource_url',resource_url,'method',method,'provider_price_micro_usd',provider_price_micro_usd,'charged_micro_usd',charged_micro_usd,'metadata',metadata,'created_at',created_at) from actions.user_selected_apis where id=$1 and user_id=$2")
    .bind(id).bind(p.owner).fetch_one(&engine.store.pool).await?;
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
    let approved = total
        .checked_mul(dates.len() as i64)
        .ok_or_else(|| anyhow::anyhow!("Price overflow."))?;
    sqlx::query("insert into billing.recurring_orders(id,owner_id,workspace_id,question,timezone,configuration,sources,delivery,dates,provider_per_run,total_per_run,approved_micro_usd) values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
  .bind(b.request_id).bind(p.owner).bind(p.workspace).bind(b.question).bind(b.schedule.timezone).bind(config).bind(serde_json::to_value(sources)?).bind(serde_json::to_value(b.delivery)?).bind(dates).bind(provider).bind(total).bind(approved).execute(&engine.store.pool).await?;
    read(engine, p, b.request_id).await
}

pub async fn read(engine: &Engine, p: &Principal, id: Uuid) -> anyhow::Result<Value> {
    engine.store.check_workspace(p).await?;
    let row=sqlx::query("select o.*, (select balance_micro_usd::text from user_balances where account_id=coalesce(o.workspace_id,o.owner_id)) available from billing.recurring_orders o where id=$1 and owner_id=$2 and workspace_id is not distinct from $3")
  .bind(id).bind(p.owner).bind(p.workspace).fetch_one(&engine.store.pool).await?;
    let runs:Vec<Value>=sqlx::query_scalar("select jsonb_build_object('id',r.id,'scheduled_at',r.scheduled_at,'state',r.state,'error',r.error,'has_result',r.result is not null and r.result<>'null'::jsonb,'delivery_state',d.state,'spent_micro_usd',coalesce((select sum(b.charged_micro_usd) from billing.recurring_calls c join balance_charges b on b.user_id=$2 and b.idempotency_key=c.id::text and b.status='charged' where c.run_id=r.id),0)::text) from billing.recurring_runs r left join billing.recurring_deliveries d on d.run_id=r.id where r.order_id=$1 order by r.scheduled_at")
  .bind(id).bind(p.owner).fetch_all(&engine.store.pool).await?;
    let dates: Vec<DateTime<Utc>> = row.get("dates");
    let snapshots: Vec<Source> = serde_json::from_value(row.get("sources"))?;
    let date_inputs:Vec<Value>=snapshots.iter().flat_map(|source|dates::candidates(&source.selection["metadata"]["inputs"]).into_iter().map(|input|json!({"selection_id":source.selection["id"],"source_name":source.selection["name"],"pointer":input.pointer,"original":input.original,"date_only":input.date_only,"offset_seconds":source.date_rules.iter().find(|r|r.pointer==input.pointer).map(|r|r.offset_seconds)}))).collect();
    Ok(
        json!({"id":id,"state":row.get::<String,_>("state"),"question":row.get::<String,_>("question"),"configuration":row.get::<Value,_>("configuration"),"delivery":row.get::<Value,_>("delivery"),"dates":dates,"run_count":dates.len(),"provider_per_run":row.get::<i64,_>("provider_per_run").to_string(),"total_per_run":row.get::<i64,_>("total_per_run").to_string(),"total":row.get::<i64,_>("approved_micro_usd").to_string(),"reserved":row.get::<i64,_>("reserved_micro_usd").to_string(),"released":row.get::<i64,_>("released_micro_usd").to_string(),"balance_available":row.get::<Option<String>,_>("available"),"expires_at":row.get::<DateTime<Utc>,_>("expires_at"),"sources":snapshots.iter().map(|source|json!({"name":source.selection["name"],"total_per_run":source.selection["charged_micro_usd"].as_i64().unwrap_or(0).to_string(),"usage_notice":source.service.as_ref().and_then(|service|service.usage_notice_view())})).collect::<Vec<_>>(),"date_inputs":date_inputs,"date_rules":row.get::<Value,_>("sources").as_array().map(|sources| sources.iter().flat_map(|source|source["date_rules"].as_array().into_iter().flatten().cloned()).collect::<Vec<_>>()).unwrap_or_default(),"runs":runs}),
    )
}

pub fn start(engine: Arc<Engine>) {
    if enabled() {
        worker::start(engine);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finite_schedule_and_destination() {
        let s = Schedule {
            start: "2026-10-01".parse().unwrap(),
            end: "2026-10-31".parse().unwrap(),
            time: "09:00:00".parse().unwrap(),
            timezone: "Europe/Amsterdam".into(),
            weekdays: vec![1, 2, 3, 4, 5],
        };
        assert!(s.validate().is_ok());
        let mut bad = s.clone();
        bad.weekdays = vec![0];
        assert!(bad.validate().is_err());
        bad = s;
        bad.end = "2028-01-01".parse().unwrap();
        assert!(bad.validate().is_err());
        assert!(Delivery {
            toolkit: "gmail".into(),
            destination: "a@b.com\nbcc:other".into()
        }
        .validate()
        .is_err());
    }
}

pub async fn read_run(
    engine: &Engine,
    p: &Principal,
    order: Uuid,
    run: Uuid,
) -> anyhow::Result<Value> {
    engine.store.check_workspace(p).await?;
    let result:Value=sqlx::query_scalar("select r.result from billing.recurring_runs r join billing.recurring_orders o on o.id=r.order_id where r.id=$1 and o.id=$2 and o.owner_id=$3 and o.workspace_id is not distinct from $4 and r.result is not null").bind(run).bind(order).bind(p.owner).bind(p.workspace).fetch_one(&engine.store.pool).await?;
    Ok(result)
}
