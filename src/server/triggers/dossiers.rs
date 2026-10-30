//! Slack-to-dossier bridge. No implicit spending, no fabricated screening results.
//! The existing monitoring prepare/authorize/baseline flow owns source execution.
use super::super::{monitoring, ServerState};
use super::{activity, client, slack::esc, tools};
use crate::{auth::Principal, company_checks::Company, engine::Engine, integrations::Composio};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

const FIELDS: &str = "jsonb_build_object('id',d.id,'request',d.request_text,'company',d.company,'sources',d.sources,'establishment_number',d.establishment_number,'task_id',d.task_id,'state',d.state,'snapshot',d.snapshot,'order_id',d.order_id,'workspace_id',d.workspace_id,'channel_id',d.channel_id,'created_at',d.created_at,'approval',(select e.activity from gateway_v2.inbound_trigger_events e where e.owner_id=d.owner_id and e.trigger_id=d.trigger_id and e.activity->>'dossier_id'=d.id::text and e.activity->>'kind' in ('request_approval','approval_decision') order by e.id desc limit 1))";

pub(super) fn screening_request(text: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric()).any(|word| {
        matches!(
            word.to_lowercase().as_str(),
            "screen" | "screening" | "screenen" | "check" | "controleer" | "onboard" | "onboarden"
        )
    })
}
pub(super) fn url(id: Uuid) -> String {
    format!(
        "{}/monitoring?dossier={id}",
        std::env::var("APP_ORIGIN")
            .unwrap_or_else(|_| "https://app.apiosk.com".into())
            .trim_end_matches('/')
    )
}
fn section(text: impl Into<String>) -> Value {
    json!({"type":"section","text":{"type":"mrkdwn","text":text.into()}})
}
fn card(d: &Value) -> (String, Value) {
    let id: Uuid = d["id"].as_str().unwrap_or_default().parse().unwrap();
    let title = d["company"]["name"].as_str().unwrap_or("Company screening");
    let state = d["state"].as_str().unwrap_or("draft");
    let summary=match state {
        "checked"=>"Screening saved. Review the source evidence and coverage in the dossier. Potential sanctions matches require human identity review.",
        "failed"=>"The check did not produce a verified company baseline. Open the dossier to review the source errors. No clearance has been issued.",
        "running"=>"The approved source check is running. Its results will be saved in this dossier.",
        "prepared"=>"The first-check price is ready. Open the dossier to review the selected sources and explicitly approve the cost.",
        _=>"Dossier created. Open it to choose the exact company and review the first-check price before any purchase.",
    };
    let text = format!("{}: {summary}", esc(title));
    let mut blocks = vec![section(format!("*{}*\n{summary}", esc(title)))];
    if let Some(coverage) = d["snapshot"]["coverage"].as_array() {
        let lines: Vec<_> = coverage
            .iter()
            .map(|v| {
                format!(
                    "• {}: {}",
                    esc(v["category"].as_str().unwrap_or("Source")),
                    esc(v["status"].as_str().unwrap_or("unknown"))
                )
            })
            .collect();
        if !lines.is_empty() {
            blocks.push(section(format!("*Source coverage*\n{}", lines.join("\n"))));
        }
    }
    blocks.push(json!({"type":"actions","elements":[{"type":"button","action_id":"apiosk_open_dossier","text":{"type":"plain_text","text":"Open dossier"},"url":url(id)}]}));
    blocks.push(section("In this thread, mention me with `status`, `request approval` or `monitor` to continue with this same dossier."));
    (text, Value::Array(blocks))
}
async fn get(pool: &PgPool, owner: Uuid, id: Uuid) -> anyhow::Result<Value> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT {FIELDS} FROM gateway_v2.slack_dossiers d WHERE d.id=$1 AND d.owner_id=$2"
    ))
    .bind(id)
    .bind(owner)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("Dossier unavailable."))?)
}
async fn scoped(s: &ServerState, h: &HeaderMap, id: Uuid) -> Result<(Principal, Value), Response> {
    let p = monitoring::person(s, h).await?;
    s.engine
        .store
        .check_workspace(&p)
        .await
        .map_err(|e| monitoring::reply(Err(e)))?;
    let d = get(&s.engine.store.pool, p.owner, id)
        .await
        .map_err(|_| super::error(StatusCode::NOT_FOUND, "Dossier unavailable."))?;
    // Drafts are unassigned until the owner chooses a workspace and prepares a check.
    if !d["company"].is_null() && d["workspace_id"] != json!(p.workspace) {
        return Err(super::error(
            StatusCode::CONFLICT,
            "Open this dossier in the workspace where its first check was prepared.",
        ));
    }
    Ok((p, d))
}
pub(in crate::server) async fn list(State(s): State<ServerState>, h: HeaderMap) -> Response {
    let p = match monitoring::person(&s, &h).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    monitoring::reply(async{
        s.engine.store.check_workspace(&p).await?;
        let rows:Vec<Value>=sqlx::query_scalar(&format!("SELECT {FIELDS} FROM gateway_v2.slack_dossiers d WHERE d.owner_id=$1 AND (d.company IS NULL OR d.workspace_id IS NOT DISTINCT FROM $2) ORDER BY d.created_at DESC LIMIT 100"))
            .bind(p.owner).bind(p.workspace).fetch_all(&s.engine.store.pool).await?;
        Ok(json!({"dossiers":rows}))
    }.await)
}
pub(in crate::server) async fn read(
    State(s): State<ServerState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    match scoped(&s, &h, id).await {
        Ok((_, d)) => Json(d).into_response(),
        Err(r) => r,
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::server) struct Prepare {
    company: Company,
    sources: Vec<String>,
    establishment_number: Option<String>,
}
pub(in crate::server) async fn prepare(
    State(s): State<ServerState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<Prepare>,
) -> Response {
    let (p, _) = match scoped(&s, &h, id).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    monitoring::reply(async{
        crate::company_checks::validate_companies(std::slice::from_ref(&b.company))?;
        let sources=crate::monitoring::sources(b.sources)?;
        anyhow::ensure!(b.establishment_number.as_ref().is_none_or(|n|n.len()==12&&n.bytes().all(|b|b.is_ascii_digit())),"Choose an exact twelve-digit establishment number.");
        s.engine.store.check_spending(&p).await?;
        // Claim the immutable company/workspace before planning. Every retry reuses id.
        let saved:Option<Value>=sqlx::query_scalar("UPDATE gateway_v2.slack_dossiers SET company=coalesce(company,$3),workspace_id=CASE WHEN company IS NULL THEN $4 ELSE workspace_id END,sources=CASE WHEN company IS NULL THEN $5 ELSE sources END,establishment_number=CASE WHEN company IS NULL THEN $6 ELSE establishment_number END,updated_at=now() WHERE id=$1 AND owner_id=$2 AND (company IS NULL OR (company=$3 AND workspace_id IS NOT DISTINCT FROM $4 AND sources=$5 AND establishment_number IS NOT DISTINCT FROM $6)) RETURNING company")
            .bind(id).bind(p.owner).bind(json!(b.company)).bind(p.workspace).bind(json!(sources)).bind(&b.establishment_number).fetch_optional(&s.engine.store.pool).await?;
        anyhow::ensure!(saved.is_some(),"This dossier already has a different company or source selection. Recover its saved check.");
        let response=monitoring::prepare_check(&s.engine,&p,id,&b.company,&sources,b.establishment_number.as_deref()).await?;
        if let Some(state)=&response.state {
            sqlx::query("UPDATE gateway_v2.slack_dossiers SET task_id=$3,state=CASE WHEN state='draft' THEN 'prepared' ELSE state END,next_refresh_at=now(),updated_at=now() WHERE id=$1 AND owner_id=$2 AND (task_id IS NULL OR task_id=$3)")
                .bind(id).bind(p.owner).bind(state.state_ref).execute(&s.engine.store.pool).await?;
        }
        Ok(serde_json::to_value(response)?)
    }.await)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::server) struct Link {
    order_id: Uuid,
}
pub(in crate::server) async fn link(
    State(s): State<ServerState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<Link>,
) -> Response {
    let (p, d) = match scoped(&s, &h, id).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    monitoring::reply(async{
        let order=crate::recurring::read(&s.engine,&p,b.order_id).await?;
        anyhow::ensure!(order["workflow"]["processing"]==crate::monitoring::VERSION && order["configuration"]["monitoring"]["task_id"]==d["task_id"] && !d["task_id"].is_null(),"Use this dossier's saved first check.");
        sqlx::query("UPDATE gateway_v2.slack_dossiers SET order_id=$3,updated_at=now() WHERE id=$1 AND owner_id=$2").bind(id).bind(p.owner).bind(b.order_id).execute(&s.engine.store.pool).await?;
        get(&s.engine.store.pool,p.owner,id).await
    }.await)
}

/// Called only for authenticated, deduplicated, non-bot events mentioning Apiosk.
/// Thread identity, not a model-supplied dossier ID, selects the dossier.
pub(super) async fn handle(
    engine: &Engine,
    c: &Composio,
    owner: Uuid,
    trigger: Uuid,
    team: &str,
    bot: &str,
    channel: &str,
    thread: &str,
    text: &str,
    may_create: bool,
) -> anyhow::Result<bool> {
    let pool = &engine.store.pool;
    let existing:Option<Uuid>=sqlx::query_scalar("SELECT id FROM gateway_v2.slack_dossiers WHERE owner_id=$1 AND trigger_id=$2 AND team_id=$3 AND channel_id=$4 AND thread_ts=$5")
        .bind(owner).bind(trigger).bind(team).bind(channel).bind(thread).fetch_optional(pool).await?;
    if existing.is_none() && (!may_create || !screening_request(text)) {
        return Ok(false);
    }
    let id = if let Some(id) = existing {
        id
    } else {
        let id = Uuid::new_v4();
        sqlx::query_scalar("INSERT INTO gateway_v2.slack_dossiers(id,owner_id,trigger_id,team_id,channel_id,thread_ts,request_text) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(trigger_id,thread_ts) DO UPDATE SET thread_ts=excluded.thread_ts RETURNING id")
            .bind(id).bind(owner).bind(trigger).bind(team).bind(channel).bind(thread).bind(text).fetch_one(pool).await?
    };
    let d = get(pool, owner, id).await?;
    if !d["company"].is_null() {
        engine.store.check_workspace(&Principal { owner, connection:None, workspace:d["workspace_id"].as_str().and_then(|v|v.parse().ok()) }).await?;
    }
    let lower = text.to_lowercase();
    if existing.is_some()
        && (lower.contains("request approval")
            || lower.contains("goedkeuring")
            || lower.contains("approve"))
    {
        if d["state"] == "checked" {
            let approver:Option<(String,String)>=sqlx::query_as("SELECT approver_id,approver_name FROM gateway_v2.inbound_triggers WHERE id=$1 AND owner_id=$2 AND approver_id IS NOT NULL")
                .bind(trigger).bind(owner).fetch_optional(pool).await?;
            if let Some((user, name)) = approver {
                tools::request_dossier_review(
                    pool, c, owner, bot, trigger, channel, thread, &d, &user, &name,
                )
                .await?;
                return Ok(true);
            }
        }
        let text = if d["state"] == "checked" {
            "Choose an approver in this trigger's settings first. The saved dossier is unchanged."
        } else {
            "Complete the source check and review its evidence before requesting dossier approval."
        };
        c.apiosk_bot_post(owner, bot, channel, Some(thread), text, None)
            .await?;
        return Ok(true);
    }
    let (mut line, mut blocks) = card(&d);
    if existing.is_some() && (lower.contains("monitor") || lower.contains("volg")) {
        line="Open this dossier and choose Set up monitoring to review the schedule, delivery and price. Its saved first check will be reused.".into();
        blocks.as_array_mut().unwrap().insert(0, section(&line));
    }
    let ts = c
        .apiosk_bot_post(owner, bot, channel, Some(thread), &line, Some(&blocks))
        .await?;
    sqlx::query("UPDATE gateway_v2.slack_dossiers SET message_ts=coalesce(message_ts,$2),published_state=CASE WHEN message_ts IS NULL THEN state ELSE published_state END WHERE id=$1").bind(id).bind(&ts).execute(pool).await?;
    activity::record(pool,trigger,owner,&format!("response:{channel}:{ts}"),&line,false,&json!({"kind":"screen_company","status":d["state"],"dossier_id":id,"dossier_url":url(id),"channel_id":channel,"message_ts":ts,"preview":activity::preview(&blocks)})).await?;
    Ok(true)
}

async fn refresh(engine: &Engine, owner: Uuid, id: Uuid) -> anyhow::Result<()> {
    let pool = &engine.store.pool;
    let mut d = get(pool, owner, id).await?;
    if matches!(d["state"].as_str(), Some("prepared" | "running")) {
        if let Some(task) = d["task_id"].as_str().and_then(|v| v.parse().ok()) {
            let p = Principal {
                owner,
                connection: None,
                workspace: d["workspace_id"].as_str().and_then(|v| v.parse().ok()),
            };
            engine.store.check_workspace(&p).await?;
            let view = engine.view(&p, task, Uuid::new_v4(), None).await?;
            let company: Company = serde_json::from_value(d["company"].clone())?;
            let sources: Vec<String> = serde_json::from_value(d["sources"].clone())?;
            let (state, snapshot) = if crate::company_checks::can_snapshot(&view) {
                if matches!(
                    view.status,
                    crate::protocol::Status::Succeeded | crate::protocol::Status::Partial
                ) {
                    match crate::monitoring::baseline(engine, &p, task, &company, &sources).await {
                        Ok(snapshot) => ("checked", Some(snapshot)),
                        Err(error) if error.downcast_ref::<sqlx::Error>().is_some() => {
                            return Err(error)
                        }
                        Err(_) => ("failed", None),
                    }
                } else {
                    ("failed", None)
                }
            } else if view
                .billing
                .as_ref()
                .is_some_and(|v| v["authorization_active"] == true)
                || view.status == crate::protocol::Status::Running
            {
                ("running", None)
            } else {
                ("prepared", None)
            };
            sqlx::query("UPDATE gateway_v2.slack_dossiers SET state=$3,snapshot=coalesce(snapshot,$4),updated_at=now() WHERE id=$1 AND owner_id=$2").bind(id).bind(owner).bind(state).bind(snapshot).execute(pool).await?;
            d = get(pool, owner, id).await?;
        }
    }
    let row=sqlx::query("SELECT trigger_id,team_id,channel_id,message_ts FROM gateway_v2.slack_dossiers WHERE id=$1 AND state IS DISTINCT FROM published_state AND message_ts IS NOT NULL")
        .bind(id).fetch_optional(pool).await?;
    if let Some(row) = row {
        let c = client().map_err(|_| anyhow::anyhow!("Slack unavailable"))?;
        let team: String = row.get("team_id");
        let bot = c
            .apiosk_bot(owner, Some(&team))
            .await?
            .ok_or_else(|| anyhow::anyhow!("Slack bot unavailable"))?;
        anyhow::ensure!(bot.team == team, "Slack workspace changed");
        let channel: String = row.get("channel_id");
        let ts: String = row.get("message_ts");
        let (line, blocks) = card(&d);
        c.apiosk_bot_update(owner, &bot.account, &channel, &ts, &line, &blocks)
            .await?;
        let state = d["state"].as_str().unwrap();
        activity::record(pool,row.get("trigger_id"),owner,&format!("dossier:{id}:{state}"),&line,false,&json!({"kind":"screen_company","status":state,"dossier_id":id,"dossier_url":url(id),"channel_id":channel,"message_ts":ts,"preview":activity::preview(&blocks)})).await?;
        sqlx::query("UPDATE gateway_v2.slack_dossiers SET published_state=$2 WHERE id=$1")
            .bind(id)
            .bind(state)
            .execute(pool)
            .await?;
    }
    Ok(())
}
/// Durable polling resumes after deployments. Only reads already-authorized tasks;
/// it never authorizes purchases or starts an unapproved source execution.
pub fn start(engine: Arc<Engine>) {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(15));
        loop {
            timer.tick().await;
            let work:Result<Vec<(Uuid,Uuid)>,_>=sqlx::query_as("UPDATE gateway_v2.slack_dossiers SET next_refresh_at=now()+interval '45 seconds' WHERE id IN (SELECT id FROM gateway_v2.slack_dossiers WHERE next_refresh_at<=now() AND (state IN ('prepared','running') OR (message_ts IS NOT NULL AND state IS DISTINCT FROM published_state)) ORDER BY next_refresh_at LIMIT 10 FOR UPDATE SKIP LOCKED) RETURNING id,owner_id").fetch_all(&engine.store.pool).await;
            match work {
                Ok(rows) => {
                    for (id, owner) in rows {
                        if let Err(e) = refresh(&engine, owner, id).await {
                            tracing::warn!(error=%e,"Slack dossier refresh failed");
                        }
                    }
                }
                Err(e) => tracing::warn!(error=%e,"Slack dossier queue unavailable"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_dossier_card_preserves_coverage_without_claiming_clearance() {
        let d = json!({"id":Uuid::new_v4(),"state":"checked","company":{"name":"<@U123>","kvk_number":"12345678"},"snapshot":{"coverage":[{"category":"sanctions","status":"unavailable"}]}});
        let (text, blocks) = card(&d);
        assert!(text.contains("&lt;@U123&gt;"));
        assert!(blocks.to_string().contains("sanctions: unavailable"));
        assert!(text.contains("human identity review"));
        assert!(!blocks.to_string().contains("No issues identified"));
        assert!(!blocks.to_string().contains("News from the last 7 days"));
    }
    #[test]
    fn screening_intent_is_separate_from_conversation() {
        for text in [
            "screen Acme",
            "screening KVK 12345678",
            "Controleer dit bedrijf",
            "Kunnen we Testbedrijf B.V. onboarden?",
        ] {
            assert!(screening_request(text));
        }
        for text in ["how are you?", "Thanks", "status", "request approval"] {
            assert!(!screening_request(text));
        }
    }
}
