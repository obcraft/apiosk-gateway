mod execute;
mod followups;
mod results;
mod supplier;
mod workflows;
mod worker;
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
        self.store.check_workspace(p).await?;
        let scoped_hash = p.workspace.map(|w|digest(&(w,hash)));
        Ok(match self.store.claim(p.owner, id, scoped_hash.as_deref().unwrap_or(hash)).await? {
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
        self.intake(p, req, app_chat, None).await
    }
    /// `forced`: a code-built intent (workflow pages) that skips interpretation.
    pub(crate) async fn intake(
        &self,
        p: &Principal,
        req: &DiscoverRequest,
        app_chat: bool,
        forced: Option<crate::parser::Intent>,
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
            self.store.create_scoped(p.owner, p.connection, p.workspace).await?
        };
        let (revision, mut task, connection) = self.store.load_scoped(&p, id).await?;
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
        let mut chat_intent = None;
        // Invoice checks must not depend on the model recognising the wording.
        let invoice_fallback = if forced.is_none() { crate::supplier_check::fallback_intent(&req.question, &catalog) } else { None };
        // A plain invoice check needs no interpretation: skip the model call.
        let forced = forced.or_else(|| crate::supplier_check::direct_intent(&req.question, &catalog));
        if app_chat && forced.is_none() && crate::supplier_check::needs_invoice_upload(&req.question, &catalog) {
            // Deterministic: ask for the invoice in the conversation, no model call.
            self.archive_conversation_output(p, id, req.request_id, &mut task).await?;
            task.record_turn(&req.question, Some(crate::supplier_check::upload_message(&req.question)));
            task.intent = None;
            task.proposal = None;
            task.completed.clear();
            task.failed_steps.clear();
            task.active_results.clear();
            task.analysis = None;
            task.supplier_check = None;
            task.pending_candidates.clear();
            task.original_subject = None;
            task.execution_error = None;
            task.cancelled = false;
            task.focus.goal_refs.clear();
            task.actions = vec![serde_json::to_value(Action::Upload { action_id: Uuid::new_v4(), purpose: "invoice".into() })?];
            self.store.save_intent(p.owner, id, revision, &task).await?;
            return self.view(p, id, req.request_id, Some(Status::NeedsInput)).await;
        }
        if app_chat && forced.is_some() {
            self.archive_conversation_output(p, id, req.request_id, &mut task).await?;
            task.record_turn(&req.question, Some(crate::supplier_check::routing_message(&req.question)));
        }
        if app_chat && forced.is_none() {
            let mut reply = match (self.parser.chat(&req.question, &task, &catalog).await, &invoice_fallback) {
                (Ok(reply), _) => reply,
                (Err(_), Some(fallback)) => crate::parser::ChatReply { message: crate::supplier_check::routing_message(&req.question), discover: true, intent: Some(fallback.clone()) },
                (Err(error), None) => return Err(error),
            };
            if let Some(fallback) = invoice_fallback.as_ref().filter(|_| !reply.discover || reply.intent.is_none()) {
                reply = crate::parser::ChatReply { message: crate::supplier_check::routing_message(&req.question), discover: true, intent: Some(fallback.clone()) };
            }
            chat_intent = reply.intent;
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
                task.supplier_check = None;
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
        let mut intent = match (forced, chat_intent) {
            (Some(intent), _) | (None, Some(intent)) => intent,
            // Compatibility for an older conversational model response.
            (None, None) => match (self.parser.parse(p.owner, &req.question, &task, &catalog).await, invoice_fallback) {
                (Ok(intent), _) => intent,
                (Err(_), Some(fallback)) => fallback,
                (Err(error), None) => return Err(error),
            },
        };
        if crate::supplier_check::route(&req.question, &mut intent, &catalog) {
            tracing::info!("invoice question routed to the supplier payment check");
        }
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
        task.supplier_check = None;
        if let Some(expansion) = crate::supplier_check::expand(&req.question, &mut intent, &catalog) {
            for (kind, value) in &expansion.facts {
                self.user_fact(&mut task, &catalog, expansion.invoice.entity_ref, kind, value.clone())?;
            }
            task.supplier_check = Some(serde_json::to_value(&expansion.invoice)?);
        }
        task.focus.entity_refs = intent
            .subjects
            .iter()
            .filter_map(|s| s.entity_ref)
            .collect();
        task.focus.goal_refs = vec![Uuid::new_v4()];
        task.intent = Some(serde_json::to_value(&intent)?);
        task.completed.clear();
        task.pending_dispatches.clear();
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
            Resolution::Plan(mut plan) => {
                if task.supplier_check.is_some() {
                    // Workflow runs carry the workflow markup on every step.
                    plan.apply_markup(catalog_core::fees::WORKFLOW_MARKUP_BPS as i64);
                }
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
        let (revision, mut task, connection) = self.store.load_scoped(&p, id).await?;
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
        } else if actions.iter().any(|a| matches!(a, Action::Supply { .. } | Action::Upload { .. }))
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
        r.context_view["money_display"] = self.store.money_display(p.owner).await;
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
                if let Some(quote)=output.proposal.as_ref().and_then(|p|p["quote_ref"].as_str()).and_then(|s|s.parse().ok()) {
                    if let Ok(report)=self.research_report_link(p.owner,id,&task,quote).await {
                        r.context_view["conversation"][index]["output"]["report"]=report;
                    }
                }
            }
        }
        let mut results = Vec::new();
        for result in &task.active_results { results.push(self.result_page(p.owner, id, *result, 0).await?); }
        r.context_view["results"] = json!(results);
        if let Some(quote)=task.proposal.as_ref().and_then(|p|p["quote_ref"].as_str()).and_then(|s|s.parse().ok()) {
            if let Ok(report)=self.research_report_link(p.owner,id,&task,quote).await {r.context_view["report"]=report;}
        }
        r.context_view["result_is_historical"] = json!(
            !task.result_refs.is_empty() && task.active_result_ref().is_none()
        );
        if actions.iter().any(|a| matches!(a, Action::Upload { .. })) {
            r.context_view["upload_request"] = json!({"purpose":"invoice","workflow":crate::workflows::SUPPLIER_CHECK,"accept":".pdf,application/pdf,image/png,image/jpeg","max_bytes":crate::documents::MAX_FILE_BYTES});
        }
        r.context_view["single_call_mode"] = json!(single);
        r.context_view["research_paused"] = json!(paused_plan);
        r.context_view["execution_enabled"] = json!(self.executor.enabled);
        let worker_active = self.worker_active(id).await?;
        r.context_view["execution_mode"] = json!("server");
        r.context_view["worker_active"] = json!(worker_active);
        let events_expires = (Utc::now() + chrono::Duration::minutes(30)).timestamp();
        let events_signature = self.signer.events_signature(p.owner, id, p.connection, events_expires);
        r.context_view["events_path"] = json!(format!("/v2/tasks/{id}/events?owner={}&expires={events_expires}&signature={events_signature}{}", p.owner,
            p.connection.map(|c| format!("&connection={c}")).unwrap_or_default()));
        if worker_active && r.status == Status::Ready { r.status = Status::Running; }
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
        if worker_active { r.next_actions.retain(|a| !matches!(a.kind.as_str(), "execute_quoted_step" | "poll")); }
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
        if let (Some(billing), Some(plan)) = (r.billing.as_mut(), billing_plan.as_ref()) {
            billing["fee_percent"] = json!(plan.fee_bps / 100);
        }
        if p.workspace.is_some() {
            let ctx: Value = sqlx::query_scalar("SELECT public.workspace_gateway_context($1,$2,$3)").bind(p.owner).bind(p.connection).bind(p.workspace).fetch_one(&self.store.pool).await?;
            r.context_view["execution_enabled"] = json!(ctx["can_spend"] == true && self.executor.enabled);
            r.context_view["workspace"] = ctx.clone();
            if let Some(b) = &mut r.billing { b["funding_source"] = json!("workspace_balance"); b["workspace"] = ctx; }
        }
        if let Some(error) = &task.execution_error {
            r.errors.push(error.clone());
        }
        if actions.is_empty() { r.errors.extend(task.failed_steps.values().map(crate::billing::display_step_error)); }
        if let Some(value) = &task.proposal {
            let plan: Plan = serde_json::from_value(value.clone())?;
            let sources = self.catalog.sources().await.unwrap_or_default();
            let mut step_details = crate::resolver::describe_steps(&plan, &sources);
            let current = plan.steps.iter().position(|s| !task.step_finished(&s.key()));
            for (i, detail) in step_details.iter_mut().enumerate() {
                detail["subject"] = json!(task.entities.iter().find(|e| e.entity_ref == plan.steps[i].entity_ref).map(|e| &e.label));
                detail["entity_ref"] = json!(plan.steps[i].entity_ref);
                if let Some(error) = task.failed_steps.get(&plan.steps[i].key()) { detail["error"] = crate::billing::display_step_error(error); }
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
            r.context_view["cooldown"] = json!({"until":until,"message":"Waiting for the source request to finish. Saved results will appear automatically."});
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
