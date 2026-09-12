mod execute;
mod followups;
mod results;
use crate::{
    auth::Principal,
    catalog::{Catalog, CatalogCache},
    execution::{Dispatch, Executor},
    parser::{Intent, Parser},
    protocol::*,
    resolver::{self, Action, Plan, Resolution},
    state::{digest, Entity, Fact, Signer, Task},
    store::{Claim, Store},
};
use chrono::Utc;
pub use followups::FollowupRequest;
use serde_json::{json, Value};
use uuid::Uuid;

pub struct Engine {
    pub store: Store,
    pub catalog: CatalogCache,
    pub parser: Parser,
    pub signer: Signer,
    pub executor: Executor,
    pub app_url: String,
}
pub fn single_call_mode() -> bool { std::env::var("V2_SINGLE_CALL_MODE").as_deref() == Ok("1") }

impl Engine {
    async fn request_claim(
        &self,
        p: &Principal,
        id: Uuid,
        hash: &str,
    ) -> anyhow::Result<Option<Response>> {
        Ok(match self.store.claim(p.owner, id, hash).await? {
            Claim::New => None,
            Claim::Replay(r) => Some(*r),
            Claim::Conflict => Some(Response::error(
                id,
                Status::Failed,
                "request_conflict",
                "This request ID is already associated with different input.",
            )),
            Claim::Busy => {
                let mut r=Response::error(id,Status::Running,"request_pending","Deze request is al gestart; herhaal dezelfde request-ID of herstel de taak. Een onderbroken parserrequest wordt niet opnieuw geparseerd.");
                r.retry_after_ms = Some(2000);
                Some(r)
            }
        })
    }
    async fn complete(
        &self,
        p: &Principal,
        id: Uuid,
        result: anyhow::Result<Response>,
    ) -> Response {
        let r = result.unwrap_or_else(|e| {
            tracing::warn!(error=%e,"v2 operation failed");
            if let Some(model) = e.downcast_ref::<crate::parser::ModelFailure>() {
                let (status, code) = match model {
                    crate::parser::ModelFailure::Unsupported => {
                        (Status::Unsupported, "unsupported_request")
                    }
                    crate::parser::ModelFailure::Unavailable => {
                        (Status::Failed, "question_unavailable")
                    }
                };
                return Response::error(id, status, code, &model.to_string());
            }
            let message = if e.downcast_ref::<sqlx::Error>().is_some() {
                "Task storage is temporarily unavailable. Recover the task before continuing."
                    .to_string()
            } else {
                e.to_string()
            };
            Response::error(id, Status::Failed, "operation_failed", &message)
        });
        if self.store.finish(p.owner, &r).await.is_err() {
            tracing::error!("request receipt persistence failed");
        }
        r
    }
    pub async fn discover(&self, p: &Principal, req: DiscoverRequest) -> anyhow::Result<Response> {
        if let Some(r) = self
            .request_claim(
                p,
                req.request_id,
                &digest(&("discover", p.connection, &req)),
            )
            .await?
        {
            return Ok(r);
        }
        let result = self.discover_inner(p, &req, false).await;
        Ok(self.complete(p, req.request_id, result).await)
    }
    pub async fn chat(&self, p: &Principal, req: DiscoverRequest) -> anyhow::Result<Response> {
        anyhow::ensure!(p.is_person(), "Ask chat requires a person in the App");
        if let Some(r) = self
            .request_claim(p, req.request_id, &digest(&("ask-chat", &req)))
            .await?
        {
            return Ok(r);
        }
        let result = self.discover_inner(p, &req, true).await;
        Ok(self.complete(p, req.request_id, result).await)
    }
    async fn discover_inner(
        &self,
        p: &Principal,
        req: &DiscoverRequest,
        app_chat: bool,
    ) -> anyhow::Result<Response> {
        anyhow::ensure!(
            !req.question.trim().is_empty() && req.question.len() <= MAX_QUESTION,
            "invalid question length"
        );
        let id = if let Some(state) = &req.state {
            self.signer.verify(p.owner, state, true)?;
            if state.expires_at <= Utc::now() {
                return self
                    .view(
                        p,
                        state.state_ref,
                        req.request_id,
                        Some(Status::StateConflict),
                    )
                    .await;
            }
            state.state_ref
        } else {
            self.store.create(p.owner, p.connection).await?
        };
        let (revision, mut task, connection) = self.store.load(p.owner, id).await?;
        anyhow::ensure!(
            connection == p.connection,
            "task belongs to another connection; use explicit task recovery"
        );
        if req.state.as_ref().is_some_and(|s| s.revision != revision) {
            return self
                .view(p, id, req.request_id, Some(Status::StateConflict))
                .await;
        }
        let in_flight:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM executions e JOIN tasks t ON t.id=e.task_id WHERE e.task_id=$1 AND (e.status IN ('dispatching','unresolved') OR (e.status='succeeded' AND NOT EXISTS(SELECT 1 FROM results r WHERE r.execution_id=e.id AND (t.document->'result_refs') ? r.id::text))))").bind(id).fetch_one(&self.store.pool).await?;
        anyhow::ensure!(
            !in_flight,
            "existing execution must finish or reconcile before a new question"
        );
        self.prepare_conversation(p.owner, id, &mut task).await?;
        for focus in &req.context_delta.requested_focus_refs {
            anyhow::ensure!(
                task.entities.iter().any(|e| e.entity_ref == *focus),
                "unknown focus entity"
            );
        }
        if !req.context_delta.requested_focus_refs.is_empty() {
            task.focus.entity_refs = req.context_delta.requested_focus_refs.clone()
        }
        let catalog = self.catalog.get().await?;
        for f in &req.context_delta.explicit_values {
            self.user_fact(
                &mut task,
                &catalog,
                f.entity_ref,
                &f.semantic_type,
                f.value.clone(),
            )?;
        }
        if app_chat {
            let reply = self.parser.chat(&req.question, &task, &catalog).await?;
            self.archive_conversation_output(p, id, req.request_id, &mut task).await?;
            task.record_turn(&req.question, Some(reply.message));
            if !reply.discover {
                task.intent = None;
                task.proposal = None;
                task.actions.clear();
                task.completed.clear();
                task.failed_steps.clear();
                task.active_results.clear();
                task.analysis = None;
                task.pending_candidates.clear();
                task.original_subject = None;
                task.execution_error = None;
                task.cancelled = false;
                task.focus.goal_refs.clear();
                self.store.save_intent(p.owner, id, revision, &task).await?;
                return self
                    .view(p, id, req.request_id, Some(Status::NeedsInput))
                    .await;
            }
        }
        let mut intent = self
            .parser
            .parse(p.owner, &req.question, &task, &catalog)
            .await?;
        if !app_chat {
            self.archive_conversation_output(p, id, req.request_id, &mut task).await?;
            task.record_turn(&req.question, None);
        }
        for subject in &mut intent.subjects {
            let entity = subject.entity_ref.unwrap_or_else(Uuid::new_v4);
            if subject.entity_ref.is_none() {
                task.entities.push(Entity {
                    entity_ref: entity,
                    label: subject.label.clone(),
                    selected: true,
                })
            }
            subject.entity_ref = Some(entity);
            for (kind, value) in &subject.values {
                self.user_fact(&mut task, &catalog, entity, kind, value.clone())?;
            }
        }
        task.focus.entity_refs = intent
            .subjects
            .iter()
            .filter_map(|s| s.entity_ref)
            .collect();
        task.focus.goal_refs = vec![Uuid::new_v4()];
        task.intent = Some(serde_json::to_value(&intent)?);
        task.completed.clear();
        task.failed_steps.clear();
        task.active_results.clear();
        task.analysis = None;
        task.pending_candidates.clear();
        task.original_subject = None;
        task.cancelled = false;
        task.execution_error = None;
        let mut status = self.plan(&mut task, &catalog, &intent)?;
        if let Resolution::Unsupported(reason) = resolver::resolve(&catalog, &task, &intent) {
            let review = self.parser.review_plan(&req.question, &task, &catalog, &intent, &reason).await?;
            if let Some(revised) = review.intent {
                task.intent = Some(serde_json::to_value(&revised)?);
                status = self.plan(&mut task, &catalog, &revised)?;
            } else {
                task.proposal = None;
                task.actions = vec![json!({"kind":"planning_clarification","message":review.message})];
                status = Status::NeedsInput;
            }
            if let Some(turn) = task.conversation.last_mut().filter(|turn| turn.question == req.question) {
                turn.reply = Some(review.message);
            } else {
                task.record_turn(&req.question, Some(review.message));
            }
        }
        self.store.save_intent(p.owner, id, revision, &task).await?;
        self.view(p, id, req.request_id, Some(status)).await
    }
    fn user_fact(
        &self,
        task: &mut Task,
        catalog: &Catalog,
        entity: Uuid,
        kind: &str,
        value: Value,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            task.entities.iter().any(|e| e.entity_ref == entity),
            "unknown entity for explicit value"
        );
        let ft = catalog
            .vocabulary
            .get(kind)
            .ok_or_else(|| anyhow::anyhow!("unknown fact type"))?;
        catalog_core::facts::schema::validate(&ft.value_schema, &value)?;
        if task.facts.iter().any(|f| {
            f.entity_ref == entity
                && f.semantic_type == kind
                && f.value == value
                && f.expires_at > Utc::now()
        }) {
            return Ok(());
        }
        task.facts.push(Fact {
            entity_ref: entity,
            semantic_type: kind.into(),
            value,
            provenance: json!({"kind":"user_input"}),
            observed_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            verification: "user_asserted".into(),
        });
        Ok(())
    }
    fn plan(&self, task: &mut Task, catalog: &Catalog, intent: &Intent) -> anyhow::Result<Status> {
        task.actions.clear();
        task.proposal = None;
        match resolver::resolve(catalog, task, intent) {
            Resolution::Plan(plan) => {
                if single_call_mode() && plan.steps.len() > 1 {
                    task.actions.push(json!({"kind":"unsupported_reason","message":"Multi-step research is paused. Ask for one direct API call, such as a company search. To retrieve a profile or filing directly, provide its required identifier."}));
                    return Ok(Status::Unsupported);
                }
                task.proposal = Some(serde_json::to_value(&plan)?);
                if !plan.steps.is_empty() {
                    task.actions.push(serde_json::to_value(Action::Run {
                        action_id: Uuid::new_v4(),
                        step: 0,
                    })?);
                }
                Ok(Status::Ready)
            }
            Resolution::Missing { entity, kind } => {
                let schema = catalog
                    .vocabulary
                    .get(&kind)
                    .map(|v| v.value_schema.clone())
                    .unwrap_or(json!({}));
                task.actions.push(serde_json::to_value(Action::Supply {
                    action_id: Uuid::new_v4(),
                    entity_ref: entity,
                    semantic_type: kind,
                    schema,
                })?);
                Ok(Status::NeedsInput)
            }
            Resolution::Unsupported(reason) => {
                task.actions
                    .push(json!({"kind":"unsupported_reason","message":reason}));
                Ok(Status::Unsupported)
            }
        }
    }
    async fn prepare_conversation(&self, owner: Uuid, id: Uuid, task: &mut Task) -> anyhow::Result<()> {
        if task.conversation_timeline_version != 0 { return Ok(()); }
        if let Some(result_ref) = task.result_refs.last().copied() {
            if let Some(snapshot) = self.store.original_result_turn(owner, id, result_ref).await? {
                task.restore_legacy_output(&snapshot, result_ref);
            }
        }
        task.conversation_timeline_version = 1;
        Ok(())
    }

    async fn archive_conversation_output(&self, p: &Principal, id: Uuid, request: Uuid, task: &mut Task) -> anyhow::Result<()> {
        if task.proposal.is_none() && task.active_result_ref().is_none() { return Ok(()); }
        let previous = self.view(p, id, request, None).await?;
        task.archive_output(previous.proposal.map(serde_json::to_value).transpose()?);
        Ok(())
    }

    pub async fn view(
        &self,
        p: &Principal,
        id: Uuid,
        request: Uuid,
        forced: Option<Status>,
    ) -> anyhow::Result<Response> {
        let (revision, mut task, connection) = self.store.load(p.owner, id).await?;
        anyhow::ensure!(
            p.is_person() || connection == p.connection,
            "task belongs to another connection"
        );
        self.prepare_conversation(p.owner, id, &mut task).await?;
        let mut actions: Vec<Action> = task
            .actions
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        let single = single_call_mode();
        let paused_plan = single
            && task
                .proposal
                .as_ref()
                .and_then(|v| v["steps"].as_array())
                .is_some_and(|v| v.len() > 1);
        if paused_plan {
            actions.retain(|a| {
                !matches!(
                    a,
                    Action::Run { .. } | Action::Choose { .. } | Action::Supply { .. }
                )
            });
        }
        let inferred = if task.execution_error.is_some() && task.result_refs.is_empty() {
            Status::Failed
        } else if task.cancelled {
            Status::Partial
        } else if paused_plan {
            if task.result_refs.is_empty() {
                Status::Unsupported
            } else {
                Status::Partial
            }
        } else if !task.pending_candidates.is_empty() {
            Status::NeedsSelection
        } else if actions.iter().any(|a| matches!(a, Action::Supply { .. }))
            || task
                .actions
                .iter()
                .any(|v| v["kind"] == "planning_clarification")
        {
            Status::NeedsInput
        } else if actions.iter().any(|a| {
            matches!(
                a,
                Action::Poll { .. } | Action::Wait { .. } | Action::Analyze { .. }
            )
        }) {
            Status::Running
        } else if actions.iter().any(|a| matches!(a, Action::Run { .. })) {
            Status::Ready
        } else if task
            .actions
            .iter()
            .any(|v| v["kind"] == "unsupported_reason")
        {
            Status::Unsupported
        } else if task.intent.is_none() && !task.conversation.is_empty() {
            Status::NeedsInput
        } else if !task.failed_steps.is_empty() {
            if task.active_results.is_empty() { Status::Failed } else { Status::Partial }
        } else if task.analysis.as_ref().is_some_and(|a| a["status"] != "completed") {
            Status::Partial
        } else if !task.result_refs.is_empty() {
            Status::Succeeded
        } else {
            Status::Ready
        };
        let mut r = Response::new(request, forced.unwrap_or(inferred));
        r.intent_ref = task.focus.goal_refs.first().copied();
        r.context_view = task.context_view();
        // Hydrate saved turn results through the same owner-scoped reader as
        // the current result, including freshly signed report download links.
        for (index, turn) in task.conversation.iter().enumerate() {
            if let Some(result_ref) = turn.output.as_ref().and_then(|output| output.result_ref) {
                r.context_view["conversation"][index]["output"]["result"] =
                    self.result_page(p.owner, id, result_ref, 0).await?;
            }
            if let Some(output) = &turn.output {
                let mut results = Vec::new();
                for result in &output.result_refs { results.push(self.result_page(p.owner, id, *result, 0).await?); }
                r.context_view["conversation"][index]["output"]["results"] = json!(results);
            }
        }
        let mut results = Vec::new();
        for result in &task.active_results { results.push(self.result_page(p.owner, id, *result, 0).await?); }
        r.context_view["results"] = json!(results);
        r.context_view["result_is_historical"] = json!(
            !task.result_refs.is_empty() && task.active_result_ref().is_none()
        );
        r.context_view["single_call_mode"] = json!(single);
        r.context_view["research_paused"] = json!(paused_plan);
        r.context_view["execution_enabled"] = json!(self.executor.enabled);
        r.context_view["approval_mode"] = json!(if self.executor.connection_can_approve(p).await.unwrap_or(false) {
            "chatbot"
        } else { "app" });
        r.context_view["execution_owner"] = json!(if connection.is_some() {
            "connected_agent"
        } else {
            "app"
        });
        if !single && p.is_person() && connection.is_none() && !task.result_refs.is_empty() {
            // Recommendations are optional; catalog downtime must not hide a purchased result.
            if let Ok(catalog) = self.catalog.get().await {
                r.context_view["related_services"] =
                    json!(crate::followups::related_services(&catalog, &task));
            }
        }
        if !paused_plan && !task.pending_candidates.is_empty() {
            r.context_view["candidates"] = json!(task.pending_candidates)
        }
        r.next_actions = actions.iter().map(Action::public).collect();
        for result in task.result_refs.iter().rev().take(8) {
            r.next_actions.push(
                Action::Read {
                    action_id: *result,
                    result_ref: *result,
                }
                .public(),
            );
        }
        if task.proposal.is_some() && !task.cancelled {
            r.next_actions
                .push(Action::Cancel { action_id: id }.public());
        }
        let billing_plan = task
            .proposal
            .clone()
            .map(serde_json::from_value::<Plan>)
            .transpose()?;
        r.billing = Some(
            self.store
                .billing(
                    p.owner,
                    id,
                    billing_plan.as_ref().map(|v| v.quote_ref),
                    billing_plan.as_ref().map_or(0, |v| v.cap_atomic),
                )
                .await?,
        );
        if let Some(error) = &task.execution_error {
            r.errors.push(error.clone());
        }
        if actions.is_empty() { r.errors.extend(task.failed_steps.values().cloned()); }
        if let Some(value) = &task.proposal {
            let plan: Plan = serde_json::from_value(value.clone())?;
            let sources = self.catalog.sources().await.unwrap_or_default();
            let mut step_details = crate::resolver::describe_steps(&plan, &sources);
            let current = plan.steps.iter().position(|s| !task.step_finished(&s.key()));
            for (i, detail) in step_details.iter_mut().enumerate() {
                detail["subject"] = json!(task.entities.iter().find(|e| e.entity_ref == plan.steps[i].entity_ref).map(|e| &e.label));
                detail["entity_ref"] = json!(plan.steps[i].entity_ref);
                if let Some(error) = task.failed_steps.get(&plan.steps[i].key()) { detail["error"] = error.clone(); }
                detail["status"] = json!(if task.failed_steps.contains_key(&plan.steps[i].key()) || (current == Some(i) && task.execution_error.is_some()) { "failed" }
                    else if task.completed.contains(&plan.steps[i].key()) { "completed" }
                    else if current == Some(i) && actions.iter().any(|a|matches!(a, Action::Poll {..})) { "running" }
                    else if task.cancelled { "cancelled" } else if paused_plan { "paused" } else { "pending" });
            }
            r.proposal = Some(Proposal {
                label: "Data request".into(),
                step_details,
                quote_ref: plan.quote_ref,
                price_status: "capped".into(),
                currency: "USD".into(),
                max_total_atomic: plan.cap_atomic.to_string(),
                expires_at: plan.expires_at,
                approval_url: format!("{}/gateway-v2?task={}", self.app_url, id),
                steps: plan
                    .steps
                    .iter()
                    .map(|s| s.service.contract.capability_slug.clone())
                    .collect(),
            });
        }
        for value in &task.actions {
            if value["kind"] == "unsupported_reason" {
                r.errors
                    .push(json!({"code":"unsupported","message":value["message"]}))
            }
        }
        if r.status == Status::Running {
            r.retry_after_ms = Some(2000)
        }
        if let Some(Action::Wait { until, .. }) =
            actions.iter().find(|a| matches!(a, Action::Wait { .. }))
        {
            r.context_view["cooldown"] = json!({"until":until,"message":"The annual account is queued or being retrieved from KVK. Requests for the same report share one job; the result will appear automatically."});
            r.retry_after_ms = Some((*until - Utc::now()).num_milliseconds().max(350) as u64);
        }
        r.context_view["analyzing"] =
            json!(actions.iter().any(|a| matches!(a, Action::Analyze { .. })));
        if let Some(result_ref) = task.result_refs.last() {
            r.result = Some(self.result_page(p.owner, id, *result_ref, 0).await?);
        }
        r.state = Some(self.signer.issue(p.owner, id, revision, task.focus));
        Ok(r)
    }
}
