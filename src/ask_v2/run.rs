//! Durable Ask orchestration. Provider execution and approval remain in Engine.
use super::{evidence, planner, run_contract::*, synthesis};
use crate::{auth::Principal, engine::Engine, state::Task};
use anyhow::{ensure, Result};
use chrono::Utc;
use serde_json::{json, Value};
use sqlx::Row;
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

#[derive(Clone)]
pub struct Run {
    pub engine: Arc<Engine>,
    pub person: Principal,
    pub id: Uuid,
}
impl Run {
    pub async fn start(
        engine: Arc<Engine>,
        person: Principal,
        request: StartRunRequest,
    ) -> Result<Self> {
        ensure!(
            !request.request_id.is_nil()
                && !request.question.trim().is_empty()
                && request.question.encode_utf16().count() <= 16000,
            "invalid_question"
        );
        engine.store.check_workspace(&person).await?;
        let mut tx = engine.store.pool.begin().await?;
        // Serialize the idempotency key before creating its backing task.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("ask:{}:{}", person.owner, request.request_id))
            .execute(&mut *tx)
            .await?;
        if let Some(row) = sqlx::query(
            "SELECT id,question,workspace_id FROM ask_runs WHERE owner_id=$1 AND request_id=$2",
        )
        .bind(person.owner)
        .bind(request.request_id)
        .fetch_optional(&mut *tx)
        .await?
        {
            ensure!(
                row.get::<String, _>("question") == request.question.trim()
                    && row.get::<Option<Uuid>, _>("workspace_id") == person.workspace,
                "idempotency_conflict"
            );
            let id = row.get("id");
            tx.commit().await?;
            return Ok(Self { engine, person, id });
        }
        let id = Uuid::new_v4();
        let task = Task {
            workspace_id: person.workspace,
            ..Task::default()
        };
        sqlx::query("INSERT INTO tasks(id,owner_id,connection_id,document) VALUES($1,$2,$3,$4)")
            .bind(id)
            .bind(person.owner)
            .bind(person.connection)
            .bind(serde_json::to_value(task)?)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO ask_runs(id,owner_id,workspace_id,request_id,question,question_sha256,task_id,stage) VALUES($1,$2,$3,$4,$5,$6,$1,'understanding')").bind(id).bind(person.owner).bind(person.workspace).bind(request.request_id).bind(request.question.trim()).bind(crate::state::digest(&request.question.trim())).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Self { engine, person, id })
    }
    async fn row(&self) -> Result<sqlx::postgres::PgRow> {
        self.engine.store.check_workspace(&self.person).await?;
        sqlx::query("SELECT * FROM ask_runs WHERE id=$1 AND owner_id=$2 AND workspace_id IS NOT DISTINCT FROM $3 AND expires_at>now()").bind(self.id).bind(self.person.owner).bind(self.person.workspace).fetch_optional(&self.engine.store.pool).await?.ok_or_else(||anyhow::anyhow!("run_unavailable"))
    }
    pub async fn event(&self, stage: Stage, status: EventStatus, counts: Counts) -> Result<()> {
        self.append_event(stage, status, counts, None).await
    }
    async fn append_event(
        &self,
        stage: Stage,
        status: EventStatus,
        counts: Counts,
        source: Option<(&str, &str, SourceRowStatus)>,
    ) -> Result<()> {
        let mut tx = self.engine.store.pool.begin().await?;
        let row=sqlx::query("SELECT last_sequence,question_revision,plan_revision,created_at,stage FROM ask_runs WHERE id=$1 AND owner_id=$2 AND workspace_id IS NOT DISTINCT FROM $3 FOR UPDATE").bind(self.id).bind(self.person.owner).bind(self.person.workspace).fetch_one(&mut *tx).await?;
        let previous: Stage = serde_json::from_value(json!(row.get::<String, _>("stage")))?;
        if previous == Stage::Cancelled {
            anyhow::bail!("run_cancelled")
        }
        if previous.terminal() {
            return Ok(());
        }
        let dedupe = source.map(|(_, step, status)| format!("source:{step}:{status:?}"));
        if let Some(key) = &dedupe {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM ask_run_events WHERE run_id=$1 AND dedupe_key=$2)",
            )
            .bind(self.id)
            .bind(key)
            .fetch_one(&mut *tx)
            .await?;
            if exists {
                return Ok(());
            }
        }
        let sequence = row.get::<i64, _>("last_sequence") + 1;
        let now = Utc::now();
        let event = ProgressEvent {
            event_schema_version: 1,
            event_id: event_id(self.id, sequence as u64),
            run_id: self.id,
            question_revision: row.get::<i32, _>("question_revision") as u32,
            plan_revision: row.get::<Option<i32>, _>("plan_revision").map(|n| n as u32),
            sequence: sequence as u64,
            timestamp: now,
            stage,
            status,
            message_key: stage.message_key(),
            message_args: Default::default(),
            counts,
            source_id: source.map(|(id, _, _)| id.into()),
            step_id: source.map(|(_, step, _)| step.into()),
            source_status: source.map(|(_, _, status)| status),
            elapsed_ms: (now - row.get::<chrono::DateTime<Utc>, _>("created_at"))
                .num_milliseconds()
                .max(0) as u64,
            error: None,
        };
        sqlx::query("INSERT INTO ask_run_events(run_id,sequence,owner_id,question_revision,plan_revision,stage,event,dedupe_key) VALUES($1,$2,$3,$4,$5,$6,$7,$8)").bind(self.id).bind(sequence).bind(self.person.owner).bind(event.question_revision as i32).bind(event.plan_revision.map(|n|n as i32)).bind(stage.as_str()).bind(serde_json::to_value(&event)?).bind(dedupe).execute(&mut *tx).await?;
        sqlx::query("UPDATE ask_runs SET stage=$2,last_sequence=$3,updated_at=now(),stage_started_at=CASE WHEN stage=$2 THEN stage_started_at ELSE now() END WHERE id=$1").bind(self.id).bind(stage.as_str()).bind(sequence).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn resume(&self, bearer: String) -> Result<()> {
        let row = self.row().await?;
        let stage: Stage = serde_json::from_value(json!(row.get::<String, _>("stage")))?;
        if stage.terminal() || stage == Stage::NeedsClarification {
            return Ok(());
        }
        if stage == Stage::AwaitingApproval {
            let plan = self.saved_plan().await?;
            let authorized:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM authorizations WHERE task_id=$1 AND owner_id=$2 AND quote_ref=$3 AND plan_hash=$4 AND revoked_at IS NULL AND expires_at>now())").bind(self.id).bind(self.person.owner).bind(plan.quote.quote_ref).bind(&plan.quote.plan_hash).fetch_one(&self.engine.store.pool).await?;
            if !authorized {
                return Ok(());
            }
            self.event(Stage::Executing, EventStatus::Running, Counts::default())
                .await?;
        }
        let lease = Uuid::new_v4();
        let acquired=sqlx::query("UPDATE ask_runs SET lease_id=$2,lease_expires_at=now()+interval '90 seconds' WHERE id=$1 AND (lease_expires_at IS NULL OR lease_expires_at<now())").bind(self.id).bind(lease).execute(&self.engine.store.pool).await?.rows_affected()==1;
        if !acquired {
            return Ok(());
        }
        let run = self.clone();
        crate::telemetry::spawn(async move {
            if let Err(error) = run.drive(&bearer, lease).await {
                tracing::warn!(run=%run.id,error=%error,"Ask run stopped");
                let _ = sqlx::query("UPDATE ask_runs SET error=$2 WHERE id=$1")
                    .bind(run.id)
                    .bind(json!({"code":"run_failed","message_key":"run.error.failed"}))
                    .execute(&run.engine.store.pool)
                    .await;
                let _ = run
                    .event(Stage::Failed, EventStatus::Failed, Counts::default())
                    .await;
            }
            let _=sqlx::query("UPDATE ask_runs SET lease_id=NULL,lease_expires_at=NULL WHERE id=$1 AND lease_id=$2").bind(run.id).bind(lease).execute(&run.engine.store.pool).await;
        });
        Ok(())
    }
    async fn drive(&self, bearer: &str, lease: Uuid) -> Result<()> {
        let row = self.row().await?;
        if row.get::<Option<Value>, _>("planner_input").is_none() {
            self.event(
                Stage::Understanding,
                EventStatus::Running,
                Counts::default(),
            )
            .await?;
            let catalog = self.engine.catalog.get().await?;
            let definitions = super::intent_catalog::load(&self.engine.store.pool).await?;
            let client = self
                .engine
                .parser
                .ask_v2()
                .ok_or_else(|| anyhow::anyhow!("intent_not_configured"))?;
            ensure!(client.admit(&self.person.owner.to_string()), "rate_limited");
            // Record the attempt before dispatch. Recovery never blindly repeats a paid model call.
            let prior = row.get::<Value, _>("model_calls");
            ensure!(
                prior["extraction"].as_u64().unwrap_or(0) == 0,
                "interrupted_extraction"
            );
            sqlx::query("UPDATE ask_runs SET model_calls=jsonb_set(model_calls,'{extraction}','1') WHERE id=$1").bind(self.id).execute(&self.engine.store.pool).await?;
            let (mut output, drafts) = client
                .extract_intent(&row.get::<String, _>("question"), &definitions)
                .await?;
            self.event(
                Stage::ValidatingIntent,
                EventStatus::Running,
                Counts::default(),
            )
            .await?;
            super::intent_fields::ground_response(&mut output, &catalog)?;
            super::intent_catalog::save_drafts(&drafts).await?;
            let envelope: GroundedIntentEnvelope = serde_json::from_value(output.clone())?;
            let input = planner::adapt(&envelope)?;
            sqlx::query("UPDATE ask_runs SET intent=$2,planner_input=$3,model_calls=$4,model_usage=$5 WHERE id=$1").bind(self.id).bind(output.clone()).bind(serde_json::to_value(input)?).bind(json!({"extraction":output["attempts"].as_u64().unwrap_or(1),"synthesis":0,"repair":0})).bind(output.get("usage").cloned()).execute(&self.engine.store.pool).await?;
        }
        let row = self.row().await?;
        if row.get::<String, _>("stage") == "cancelled" {
            return Ok(());
        }
        let mut input: PlannerInput = serde_json::from_value(row.get::<Value, _>("planner_input"))?;
        planner::recover_explicit_inputs(&mut input, &row.get::<String, _>("question"));
        if row.get::<Option<i32>, _>("plan_revision").is_none() {
            self.event(
                Stage::FindingSources,
                EventStatus::Running,
                Counts::default(),
            )
            .await?;
            let catalog = self.engine.catalog.get().await?;
            self.event(
                Stage::FilteringSources,
                EventStatus::Running,
                Counts {
                    candidates_total: Some(catalog.services.len() as u32),
                    ..Counts::default()
                },
            )
            .await?;
            self.event(
                Stage::RankingSources,
                EventStatus::Running,
                Counts::default(),
            )
            .await?;
            let context = planner::PlanContext {
                run_id: self.id,
                plan_revision: sqlx::query_scalar::<_, i32>(
                    "SELECT coalesce(max(plan_revision),0)+1 FROM ask_run_plans WHERE run_id=$1",
                )
                .bind(self.id)
                .fetch_one(&self.engine.store.pool)
                .await? as u32,
                policy: planner::active_policy(),
                shadow_policies: vec![],
                signals: planner::SignalSnapshot::default(),
                now: Utc::now(),
            };
            let pass:i32=sqlx::query_scalar("UPDATE ask_runs SET planning_passes=planning_passes+1 WHERE id=$1 RETURNING planning_passes").bind(self.id).fetch_one(&self.engine.store.pool).await?;
            let outcome = planner::plan(&input, &catalog, &context);
            let records = match &outcome {
                PlanOutcome::Ready { scores, .. }
                | PlanOutcome::NoQualifyingSources { scores, .. }
                | PlanOutcome::NeedsClarification { scores, .. } => scores,
            };
            for score in records {
                let record = serde_json::to_value(score)?;
                sqlx::query("INSERT INTO ask_run_scores(run_id,owner_id,planning_pass,question_revision,candidate_id,request_id,contract_id,endpoint_id,source_id,fit_score,score_version,calibration_status,mode,threshold,decision,record) VALUES($1,$2,$15,$16,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) ON CONFLICT DO NOTHING")
                    .bind(self.id).bind(self.person.owner).bind(&score.candidate_id).bind(&score.request_id).bind(score.contract_id).bind(score.endpoint_id).bind(&score.source_id).bind(score.fit_score.map(|s|s.get())).bind(&score.score_version).bind(record["calibration_status"].as_str().unwrap()).bind(record["mode"].as_str().unwrap()).bind(score.threshold.get()).bind(record["decision"].as_str().unwrap()).bind(record.clone()).bind(pass).bind(input.question_revision as i32).execute(&self.engine.store.pool).await?;
            }
            self.event(Stage::BuildingPlan, EventStatus::Running, Counts::default())
                .await?;
            match outcome {
                PlanOutcome::Ready { plan, seed, .. } => {
                    check_quote_totals(&plan)?;
                    let mut pinned = input.clone();
                    for step in &seed.plan.steps {
                        for fact in step
                            .service
                            .contract
                            .output_map
                            .produces
                            .keys()
                            .chain(step.service.contract.requires.iter().map(|r| &r.fact_type))
                        {
                            if let Some(def) = catalog.vocabulary.get(fact) {
                                pinned
                                    .fact_schemas
                                    .insert(fact.clone(), def.value_schema.clone());
                            }
                        }
                    }
                    let mut tx = self.engine.store.pool.begin().await?;
                    let stage: String =
                        sqlx::query_scalar("SELECT stage FROM ask_runs WHERE id=$1 FOR UPDATE")
                            .bind(self.id)
                            .fetch_one(&mut *tx)
                            .await?;
                    if stage == "cancelled" {
                        return Ok(());
                    }
                    let mut task = Task {
                        workspace_id: self.person.workspace,
                        entities: seed.entities,
                        facts: seed.facts,
                        proposal: Some(serde_json::to_value(&seed.plan)?),
                        ..Task::default()
                    };
                    task.actions = vec![json!({"kind":"run","action_id":Uuid::new_v4(),"step":0})];
                    sqlx::query("UPDATE tasks SET document=$2,revision=revision+1 WHERE id=$1 AND owner_id=$3").bind(self.id).bind(serde_json::to_value(task)?).bind(self.person.owner).execute(&mut *tx).await?;
                    sqlx::query("INSERT INTO ask_run_plans(run_id,plan_revision,question_revision,plan_id,owner_id,task_id,quote_ref,plan_hash,cap_atomic,currency,status,score_version,threshold,catalog_version,expires_at,source_plan,planner_input) VALUES($1,$14,$15,$2,$3,$1,$4,$5,$6,'USD',$7,$8,$9,$10,$11,$12,$13)").bind(self.id).bind(plan.plan_id).bind(self.person.owner).bind(plan.quote.quote_ref).bind(&plan.quote.plan_hash).bind(plan.quote.total_atomic.0).bind(if plan.status==PlanStatus::Complete{"complete"}else{"partial"}).bind(&plan.score_policy.score_version).bind(plan.score_policy.threshold.get()).bind(&plan.catalog_version).bind(plan.quote.expires_at).bind(serde_json::to_value(&*plan)?).bind(serde_json::to_value(&pinned)?).bind(plan.plan_revision as i32).bind(input.question_revision as i32).execute(&mut *tx).await?;
                    sqlx::query(
                        "UPDATE ask_runs SET plan_revision=$2,planner_input=$3 WHERE id=$1",
                    )
                    .bind(self.id)
                    .bind(plan.plan_revision as i32)
                    .bind(serde_json::to_value(&pinned)?)
                    .execute(&mut *tx)
                    .await?;
                    tx.commit().await?;
                    self.event(
                        Stage::AwaitingApproval,
                        EventStatus::Waiting,
                        plan.candidates.clone(),
                    )
                    .await?;
                    return Ok(());
                }
                PlanOutcome::NoQualifyingSources { gaps, .. } => {
                    sqlx::query("UPDATE ask_runs SET gaps=$2 WHERE id=$1")
                        .bind(self.id)
                        .bind(serde_json::to_value(gaps)?)
                        .execute(&self.engine.store.pool)
                        .await?;
                    self.event(
                        Stage::NoQualifyingSources,
                        EventStatus::Completed,
                        Counts::default(),
                    )
                    .await?;
                    return Ok(());
                }
                PlanOutcome::NeedsClarification { clarification, .. } => {
                    sqlx::query("UPDATE ask_runs SET clarification=$2 WHERE id=$1")
                        .bind(self.id)
                        .bind(serde_json::to_value(clarification)?)
                        .execute(&self.engine.store.pool)
                        .await?;
                    self.event(
                        Stage::NeedsClarification,
                        EventStatus::Waiting,
                        Counts::default(),
                    )
                    .await?;
                    return Ok(());
                }
            }
        }
        let stage = self.row().await?.get::<String, _>("stage");
        if stage == "awaiting_approval" || stage == "cancelled" {
            return Ok(());
        }
        let plan = self.saved_plan().await?;
        let authorized:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM authorizations WHERE task_id=$1 AND owner_id=$2 AND quote_ref=$3 AND plan_hash=$4 AND revoked_at IS NULL)").bind(self.id).bind(self.person.owner).bind(plan.quote.quote_ref).bind(&plan.quote.plan_hash).fetch_one(&self.engine.store.pool).await?;
        if !authorized {
            self.event(
                Stage::AwaitingApproval,
                EventStatus::Waiting,
                plan.candidates.clone(),
            )
            .await?;
            return Ok(());
        }
        self.engine
            .start_worker(self.person.clone(), bearer.into(), self.id)
            .await?;
        loop {
            let (_, task, _) = self.engine.store.load_scoped(&self.person, self.id).await?;
            if task.cancelled {
                return Ok(());
            }
            let active = self.engine.worker_active(self.id).await?;
            for source in &plan.sources {
                let status = if task.failed_steps.contains_key(&source.step_id) {
                    SourceRowStatus::Failed
                } else if task.completed.contains(&source.step_id) {
                    SourceRowStatus::Validating
                } else if task
                    .pending_dispatches
                    .iter()
                    .any(|p| p.step == source.step_index as usize)
                {
                    SourceRowStatus::Running
                } else if source.depends_on.is_empty() {
                    SourceRowStatus::Queued
                } else {
                    SourceRowStatus::WaitingForPrerequisite
                };
                self.append_event(
                    Stage::Executing,
                    EventStatus::Running,
                    Counts {
                        calls_total: Some(plan.sources.len() as u32),
                        calls_finished: Some(
                            task.completed.len() as u32 + task.failed_steps.len() as u32,
                        ),
                        ..Counts::default()
                    },
                    Some((&source.source_id, &source.step_id, status)),
                )
                .await?;
            }
            if !active {
                break;
            }
            sqlx::query("UPDATE ask_runs SET lease_expires_at=now()+interval '90 seconds' WHERE id=$1 AND lease_id=$2").bind(self.id).bind(lease).execute(&self.engine.store.pool).await?;
            tokio::time::sleep(Duration::from_millis(750)).await;
        }
        self.finish(&input).await
    }
    async fn saved_plan(&self) -> Result<SourcePlan> {
        Ok(serde_json::from_value(sqlx::query_scalar::<_,Value>("SELECT source_plan FROM ask_run_plans WHERE run_id=$1 AND superseded_at IS NULL ORDER BY plan_revision DESC LIMIT 1").bind(self.id).fetch_one(&self.engine.store.pool).await?)?)
    }
    async fn finish(&self, input: &PlannerInput) -> Result<()> {
        if let Some(answer) = sqlx::query_scalar::<_, Value>(
            "SELECT answer FROM ask_run_answers WHERE run_id=$1 ORDER BY plan_revision DESC LIMIT 1",
        ).bind(self.id).fetch_optional(&self.engine.store.pool).await? {
            let stage = if answer["completion_stage"] == "completed" { Stage::Completed } else { Stage::Partial };
            self.event(stage, EventStatus::Completed, Counts::default()).await?;
            return Ok(());
        }
        self.event(
            Stage::ValidatingResults,
            EventStatus::Running,
            Counts::default(),
        )
        .await?;
        let (_, task, _) = self.engine.store.load_scoped(&self.person, self.id).await?;
        let public = self.saved_plan().await?;
        let plan: crate::resolver::Plan = serde_json::from_value(task.proposal.clone().unwrap())?;
        let mut catalog = (*self.engine.catalog.get().await?).clone();
        ensure!(
            !input.fact_schemas.is_empty(),
            "evidence_schema_unavailable"
        );
        catalog.vocabulary = input
            .fact_schemas
            .iter()
            .map(|(slug, schema)| {
                (
                    slug.clone(),
                    catalog_core::facts::contract::FactType {
                        slug: slug.clone(),
                        value_schema: schema.clone(),
                    },
                )
            })
            .collect();
        if task.cancelled {
            return Ok(());
        }
        let rows=sqlx::query("SELECT id,execution_id,document,created_at FROM results WHERE task_id=$1 AND owner_id=$2").bind(self.id).bind(self.person.owner).fetch_all(&self.engine.store.pool).await?;
        let mut results = vec![];
        for row in rows {
            let mut document: Value = row.get("document");
            let step = plan.steps.iter().find(|s| {
                document["source"]["provider"] == s.service.api_slug
                    && document["source"]["path"] == s.service.path
                    && document["subject"]["entity_ref"] == s.entity_ref.to_string()
            });
            if let Some(step) = step {
                evidence::normalize(&mut document, step, &catalog);
                if let Some(normalized) = document.get("_ask_normalized") {
                    sqlx::query("UPDATE results SET document=jsonb_set(document,'{_ask_normalized}',$2) WHERE id=$1 AND owner_id=$3 AND task_id=$4").bind(row.get::<Uuid,_>("id")).bind(normalized).bind(self.person.owner).bind(self.id).execute(&self.engine.store.pool).await?;
                }
                results.push(evidence::SavedResult {
                    result_ref: row.get("id"),
                    execution_id: row.get("execution_id"),
                    step_id: step.key(),
                    document,
                    retrieved_at: row.get("created_at"),
                });
            }
        }
        let set = evidence::validate(&evidence::EvidenceInput {
            run_id: self.id,
            plan: &public,
            input,
            steps: &plan.steps,
            catalog: &catalog,
            results: &results,
            failed_steps: &task.failed_steps,
            now: Utc::now(),
        });
        for source in &public.sources {
            if let Some(status) = set.per_source.get(&source.step_id) {
                self.append_event(
                    Stage::ValidatingResults,
                    EventStatus::Running,
                    Counts::default(),
                    Some((&source.source_id, &source.step_id, status.status)),
                )
                .await?;
            }
        }
        let row = self.row().await?;
        let question: String = row.get("question");
        let attempts = row.get::<Value, _>("model_calls")["extraction"]
            .as_u64()
            .unwrap_or(1) as u32;
        let sinput = synthesis::SynthesisInput {
            run_id: self.id,
            plan_revision: public.plan_revision,
            question: &question,
            input,
            plan: &public,
            evidence: &set,
            extraction_calls: attempts,
        };
        // Claim synthesis once before sending. After an uncertain outcome, recovery
        // renders saved evidence instead of spending on another model call.
        let claimed=sqlx::query("UPDATE ask_runs SET model_calls=jsonb_set(model_calls,'{synthesis}','1') WHERE id=$1 AND coalesce((model_calls->>'synthesis')::int,0)=0 AND stage<>'cancelled'").bind(self.id).execute(&self.engine.store.pool).await?.rows_affected()==1;
        let output = synthesis::answer(
            if claimed {
                self.engine.parser.ask_v2()
            } else {
                None
            },
            &sinput,
            self,
        )
        .await?;
        let completion_stage = if output.gaps.is_empty()
            && !output.blocks.is_empty()
            && task.failed_steps.is_empty()
            && task.pending_candidates.is_empty()
            && !results.iter().any(|r| r.document["coverage"] == "partial")
        {
            Stage::Completed
        } else {
            Stage::Partial
        };
        let mut saved_answer = serde_json::to_value(&output)?;
        saved_answer["completion_stage"] = json!(completion_stage);
        let mut tx = self.engine.store.pool.begin().await?;
        for e in &set.records {
            sqlx::query("INSERT INTO ask_run_evidence(run_id,evidence_id,owner_id,plan_revision,request_id,subject_id,field_id,step_id,execution_id,response_ref,response_pointer,validation_status,outcome,record) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,'accepted',$12,$13) ON CONFLICT DO NOTHING").bind(self.id).bind(&e.evidence_id).bind(self.person.owner).bind(public.plan_revision as i32).bind(&e.request_id).bind(&e.subject_id).bind(&e.field_id).bind(&e.step_id).bind(e.execution_id).bind(e.response_ref).bind(&e.response_pointer).bind(if e.outcome==FieldOutcome::Conflicting{"conflicting"}else{"supported"}).bind(serde_json::to_value(e)?).execute(&mut *tx).await?;
        }
        sqlx::query("INSERT INTO ask_run_answers(run_id,plan_revision,owner_id,status,model,synthesis_calls,repair_calls,answer,field_outcomes,usage) VALUES($1,$2,$3,$4,$5,$6,0,$7,$8,$9) ON CONFLICT DO NOTHING").bind(self.id).bind(public.plan_revision as i32).bind(self.person.owner).bind(if output.status==SynthesisStatus::Verified{"verified"}else{"deterministic_fallback"}).bind(&output.model).bind(output.calls.synthesis as i32).bind(saved_answer).bind(serde_json::to_value(&set.field_outcomes)?).bind(serde_json::to_value(&output.usage)?).execute(&mut *tx).await?;
        tx.commit().await?;
        self.event(
            completion_stage,
            EventStatus::Completed,
            Counts {
                evidence_accepted: Some(set.records.len() as u32),
                ..Counts::default()
            },
        )
        .await
    }
    pub async fn approve(&self, request: ApproveRunRequest, bearer: String) -> Result<()> {
        let row = self.row().await?;
        let mut plan = self.saved_plan().await?;
        ensure!(
            matches!(
                row.get::<String, _>("stage").as_str(),
                "awaiting_approval" | "executing"
            ),
            "run_not_awaiting_approval"
        );
        ensure!(
            request.plan_revision == plan.plan_revision
                && request.quote_ref == plan.quote.quote_ref
                && request.max_total_atomic == plan.quote.total_atomic.0.to_string(),
            "quote_mismatch"
        );
        ensure!(plan.quote.expires_at > Utc::now(), "quote_expired");
        self.engine.store.check_spending(&self.person).await?;
        let (_, task, _) = self.engine.store.load_scoped(&self.person, self.id).await?;
        let saved: crate::resolver::Plan = serde_json::from_value(
            task.proposal
                .ok_or_else(|| anyhow::anyhow!("quote_mismatch"))?,
        )?;
        let ids: Vec<_> = saved.steps.iter().map(|s| s.service.id).collect();
        let current = self.engine.catalog.fresh_execution(&ids).await?;
        ensure!(
            saved.steps.iter().all(|s| current
                .services
                .iter()
                .any(|c| c.id == s.service.id && c.version_hash() == s.service.version_hash())),
            "quote_mismatch"
        );
        let claimed=sqlx::query("UPDATE ask_runs SET stage='executing' WHERE id=$1 AND plan_revision=$2 AND stage IN ('awaiting_approval','executing')").bind(self.id).bind(request.plan_revision as i32).execute(&self.engine.store.pool).await?.rows_affected();
        ensure!(claimed == 1, "quote_mismatch");
        let authorization = self
            .engine
            .executor
            .approve(
                &self.person,
                self.id,
                request.quote_ref,
                &request.max_total_atomic,
            )
            .await?;
        plan.approval = Approval {
            state: ApprovalState::Approved,
            approved_at: Some(Utc::now()),
            authorization_ref: Some(authorization),
        };
        sqlx::query("UPDATE ask_run_plans SET source_plan=$2 WHERE run_id=$1 AND plan_revision=$3")
            .bind(self.id)
            .bind(serde_json::to_value(plan)?)
            .bind(request.plan_revision as i32)
            .execute(&self.engine.store.pool)
            .await?;
        self.event(Stage::Executing, EventStatus::Running, Counts::default())
            .await?;
        self.resume(bearer).await
    }
    pub async fn clarify(&self, request: ClarifyRunRequest, bearer: String) -> Result<()> {
        let row = self.row().await?;
        ensure!(
            row.get::<String, _>("stage") == "needs_clarification",
            "clarification_stale"
        );
        let input: PlannerInput = serde_json::from_value(row.get("planner_input"))?;
        let asked: ClarificationRequest = serde_json::from_value(row.get("clarification"))?;
        let catalog = self.engine.catalog.get().await?;
        let answer = ClarificationAnswer {
            clarification_id: request.clarification_id,
            question_revision: request.question_revision,
            option_id: request.option_id,
            value: request.value,
        };
        let next = planner::apply_clarification(&input, &asked, &answer, &catalog)?;
        let updated=sqlx::query("UPDATE ask_runs SET planner_input=$2,question_revision=$3,clarification=NULL,stage='finding_sources' WHERE id=$1 AND question_revision=$4 AND stage='needs_clarification'").bind(self.id).bind(serde_json::to_value(&next)?).bind(next.question_revision as i32).bind(input.question_revision as i32).execute(&self.engine.store.pool).await?.rows_affected();
        ensure!(updated == 1, "clarification_stale");
        self.event(
            Stage::FindingSources,
            EventStatus::Running,
            Counts::default(),
        )
        .await?;
        self.resume(bearer).await
    }
    pub async fn replan(&self, request: ReplanRunRequest, bearer: String) -> Result<()> {
        self.row().await?;
        let mut tx = self.engine.store.pool.begin().await?;
        let row = sqlx::query("SELECT stage,plan_revision FROM ask_runs WHERE id=$1 FOR UPDATE")
            .bind(self.id)
            .fetch_one(&mut *tx)
            .await?;
        ensure!(
            row.get::<String, _>("stage") == "awaiting_approval"
                && row.get::<Option<i32>, _>("plan_revision") == Some(request.plan_revision as i32),
            "replan_blocked"
        );
        let authorized: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM authorizations WHERE task_id=$1)")
                .bind(self.id)
                .fetch_one(&mut *tx)
                .await?;
        ensure!(!authorized, "replan_blocked");
        sqlx::query(
            "UPDATE ask_run_plans SET superseded_at=now() WHERE run_id=$1 AND plan_revision=$2",
        )
        .bind(self.id)
        .bind(request.plan_revision as i32)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE ask_runs SET plan_revision=NULL,stage='building_plan' WHERE id=$1")
            .bind(self.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.event(Stage::BuildingPlan, EventStatus::Running, Counts::default())
            .await?;
        self.resume(bearer).await
    }
    pub async fn cancel(&self) -> Result<()> {
        if self.row().await?.get::<String, _>("stage") == "cancelled" {
            return Ok(());
        }
        for _ in 0..3 {
            let (revision, mut task, _) =
                self.engine.store.load_scoped(&self.person, self.id).await?;
            task.cancelled = true;
            task.actions.retain(|a| a["kind"] == "poll");
            if self
                .engine
                .store
                .save(self.person.owner, self.id, revision, &task)
                .await
                .is_ok()
            {
                self.event(Stage::Cancelled, EventStatus::Cancelled, Counts::default())
                    .await?;
                return Ok(());
            }
        }
        anyhow::bail!("state_conflict")
    }
    pub async fn events(&self, after: u64) -> Result<EventPage> {
        let row = self.row().await?;
        let stage: Stage = serde_json::from_value(json!(row.get::<String, _>("stage")))?;
        let values=sqlx::query_scalar::<_,Value>("SELECT event FROM ask_run_events WHERE run_id=$1 AND sequence>$2 ORDER BY sequence LIMIT 200").bind(self.id).bind(after.min(i64::MAX as u64) as i64).fetch_all(&self.engine.store.pool).await?;
        let events = values
            .into_iter()
            .map(serde_json::from_value)
            .collect::<std::result::Result<Vec<ProgressEvent>, _>>()?;
        let last = row.get::<i64, _>("last_sequence") as u64;
        Ok(EventPage {
            run_id: self.id,
            more: events.last().is_some_and(|e| e.sequence < last),
            events,
            last_sequence: last,
            stage,
            terminal: stage.terminal(),
        })
    }
    pub async fn snapshot(&self) -> Result<RunSnapshot> {
        let row = self.row().await?;
        let stage: Stage = serde_json::from_value(json!(row.get::<String, _>("stage")))?;
        let plan = if row.get::<Option<i32>, _>("plan_revision").is_some() {
            Some(self.saved_plan().await?)
        } else {
            None
        };
        let (_, task, _) = self.engine.store.load_scoped(&self.person, self.id).await?;
        let answer:Option<Value>=sqlx::query_scalar("SELECT answer FROM ask_run_answers WHERE run_id=$1 ORDER BY plan_revision DESC LIMIT 1").bind(self.id).fetch_optional(&self.engine.store.pool).await?;
        let evidence = sqlx::query_scalar::<_, Value>(
            "SELECT record FROM ask_run_evidence WHERE run_id=$1 ORDER BY evidence_id",
        )
        .bind(self.id)
        .fetch_all(&self.engine.store.pool)
        .await?
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<Vec<EvidenceRecord>, _>>()?;
        let latest = sqlx::query_scalar::<_, Value>(
            "SELECT event FROM ask_run_events WHERE run_id=$1 ORDER BY sequence DESC LIMIT 1",
        )
        .bind(self.id)
        .fetch_optional(&self.engine.store.pool)
        .await?
        .map(serde_json::from_value)
        .transpose()?;
        let receipts = sqlx::query_scalar::<_, Value>(
            "SELECT document FROM results WHERE task_id=$1 AND owner_id=$2",
        )
        .bind(self.id)
        .bind(self.person.owner)
        .fetch_all(&self.engine.store.pool)
        .await?;
        let validated: Vec<Value> = sqlx::query_scalar(
            "SELECT DISTINCT ON (event->>'step_id') event FROM ask_run_events WHERE run_id=$1 AND stage='validating_results' AND event->>'step_id' IS NOT NULL ORDER BY event->>'step_id',sequence DESC",
        ).bind(self.id).fetch_all(&self.engine.store.pool).await?;
        let sources = plan
            .iter()
            .flat_map(|p| &p.sources)
            .map(|s| {
                let running = task
                    .pending_dispatches
                    .iter()
                    .any(|p| p.step == s.step_index as usize);
                let failed = task.failed_steps.contains_key(&s.step_id);
                let done = task.completed.contains(&s.step_id);
                let count = evidence.iter().filter(|e| e.step_id == s.step_id).count() as u32;
                let receipt = receipts.iter().find(|r| {
                    r["source"]["provider"] == s.source_id
                        && r["source"]["path"]
                            == task.proposal.as_ref().unwrap()["steps"][s.step_index as usize]
                                ["service"]["path"]
                        && r["subject"]["entity_ref"]
                            == task.proposal.as_ref().unwrap()["steps"][s.step_index as usize]
                                ["entity_ref"]
                });
                SourceRow {
                    step_id: s.step_id.clone(),
                    source_id: s.source_id.clone(),
                    source_name: s.source_name.clone(),
                    status: if let Some(status) = validated
                        .iter()
                        .find(|e| e["step_id"] == s.step_id)
                        .and_then(|e| serde_json::from_value(e["source_status"].clone()).ok())
                    {
                        status
                    } else if failed {
                        SourceRowStatus::Failed
                    } else if running {
                        SourceRowStatus::Running
                    } else if done {
                        if count > 0 {
                            SourceRowStatus::Succeeded
                        } else if !stage.terminal() {
                            SourceRowStatus::Validating
                        } else {
                            SourceRowStatus::Partial
                        }
                    } else if task.cancelled {
                        SourceRowStatus::Cancelled
                    } else if s.depends_on.is_empty() {
                        SourceRowStatus::Queued
                    } else {
                        SourceRowStatus::WaitingForPrerequisite
                    },
                    waiting_for: s.depends_on.iter().map(|d| d.step_id.clone()).collect(),
                    started_at: None,
                    finished_at: None,
                    execution_id: None,
                    result_ref: evidence
                        .iter()
                        .find(|e| e.step_id == s.step_id)
                        .map(|e| e.response_ref),
                    response_received: done,
                    evidence_accepted: count,
                    evidence_excluded: 0,
                    charge_status: if receipt.is_some() {
                        ChargeStatus::Captured
                    } else if running {
                        ChargeStatus::Reserved
                    } else if failed {
                        ChargeStatus::PendingReconciliation
                    } else {
                        ChargeStatus::NotStarted
                    },
                    charged_atomic: Amount(
                        receipt
                            .and_then(|r| r["charged_atomic"].as_str())
                            .and_then(|n| n.parse().ok())
                            .unwrap_or(0),
                    ),
                    error: None,
                }
            })
            .collect();
        let billing = self
            .engine
            .view(&self.person, self.id, Uuid::new_v4(), None)
            .await
            .ok()
            .and_then(|r| serde_json::to_value(r).ok())
            .and_then(|v| v.get("billing").cloned());
        Ok(RunSnapshot {
            run_contract_version: 1,
            run_id: self.id,
            request_id: row.get("request_id"),
            question: row.get("question"),
            language: row
                .get::<Option<Value>, _>("planner_input")
                .and_then(|v| v["language"].as_str().map(str::to_owned)),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            stage,
            terminal: stage.terminal(),
            question_revision: row.get::<i32, _>("question_revision") as u32,
            plan_revision: row.get::<Option<i32>, _>("plan_revision").map(|n| n as u32),
            last_sequence: row.get::<i64, _>("last_sequence") as u64,
            latest_event: latest,
            clarification: row
                .get::<Option<Value>, _>("clarification")
                .map(serde_json::from_value)
                .transpose()?,
            plan,
            gaps: serde_json::from_value(row.get("gaps"))?,
            sources,
            billing,
            evidence,
            field_outcomes: sqlx::query_scalar::<_,Value>("SELECT field_outcomes FROM ask_run_answers WHERE run_id=$1 ORDER BY plan_revision DESC LIMIT 1").bind(self.id).fetch_optional(&self.engine.store.pool).await?.map(serde_json::from_value).transpose()?.unwrap_or_default(),
            answer: answer.map(serde_json::from_value).transpose()?,
            error: row
                .get::<Option<Value>, _>("error")
                .map(serde_json::from_value)
                .transpose()?,
            actions: RunActions {
                approve: stage == Stage::AwaitingApproval,
                cancel: !stage.terminal(),
                replan: stage == Stage::AwaitingApproval,
                clarify: stage == Stage::NeedsClarification,
            },
        })
    }
}
impl ProgressSink for Run {
    async fn emit(&self, u: StageUpdate) -> Result<()> {
        self.event(u.stage, u.status, u.counts).await
    }
}
