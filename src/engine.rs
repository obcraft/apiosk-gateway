mod compute;
mod conversation;
pub use compute::UpgradeRequest;
mod endpoint_selection;
mod requote;
mod execute;
pub(crate) use execute::{analysis_operations, answer_sources};
pub(crate) mod input;
pub use input::bind_subjects;
mod followups;
mod results;
mod selection;
mod supplier;
mod supporting_news;
mod workflows;
mod worker;
mod auto;
mod direct;
mod intake_recovery;
mod name_lookup;
pub(crate) mod guest_preview;
use crate::{
    auth::Principal,
    catalog::{Catalog, CatalogCache},
    execution::{Dispatch, Executor},
    parser::{Intent, Parser},
    protocol::*,
    resolver::{self, Action, Plan, Resolution},
    state::{digest, Entity, Fact, Signer, Task},
    store::{Claim, ClaimMeta, Store},
    telemetry::{self, Phase},
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

/// Offer a manual recomposition of saved answers without reopening source
/// execution. The same predicate governs the view and dispatch; questions
/// awaiting a follow-up keep their existing continuation instead.
fn saved_analysis_retry(task: &Task, person: bool) -> bool {
    !task.cancelled && !task.active_results.is_empty()
        && (task.workflow.is_some() || person && task.analysis.as_ref().is_some_and(|analysis|
            analysis["observations"].as_array().is_some_and(|observations|
                (observations.is_empty() && !analysis["follow_up_question"].is_string())
                    || (matches!(analysis["status"].as_str(), Some("completed" | "partial"))
                        && task.intent.is_some() && task.proposal.is_some()
                        && task.intake_recovery.is_none() && task.pending_candidates.is_empty()
                        && task.pending_dispatches.is_empty()
                        && task.actions.iter().all(|action| matches!(action["kind"].as_str(), Some("read" | "cancel" | "retry_analysis")))))))
}

/// Where a person approves a task: Ask's task view in the App. Links sent
/// before this (`/gateway-v2?task=`) still redirect there.
fn approval_url(app_url: &str, task: Uuid) -> String {
    format!("{}/?task={}", app_url.trim_end_matches('/'), task)
}

fn account_navigation(app_url: &str, workspace: Option<Uuid>) -> Value {
    let base = app_url.trim_end_matches('/');
    let scope = workspace.map(|id| format!("?workspace={id}")).unwrap_or_default();
    json!({
        "balance_url": format!("{base}/settings/billing{scope}"),
        "history_url": format!("{base}/usage/history{scope}")
    })
}

/// Leave only the conversation: the reply asks the person for something
/// before anything can be planned.
fn clear_plan(task: &mut Task) {
    task.saved_answer = false;
    task.compute = None;
    task.intent = None;
    task.proposal = None;
    task.actions.clear();
    task.completed.clear();
    task.failed_steps.clear();
    task.skipped_steps.clear();
    task.active_results.clear();
    task.analysis = None;
    task.supplier_check = None;
    task.pending_candidates.clear();
    task.original_subject = None;
    task.execution_error = None;
    task.cancelled = false;
    task.focus.goal_refs.clear();
}

/// The reply to this question replaces the model's, or starts its turn.
fn reply_to(task: &mut Task, question: &str, message: String) {
    if let Some(turn) = task.conversation.last_mut().filter(|turn| turn.question == question) {
        turn.reply = Some(message);
    } else {
        task.record_turn(question, Some(message));
    }
}

fn planning_review_reply(task:&mut Task,question:&str,message:String) {
    // A revised executable plan still has to pass deterministic field
    // coverage. Its refusal must survive the model's optimistic review text.
    if !task.actions.iter().any(|action|action.get("missing_output_contracts").is_some() || action.get("missing_input_bindings").is_some()) {
        reply_to(task,question,message);
    }
}

impl Engine {
    async fn request_claim(
        &self,
        p: &Principal,
        id: Uuid,
        hash: &str,
        kind: &'static str,
    ) -> anyhow::Result<Option<Response>> {
        self.store.check_workspace(p).await?;
        let scoped_hash = p.workspace.map(|w|digest(&(w,hash)));
        let meta = ClaimMeta { kind: Some(kind), workspace: p.workspace, connection: p.connection, trace: telemetry::current().map(|t| t.id) };
        Ok(Self::claimed_response(id, telemetry::timed(Phase::Claim, "request", self.store.claim_meta(p.owner, id, scoped_hash.as_deref().unwrap_or(hash), meta)).await?))
    }
    fn claimed_response(id: Uuid, claim: Claim) -> Option<Response> {
        match claim {
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
        }
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
            if let Some(invalid) = e.downcast_ref::<endpoint_selection::Invalid>() {
                // Refused before anything was interpreted; the person fixes the selection.
                let mut r = Response::new(id, Status::NeedsInput);
                r.errors.push(invalid.error());
                return r;
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
                "discover",
            )
            .await?
        {
            return Ok(r);
        }
        let result = self.discover_inner(p, &req, false).await;
        Ok(self.complete(p, req.request_id, result).await)
    }

    /// Recover an identical research request before applying snapshot expiry.
    /// This read never claims a new request or creates a task.
    pub(crate) async fn existing_research(&self, p: &Principal, req: &DiscoverRequest, parsed: &crate::capability_intake::ParsedRequest, search_id: &str) -> anyhow::Result<Option<Response>> {
        self.store.check_workspace(p).await?;
        let hash = digest(&("ask-research", req, parsed, search_id));
        let scoped_hash = p.workspace.map(|workspace| digest(&(workspace, &hash)));
        Ok(self.store.existing_claim(p.owner, req.request_id, scoped_hash.as_deref().unwrap_or(&hash)).await?
            .and_then(|claim| Self::claimed_response(req.request_id, claim)))
    }
    /// Continue the exact server-issued source search. The current catalog was
    /// checked before this call; its contracts and ordinary approval still own
    /// every executable step and price.
    pub(crate) async fn discover_searched(&self, p: &Principal, req: DiscoverRequest, intake: crate::capability_intake::Intake, catalog: std::sync::Arc<Catalog>, search_id: &str) -> anyhow::Result<Response> {
        anyhow::ensure!(p.is_person() && req.state.is_none() && intake.catalog_version == catalog.version, "Research requires a current App search");
        if let Some(response) = self.request_claim(p, req.request_id, &digest(&("ask-research", &req, &intake.parsed_request, search_id)), "discover").await? {
            return Ok(response);
        }
        telemetry::set_segment(telemetry::Segment::Question);
        let result = self.intake_question(p, &req, true, None, Some((&intake, catalog))).await;
        telemetry::question(&result);
        Ok(self.complete(p, req.request_id, result).await)
    }
    /// The App chat intake (`app_chat`: capability intake, endpoint
    /// selections, conversational workflows, the invoice upload prompt). No
    /// route serves it since `/v2/chat` answers 410 `ask_moved`; the database
    /// integration tests still drive the shared intake through it.
    #[doc(hidden)]
    pub async fn chat(&self, p: &Principal, req: DiscoverRequest) -> anyhow::Result<Response> {
        anyhow::ensure!(p.is_person(), "Ask chat requires a person in the App");
        if let Some(r) = self
            .request_claim(p, req.request_id, &digest(&("ask-chat", &req)), "chat")
            .await?
        {
            return Ok(r);
        }
        let result = self.discover_inner(p, &req, true).await;
        Ok(self.complete(p, req.request_id, result).await)
    }
    /// #23: refresh an expired quote from the stored intent and selection.
    /// No model call, no new turn; a new price always needs a new explicit
    /// approval.
    pub async fn requote(&self, p: &Principal, id: Uuid, req: RequoteRequest) -> anyhow::Result<Response> {
        anyhow::ensure!(p.is_person(), "Refreshing a price requires a person in the App");
        anyhow::ensure!(id == req.state.state_ref, "the request path must match the state it refreshes");
        if let Some(r) = self
            .request_claim(p, req.request_id, &digest(&("requote", id, &req)), "requote")
            .await?
        {
            return Ok(r);
        }
        let result = self.requote_inner(p, id, &req).await;
        Ok(self.complete(p, req.request_id, result).await)
    }
    async fn requote_refused(&self, p: &Principal, id: Uuid, request: Uuid, code: &str, message: &str) -> anyhow::Result<Response> {
        let mut response = self.view(p, id, request, Some(Status::StateConflict)).await?;
        response.errors.push(json!({"code": code, "message": message}));
        Ok(response)
    }
    async fn requote_inner(&self, p: &Principal, id: Uuid, req: &RequoteRequest) -> anyhow::Result<Response> {
        self.signer.verify(p.owner, &req.state, true)?;
        let (revision, mut task, connection) = self.store.load_scoped(p, id).await?;
        anyhow::ensure!(connection == p.connection, "task belongs to another connection; use explicit task recovery");
        if req.state.revision != revision {
            return self.view(p, id, req.request_id, Some(Status::StateConflict)).await;
        }
        if let Some(mut plan) = task.proposal.clone().and_then(|p|serde_json::from_value::<Plan>(p).ok()).filter(|p|p.steps.is_empty() && p.compute.is_some()) {
            anyhow::ensure!(!task.cancelled && plan.quote_ref == req.quote_ref && plan.expires_at <= Utc::now()
                && task.compute.as_ref().is_some_and(|run|run.status == "pending"), "This analysis quote cannot be refreshed");
            anyhow::ensure!(!self.store.requote_blocked(id,plan.quote_ref).await?, "Analysis payment is being checked");
            plan.quote_ref=Uuid::new_v4();plan.expires_at=Utc::now()+chrono::Duration::minutes(15);
            let run=task.compute.as_mut().unwrap();run.quote_ref=plan.quote_ref;run.action_id=Uuid::new_v4();
            task.actions=vec![json!({"kind":"compute","action_id":run.action_id})];task.proposal=Some(serde_json::to_value(plan)?);
            self.store.save(p.owner,id,revision,&task).await?;
            return self.view(p,id,req.request_id,None).await;
        }
        if task.cancelled || task.intent.is_none() || task.proposal.is_none() {
            return self.requote_refused(p, id, req.request_id, "requote_unavailable", "There is no active quote to refresh.").await;
        }
        let plan: Plan = serde_json::from_value(task.proposal.clone().unwrap())?;
        if plan.quote_ref != req.quote_ref {
            return self.requote_refused(p, id, req.request_id, "quote_mismatch", "This is not the price shown. Reload the task and try again.").await;
        }
        if plan.expires_at > Utc::now() {
            return self.requote_refused(p, id, req.request_id, "quote_still_valid", "This price has not expired yet.").await;
        }
        if self.store.requote_blocked(id, plan.quote_ref).await? {
            return self.requote_refused(p, id, req.request_id, "requote_unavailable", "An execution under this price is in progress, or was never confirmed. Recover the task before refreshing the price.").await;
        }
        anyhow::ensure!(task.compute.as_ref().is_none_or(|run|run.status == "pending"), "Read the saved analysis and payment before requesting another quote");
        let catalog = self.catalog.get().await?;
        let mut intent: Intent = serde_json::from_value(task.intent.clone().unwrap())?;
        let previous_quote_ref = plan.quote_ref;
        let changed = match requote::same_steps(&catalog, &plan) {
            Some(mut refreshed) => {
                refreshed.quote_ref = Uuid::new_v4();
                refreshed.expires_at = Utc::now() + chrono::Duration::minutes(10);
                refreshed.hash = if refreshed.compute.is_some() { digest(&(&refreshed.steps,&refreshed.compute)) } else { digest(&refreshed.steps) };
                if let Some(run) = &mut task.compute { run.quote_ref=refreshed.quote_ref;run.action_id=Uuid::new_v4(); }
                task.actions = if refreshed.steps.is_empty() { task.compute.as_ref().map(|run|vec![json!({"kind":"compute","action_id":run.action_id})]).unwrap_or_default() } else { vec![serde_json::to_value(Action::Run { action_id: Uuid::new_v4(), step: 0 })?] };
                task.proposal = Some(serde_json::to_value(&refreshed)?);
                false
            }
            None => {
                input::bind_subjects(&mut task, &catalog, &mut intent)?;
                task.intent = Some(serde_json::to_value(&intent)?);
                self.plan(&mut task, &catalog, &intent)?;
                true
            }
        };
        if changed && plan.compute.is_some() {
            if let Some(mut refreshed) = task.proposal.clone().and_then(|v|serde_json::from_value::<Plan>(v).ok()) {
                refreshed.compute=plan.compute.clone();refreshed.apply_markup(refreshed.fee_bps);
                refreshed.hash=digest(&(&refreshed.steps,&refreshed.compute));
                task.compute=Some(crate::compute::Run::new(refreshed.quote_ref,task.conversation.last().map(|t|t.question.clone()).unwrap_or_default(),refreshed.compute.as_ref().unwrap().model_id.clone()));
                task.proposal=Some(serde_json::to_value(refreshed)?);
            }
        }
        self.store.save_intent(p.owner, id, revision, &task).await?;
        let mut response = self.view(p, id, req.request_id, None).await?;
        response.context_view["requote"] = json!({"previous_quote_ref": previous_quote_ref, "changed": changed});
        Ok(response)
    }
    async fn discover_inner(
        &self,
        p: &Principal,
        req: &DiscoverRequest,
        app_chat: bool,
    ) -> anyhow::Result<Response> {
        self.intake(p, req, app_chat, None).await
    }
    /// One question from any surface. Its trace reports how it ended:
    /// priced, waiting for the person, or without a price.
    pub(crate) async fn intake(
        &self,
        p: &Principal,
        req: &DiscoverRequest,
        app_chat: bool,
        forced: Option<(crate::parser::Intent, Option<crate::workflow_catalog::WorkflowContext>)>,
    ) -> anyhow::Result<Response> {
        telemetry::set_segment(telemetry::Segment::Question);
        let result = self.intake_question(p, req, app_chat, forced, None).await;
        telemetry::question(&result);
        result
    }
    /// `forced`: a code-built intent that skips interpretation — a workflow
    /// page (with its workflow context) or a direct source call (without).
    async fn intake_question(
        &self,
        p: &Principal,
        req: &DiscoverRequest,
        app_chat: bool,
        forced: Option<(crate::parser::Intent, Option<crate::workflow_catalog::WorkflowContext>)>,
        displayed_search: Option<(&crate::capability_intake::Intake, std::sync::Arc<Catalog>)>,
    ) -> anyhow::Result<Response> {
        // A direct call names its source and operation; it is priced as asked
        // or refused, never re-planned by the model.
        let direct = forced.as_ref().is_some_and(|(_, workflow)| workflow.is_none());
        let has_displayed_search = displayed_search.is_some();
        let mut workflow = forced.as_ref().and_then(|(_, workflow)| workflow.clone());
        let mut forced = forced.map(|(intent, _)| intent);
        let started = std::time::Instant::now();
        anyhow::ensure!(limits::question_ok(&req.question), "invalid question length");
        let catalog = match &displayed_search { Some((_, catalog)) => catalog.clone(), None => self.catalog.get().await? };
        // A selection is checked before any task exists: a refusal creates nothing.
        let selection = req.context_delta.selection.as_ref()
            .map(|selection| endpoint_selection::accept(selection, app_chat, req.context_delta.workflow_slug.is_some(), &catalog))
            .transpose()?;
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
        // Best-effort: a lost response can still be recovered by task id even
        // when `finish` never runs (#11). Never blocks the answer.
        if self.store.link_request_task(p.owner, req.request_id, id).await.is_err() {
            tracing::warn!("request receipt task link failed");
        }
        let (revision, mut task, connection) = self.store.load_scoped(&p, id).await?;
        anyhow::ensure!(task.external_purchase.is_none(), "External source purchases keep their own saved quote. Start a new question to choose another source.");
        anyhow::ensure!(
            connection == p.connection,
            "task belongs to another connection; use explicit task recovery"
        );
        if req.state.as_ref().is_some_and(|s| s.revision != revision) {
            return self
                .view(p, id, req.request_id, Some(Status::StateConflict))
                .await;
        }
        let in_flight:bool=telemetry::timed_db("inflight_check", sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM executions e JOIN tasks t ON t.id=e.task_id WHERE e.task_id=$1 AND (e.status IN ('dispatching','unresolved') OR (e.status='succeeded' AND NOT coalesce((t.document->'settled_compute') ? e.id::text, false) AND NOT coalesce(e.id::text=t.document #>> '{compute,action_id}' AND t.document #>> '{compute,status}' IN ('completed','unavailable'),false) AND NOT EXISTS(SELECT 1 FROM results r WHERE r.execution_id=e.id AND (t.document->'result_refs') ? r.id::text))))").bind(id).fetch_one(&self.store.pool)).await?;
        anyhow::ensure!(
            !in_flight,
            "existing execution must finish or reconcile before a new question"
        );
        self.prepare_conversation(p.owner, id, &mut task).await?;
        // The live Ask interpreter distinguishes saved evidence from retrieval
        // before capability search. A new topic retains the chat history only.
        let new_topic = if req.state.is_some() && !task.conversation.is_empty()
            && forced.is_none() && selection.is_none() && !has_displayed_search
            && self.parser.ask_v2().is_some() {
            anyhow::ensure!(!self.worker_active(id).await?, "Wait for the current answer to finish");
            let (new_topic, response) = self.route_conversation(p,req,id,revision,&mut task).await?;
            if let Some(response) = response { return Ok(response); }
            if new_topic { task.workflow = None; task.capability_intake = None; }
            new_topic
        } else { false };
        let recovery = task.intake_recovery.take();
        let resuming = recovery.as_ref().is_some_and(|saved| saved.matches(req));
        if resuming && task.conversation.last().is_some_and(|turn| turn.question == req.question && turn.output.is_none()) {
            task.conversation.pop();
        }
        let pending_workflow = task.workflow.clone().filter(|workflow| recovery.is_some() || workflow.collecting || (task.proposal.is_some() && task.completed.is_empty() && task.active_results.is_empty() && task.analysis.is_none()));
        task.workflow = workflow.clone().or_else(|| (app_chat && !direct).then(|| pending_workflow.clone()).flatten());
        for focus in &req.context_delta.requested_focus_refs {
            anyhow::ensure!(
                task.entities.iter().any(|e| e.entity_ref == *focus),
                "unknown focus entity"
            );
        }
        if !req.context_delta.requested_focus_refs.is_empty() {
            task.focus.entity_refs = req.context_delta.requested_focus_refs.clone()
        }
        for f in &req.context_delta.explicit_values {
            self.user_fact(
                &mut task,
                &catalog,
                f.entity_ref,
                &f.semantic_type,
                f.value.clone(),
            )?;
        }
        // A connected follow-up may continue what the last turn found; the
        // intake itself is rebuilt below, as for every non-App question.
        let earlier_intake = if app_chat || new_topic { None } else { task.capability_intake.clone() };
        // All Ask submissions, including follow-ups and workflow input, cross
        // this boundary before source selection or planning. Direct API calls
        // already name an exact operation and keep their separate contract,
        // and a workflow page's recipe is its plan: a code-built intent is
        // never interpreted, so it is not parsed either (a parse outage must
        // not stop a workflow that needs no model before its quote).
        task.capability_intake = if let Some((intake, _)) = displayed_search {
            // A later English rendering or a different server must not replace
            // the candidate set the person actually reviewed.
            Some(intake.clone())
        } else if app_chat && forced.is_none() {
            let model_selection = selection.as_ref().map(|selection| selection.model_selection(&catalog));
            let reusable = resuming && recovery.as_ref().zip(task.capability_intake.as_ref())
                .is_some_and(|(saved, intake)| saved.reusable(intake, &catalog.version));
            if reusable {
                task.capability_intake.take()
            } else {
                task.capability_intake = None;
                match self.parser.prepare_ask(&req.question, model_selection.as_ref(), &crate::parser::conversation_route::interpretation_context(&task,new_topic), &catalog).await {
                    Ok(intake) => Some(intake),
                    Err(error) => return self.pause_intake(p, id, revision, req, &mut task, crate::state::IntakeStage::Parsing, false, error).await,
                }
            }
        } else { None };
        let selected_workflow = req.context_delta.workflow_slug.as_deref();
        let recovering_workflow = selected_workflow.is_some() && recovery.as_ref()
            .is_some_and(|saved| saved.context_delta.workflow_slug.as_deref() == selected_workflow);
        anyhow::ensure!(selected_workflow.is_none() || (app_chat && (req.state.is_none() || recovering_workflow)), "Start a workflow in a new App chat.");
        let conversational_workflow = forced.is_none() && app_chat && (selected_workflow.is_some() || pending_workflow.is_some());
        if conversational_workflow && selection.is_some() {
            return Err(endpoint_selection::Invalid::workflow().into());
        }
        if conversational_workflow {
            task.workflow = match selected_workflow {
                Some(slug) => {
                    let recipe = crate::workflow_catalog::recipe(slug).ok_or_else(|| anyhow::anyhow!("Unknown workflow."))?;
                    pending_workflow.clone().filter(|saved| recovering_workflow && saved.slug == slug)
                        .or_else(|| Some(crate::workflow_catalog::WorkflowContext { collecting: true, slug: recipe.slug, title: recipe.name, input: Default::default(), limitations: recipe.limitations }))
                }
                None => pending_workflow,
            };
            self.archive_conversation_output(p, id, req.request_id, &mut task).await?;
            forced = match self.collect_workflow(&req.question, &mut task, &catalog).await {
                Ok(intent) => intent,
                Err(error) => return self.pause_intake(p, id, revision, req, &mut task, crate::state::IntakeStage::Planning, false, error).await,
            };
            workflow = task.workflow.clone();
            if forced.is_none() {
                self.store.save_intent(p.owner, id, revision, &task).await?;
                return self.view(p, id, req.request_id, Some(Status::NeedsInput)).await;
            }
        }
        // Deterministic shortcuts read the question alone; a selection already
        // says which endpoints may answer it.
        let shortcuts = forced.is_none() && selection.is_none() && !has_displayed_search;
        if shortcuts {
            if let Some(company) = crate::parser::broad_filing_subject(&req.question) {
                // This request has no form or topic. The connected filing
                // operations cannot return a complete filing index, and a
                // guessed full-text search would create a paid empty result.
                let gap = json!({"stage":"before_quote","missing":"filing scope: complete list, form, reported figures, or disclosure topic",
                    "known_subject":company,"available_operations":"FilingPulse can search disclosure text or retrieve quarterly figures; the connected operations do not list every filing. Companies House covers UK companies."});
                let message = self.parser.ask_for_missing_context(&req.question, &task, &gap).await
                    .unwrap_or_else(|_| format!("Are you looking for a complete list of {company} filings, a particular form, or disclosures on a topic?"));
                self.archive_conversation_output(p, id, req.request_id, &mut task).await?;
                task.record_turn(&req.question, Some(message));
                clear_plan(&mut task);
                self.store.save_intent(p.owner, id, revision, &task).await?;
                return self.view(p, id, req.request_id, Some(Status::NeedsInput)).await;
            }
        }
        let mut chat_intent = None;
        // Invoice checks must not depend on the model recognising the wording.
        let invoice_fallback = if shortcuts { crate::supplier_check::fallback_intent(&req.question, &catalog) } else { None };
        // A plain invoice check needs no interpretation: skip the model call.
        let forced = forced.or_else(|| shortcuts.then(|| crate::supplier_check::direct_intent(&req.question, &catalog)).flatten());
        if app_chat && shortcuts && forced.is_none() && crate::supplier_check::needs_invoice_upload(&req.question, &catalog) {
            // Deterministic: ask for the invoice in the conversation, no model call.
            self.archive_conversation_output(p, id, req.request_id, &mut task).await?;
            task.record_turn(&req.question, Some(crate::supplier_check::upload_message(&req.question)));
            clear_plan(&mut task);
            task.actions = vec![serde_json::to_value(Action::Upload { action_id: Uuid::new_v4(), purpose: "invoice".into() })?];
            self.store.save_intent(p.owner, id, revision, &task).await?;
            return self.view(p, id, req.request_id, Some(Status::NeedsInput)).await;
        }
        if app_chat && forced.is_some() && !conversational_workflow {
            self.archive_conversation_output(p, id, req.request_id, &mut task).await?;
            task.record_turn(&req.question, Some(if workflow.is_some() {
                "I'll prepare the source checks and show the total price before you approve.".into()
            } else { crate::supplier_check::routing_message(&req.question) }));
        }
        // Ask interprets execution inputs against its discovered operations.
        // Connected-agent/API requests retain their existing interpreter.
        // Both feed the same resolver, quote, authorization and execution path.
        let interpreted = forced.is_none();
        if interpreted {
            let parsing_task = crate::parser::conversation_route::interpretation_context(&task,new_topic);
            let model_selection = selection.as_ref().map(|selection| selection.model_selection(&catalog));
            let reply = if let Some(intake) = &task.capability_intake {
                self.parser.chat_from_capabilities(&req.question, model_selection.as_ref(), &parsing_task, &catalog, intake).await
            } else if app_chat || self.parser.ask_v2().is_none() {
                // Without an Ask v2 model (local fixtures) the full-catalog
                // interpreter remains the only reader.
                self.parser.chat_selected(&req.question, model_selection.as_ref(), &parsing_task, &catalog).await
            } else {
                // Connected agents plan from the Ask page's parse and source
                // search, never from a model pass over the whole catalog.
                match self.parser.ask_intake(&req.question, &parsing_task, &catalog, earlier_intake.as_ref()).await {
                    Ok(intake) => {
                        let reply = self.parser.chat_from_capabilities(&req.question, model_selection.as_ref(), &parsing_task, &catalog, &intake).await;
                        task.capability_intake = Some(intake);
                        reply
                    }
                    Err(error) => Err(error),
                }
            };
            let mut reply = match (reply, &invoice_fallback) {
                (Ok(reply), _) => reply,
                (Err(_), Some(fallback)) => crate::parser::ChatReply { message: crate::supplier_check::routing_message(&req.question), discover: true, intent: Some(fallback.clone()), decline_reason: None },
                (Err(error), None) if app_chat => return self.pause_intake(p, id, revision, req, &mut task, crate::state::IntakeStage::Planning, false, error).await,
                (Err(error), None) => return Err(error),
            };
            if let Some(fallback) = invoice_fallback.as_ref().filter(|_| !reply.discover || reply.intent.is_none()) {
                reply = crate::parser::ChatReply { message: crate::supplier_check::routing_message(&req.question), discover: true, intent: Some(fallback.clone()), decline_reason: None };
            }
            // A genuinely declined question (small talk, an unclear request, a
            // requirement no source covers) is answered by the conversation
            // model; on any failure the planner's own reply stands. The status
            // and actions are the planner's either way.
            if let Some(written) = self.parser.decline_reply(&req.question, &parsing_task, &catalog, &reply).await {
                reply.message = written;
            }
            chat_intent = reply.intent;
            self.archive_conversation_output(p, id, req.request_id, &mut task).await?;
            // The written reply belongs to the App conversation, and drives its
            // written answer. Elsewhere a reply is kept only when it is a
            // clarification; a plan runs exactly as its intent asks.
            task.record_turn(&req.question, (app_chat || !reply.discover).then_some(reply.message));
            if !reply.discover {
                clear_plan(&mut task);
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
        if let Some(Err(reason)) = selection.as_ref().map(|selection| selection.bind(&mut intent, &catalog)) {
            reply_to(&mut task, &req.question, reason);
            clear_plan(&mut task);
            self.store.save_intent(p.owner, id, revision, &task).await?;
            return self.view(p, id, req.request_id, Some(Status::NeedsInput)).await;
        }
        if workflow.is_none() && selection.is_none() && crate::supplier_check::route(&req.question, &mut intent, &catalog) {
            tracing::info!("invoice question routed to the supplier payment check");
        }
        if !app_chat && !interpreted {
            self.archive_conversation_output(p, id, req.request_id, &mut task).await?;
            task.record_turn(&req.question, None);
        }
        crate::uk_supplier::expand(&mut intent, &catalog);
        input::bind_subjects(&mut task, &catalog, &mut intent)?;
        task.supplier_check = None;
        if let Some(expansion) = crate::supplier_check::expand(&req.question, &mut intent, &catalog) {
            for (kind, value) in &expansion.facts {
                self.user_fact(&mut task, &catalog, expansion.invoice.entity_ref, kind, value.clone())?;
            }
            task.supplier_check = Some(serde_json::to_value(&expansion.invoice)?);
        }
        // A company the person named is looked up by that name before they
        // are asked for its identifier. Read questions only: a direct call or
        // workflow intent is priced exactly as it was asked.
        if interpreted {
            name_lookup::seed(&mut task, &catalog, &mut intent)?;
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
            task.skipped_steps.clear();
        task.active_results.clear();
        task.analysis = None;
        task.compute = None;
        task.saved_answer = false;
        task.pending_candidates.clear();
        task.original_subject = None;
        task.cancelled = false;
        task.execution_error = None;
        let planning = telemetry::span(Phase::Plan).task(id);
        let mut status = self.plan(&mut task, &catalog, &intent)?;
        // A selected endpoint is the person's own choice: the model reviews
        // neither a direct call nor a selection, and their refusal stands.
        if let (false, Resolution::Unsupported(reason)) = (direct || selection.is_some(), resolver::resolve(&catalog, &task, &intent)) {
            let review = match self.parser.review_plan(&req.question, &task, &catalog, &intent, &reason).await {
                Ok(review) => review,
                Err(error) if app_chat => return self.pause_intake(p, id, revision, req, &mut task, crate::state::IntakeStage::Planning, true, error).await,
                Err(error) => return Err(error),
            };
            if let Some(revised) = review.intent {
                task.intent = Some(serde_json::to_value(&revised)?);
                status = self.plan(&mut task, &catalog, &revised)?;
            } else {
                task.proposal = None;
                task.actions = vec![json!({"kind":"planning_clarification","message":review.message})];
                status = Status::NeedsInput;
            }
            planning_review_reply(&mut task, &req.question, review.message);
        }
        planning.finish(telemetry::Outcome::Ok);
        telemetry::set_query_class(telemetry::classify(&task));
        self.store.save_intent(p.owner, id, revision, &task).await?;
        let response = self.view(p, id, req.request_id, Some(status)).await?;
        tracing::info!(trace = telemetry::id(), task = %id, quote = response.proposal.as_ref().map(|p| p.quote_ref.to_string()).unwrap_or_default(),
            stage = "question_to_price", stage_ms = started.elapsed().as_millis() as u64,
            price_ready = response.proposal.is_some(), "request stage completed");
        Ok(response)
    }
    fn user_fact(
        &self,
        task: &mut Task,
        catalog: &Catalog,
        entity: Uuid,
        kind: &str,
        value: Value,
    ) -> anyhow::Result<()> {
        input::user_fact(task, catalog, entity, kind, value)
    }
    fn plan(&self, task: &mut Task, catalog: &Catalog, intent: &Intent) -> anyhow::Result<Status> {
        task.actions.clear();
        task.proposal = None;
        match resolver::resolve(catalog, task, intent) {
            Resolution::Plan(mut plan) => {
                let missing_inputs=crate::parser::plan_coverage::uncovered_inputs(task,catalog,intent,&plan);
                if !missing_inputs.is_empty() {
                    let fields=missing_inputs.join("; ").replace('_'," ");
                    let message=if task.capability_intake.as_ref().is_some_and(|intake|intake.parsed_request.language=="nl") {
                        format!("Het plan behoudt deze opgegeven gegevens of filters nog niet aantoonbaar: {fields}. Er is geen prijsvoorstel gemaakt en niets uitgevoerd. Verduidelijk de gewenste selectie of probeer de planning opnieuw.")
                    } else {
                        format!("The plan does not yet confirm that these supplied inputs or filters are preserved: {fields}. No quote was created and nothing was executed. Clarify the requested selection or try planning again.")
                    };
                    if let Some(turn)=task.conversation.last_mut() {turn.reply=Some(message.clone());}
                    task.actions.push(json!({"kind":"planning_clarification","message":message,"missing_input_bindings":missing_inputs}));
                    return Ok(Status::NeedsInput);
                }
                let missing=crate::parser::plan_coverage::uncovered_outputs(task,catalog,intent,&plan);
                if !missing.is_empty() {
                    let fields=missing.join("; ").replace('_'," ");
                    let message=if task.capability_intake.as_ref().is_some_and(|intake|intake.parsed_request.language=="nl") {
                        format!("De geselecteerde bronnen dekken deze gevraagde gegevens nog niet aantoonbaar: {fields}. Er is geen prijsvoorstel gemaakt en niets uitgevoerd. Verduidelijk de gewenste gegevens of beperk de vraag tot bevestigde velden.")
                    } else {
                        format!("The selected sources do not confirm coverage for these requested details: {fields}. No quote was created and nothing was executed. Clarify the required data or limit the request to confirmed fields.")
                    };
                    if let Some(turn)=task.conversation.last_mut() {turn.reply=Some(message.clone());}
                    task.actions.push(json!({"kind":"planning_clarification","message":message,"missing_output_contracts":missing}));
                    return Ok(Status::NeedsInput);
                }
                if task.supplier_check.is_some() || task.workflow.is_some() {
                    // Workflow runs carry the workflow markup on every step.
                    plan.apply_markup(catalog_core::fees::WORKFLOW_MARKUP_BPS as i64);
                    plan.workflow_slug = Some(task.workflow.as_ref().map(|w|w.slug.clone()).unwrap_or_else(||crate::workflows::SUPPLIER_CHECK.into()));
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
        telemetry::timed(Phase::View, "task", self.view_with_results(p, id, request, forced, &mut Default::default())).await
    }

    /// This cache belongs to one authorized task stream. Saved source documents
    /// are immutable; task access and expiry are still checked on every view.
    pub(crate) async fn view_with_results(
        &self,
        p: &Principal,
        id: Uuid,
        request: Uuid,
        forced: Option<Status>,
        documents: &mut std::collections::BTreeMap<Uuid, Value>,
    ) -> anyhow::Result<Response> {
        let started = std::time::Instant::now();
        let (revision, mut task, connection) = self.store.load_scoped(&p, id).await?;
        if !p.is_person() && connection != p.connection {
            return Err(crate::store::StoreAccessError::TaskUnavailable.into());
        }
        self.prepare_conversation(p.owner, id, &mut task).await?;
        if let Some(plan) = task.proposal.as_ref().map(|plan| serde_json::from_value::<Plan>(plan.clone())).transpose()? {
            if selection::pending_choice(&mut task, &plan).is_none() {
                task.actions.retain(|action| action["kind"] != "choose");
            }
        }
        let mut actions: Vec<Action> = task
            .actions
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        let receipts = if task.proposal.is_some() {
            self.executor.recoverable_receipts(p.owner, id).await?
        } else { Vec::new() };
        if !receipts.is_empty() {
            actions.retain(|action| !matches!(action, Action::Run { .. } | Action::Analyze { .. }));
            for execution_id in receipts {
                if !actions.iter().any(|action| matches!(action, Action::Poll { execution_id: existing, .. } if *existing == execution_id)) {
                    actions.push(Action::Poll { action_id: execution_id, execution_id });
                }
            }
        }
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
        let inferred = if task.intake_recovery.is_some() || (task.execution_error.is_some() && task.result_refs.is_empty()) {
            Status::Failed
        } else if task.cancelled {
            Status::Cancelled
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
        } else if actions.iter().any(|a| matches!(a, Action::Run { .. } | Action::Compute { .. })) {
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
        r.context_view["navigation"] = account_navigation(&self.app_url, p.workspace);
        // Read each referenced source document once for this response. Reuse
        // that snapshot for current results, history, pagination and PDF hashes.
        let mut result_ids = task.active_results.clone();
        result_ids.extend(task.result_refs.last().copied());
        for output in task.conversation.iter().filter_map(|turn| turn.output.as_ref()) {
            result_ids.extend(output.result_ref);
            result_ids.extend(&output.result_refs);
        }
        documents.retain(|id, _| result_ids.contains(id));
        let missing: Vec<_> = result_ids.iter().copied().filter(|id| !documents.contains_key(id)).collect();
        let fetched = self.store.results(p.owner, id, &missing).await?;
        let fetched_count = fetched.len();
        documents.extend(fetched);
        let sources = if documents.is_empty() { Default::default() } else { self.catalog.sources().await.unwrap_or_default() };
        let pages = documents.iter().map(|(result, document)| {
            Ok((*result, self.project_result_page(p.owner, id, *result, 0, document.clone(), &sources)?))
        }).collect::<anyhow::Result<std::collections::BTreeMap<_, _>>>()?;
        let page = |result: Uuid| -> anyhow::Result<Value> {
            pages.get(&result).cloned().ok_or_else(|| sqlx::Error::RowNotFound.into())
        };
        // Hydrate saved turn results through the same owner-scoped reader as
        // the current result, including freshly signed report download links.
        for (index, turn) in task.conversation.iter().enumerate() {
            if let Some(result_ref) = turn.output.as_ref().and_then(|output| output.result_ref) {
                r.context_view["conversation"][index]["output"]["result"] =
                    page(result_ref)?;
            }
            if let Some(output) = &turn.output {
                let mut results = Vec::new();
                for result in &output.result_refs { results.push(page(*result)?); }
                r.context_view["conversation"][index]["output"]["results"] = json!(results);
                if let Some(quote)=output.proposal.as_ref().and_then(|p|p["quote_ref"].as_str()).and_then(|s|s.parse().ok()) {
                    if let Ok(report)=self.research_report_link_from_results(p.owner,id,&task,quote,&documents) {
                        r.context_view["conversation"][index]["output"]["report"]=report;
                    }
                }
            }
        }
        let mut results = Vec::new();
        for result in &task.active_results { results.push(page(*result)?); }
        r.context_view["results"] = json!(results);
        if let Some(quote)=task.proposal.as_ref().and_then(|p|p["quote_ref"].as_str()).and_then(|s|s.parse().ok()) {
            if let Ok(report)=self.research_report_link_from_results(p.owner,id,&task,quote,&documents) {r.context_view["report"]=report;}
        }
        r.context_view["result_is_historical"] = json!(
            !task.result_refs.is_empty() && task.active_result_ref().is_none()
        );
        if actions.iter().any(|a| matches!(a, Action::Upload { .. })) {
            r.context_view["upload_request"] = json!({"purpose":"invoice","workflow":crate::workflows::SUPPLIER_CHECK,"accept":".pdf,application/pdf,image/png,image/jpeg","max_bytes":crate::invoice_document::MAX_FILE_BYTES});
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
        if task.intent.as_ref().is_some_and(|intent| intent["goals"].as_array().is_some_and(|goals|
            goals.iter().any(|g| g["capability"] == crate::uk_supplier::screening_cap(0)))) {
            r.context_view["coverage_notices"] = crate::uk_supplier::limitations();
        }
        // A connection with a spending mandate approves in the chat card. Its
        // automatic-approval rule, when it has one, is reported separately in
        // `auto_approval` (below), so a card that predates it keeps working.
        let budget = self.executor.connection_budget(p).await.unwrap_or(None);
        let auto_policy = self.executor.auto_policy(p).await.unwrap_or_else(|error| {
            tracing::warn!(task = %id, error = %error, "automatic approval rule unavailable");
            None
        });
        r.context_view["approval_mode"] = json!(if budget.is_some() { "chatbot" } else { "app" });
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
            r.context_view["candidates"] = json!(task.pending_candidates);
            r.context_view["candidates_note"] = json!(task.candidate_note);
        }
        r.next_actions = actions.iter().map(Action::public).collect();
        if worker_active { r.next_actions.retain(|a| !matches!(a.kind.as_str(), "execute_quoted_step" | "poll")); }
        if !worker_active {
            for pending in &task.pending_dispatches {
                r.next_actions.retain(|action| action.action_id != pending.action_id);
                r.next_actions.push(Action::Poll { action_id: pending.action_id, execution_id: pending.action_id }.public());
            }
            if saved_analysis_retry(&task, p.is_person())
                && !r.next_actions.iter().any(|action| action.kind == "retry_analysis") {
                if let Some(quote) = task.proposal.as_ref().and_then(|plan| plan["quote_ref"].as_str()).and_then(|id| id.parse().ok()) {
                    r.next_actions.push(Action::RetryAnalysis { action_id: quote }.public());
                }
            }
        }
        if !worker_active {
            if let Some(receipt) = self.compute_receipt(p,id,&task).await? {
                r.next_actions.retain(|a|a.action_id != receipt);
                let mut action=Action::Compute{action_id:receipt}.public();
                action.kind="poll".into();action.label="Read saved analysis receipt".into();action.requires_authorization=false;
                r.next_actions.push(action);
            }
        }
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
        r.context_view["analysis_upgrade"] = self.compute_view(p, id, &task, billing_plan.as_ref(), &r.billing).await?;
        if task.compute.as_ref().is_some_and(|run|run.status == "running" || run.status == "preparing") && actions.iter().any(|a|matches!(a,Action::Compute{..})) { r.status = Status::Running; }
        if let Some(policy) = auto_policy.as_ref() {
            let now = Utc::now();
            let authorized = r.billing.as_ref().is_some_and(|b| b["authorization_active"] == true);
            let applied = authorized && r.billing.as_ref().is_some_and(|b| b["approved_via"] == "connection_auto");
            // Why the current plan is waiting, when it is: the same decision
            // the gateway makes, read without a lock and without writing.
            let denial = match billing_plan.as_ref() {
                Some(plan) if !authorized && !task.cancelled => crate::auto_approval::decide(&crate::auto_approval::Quote {
                    cap_atomic: plan.cap_atomic,
                    expires_at: plan.expires_at,
                    unverified_source: plan.steps.iter().any(|step| step.service.unverified),
                }, Some(policy), budget.as_ref(), now).err(),
                _ => None,
            };
            r.context_view["auto_approval"] = crate::auto_approval::context_view(policy, now, applied, denial);
        }
        if let (Some(billing), Some(plan)) = (r.billing.as_mut(), billing_plan.as_ref()) {
            billing["fee_percent"] = json!(plan.fee_bps / 100);
            billing["apiosk_fee_bps"] = json!(plan.fee_bps);
            billing["pricing_rule"] = json!(if plan.fee_bps == 2000 { "provider_cost_plus_20_v1" } else { "provider_cost_plus_15_v1" });
        }
        if p.workspace.is_some() {
            let ctx: Value = telemetry::timed_db("workspace_ctx", sqlx::query_scalar("SELECT public.workspace_gateway_context($1,$2,$3)").bind(p.owner).bind(p.connection).bind(p.workspace).fetch_one(&self.store.pool)).await?;
            r.context_view["execution_enabled"] = json!(ctx["can_spend"] == true && self.executor.enabled);
            r.context_view["workspace"] = ctx.clone();
            if let Some(b) = &mut r.billing { b["funding_source"] = json!("workspace_balance"); b["workspace"] = ctx; }
        }
        if let Some(error) = &task.execution_error {
            r.errors.push(error.clone());
        }
        if actions.is_empty() { r.errors.extend(task.failed_steps.values().map(crate::billing::display_step_error)); }
        if let Some(value) = task.proposal.as_ref().filter(|_| !task.saved_answer) {
            let plan: Plan = serde_json::from_value(value.clone())?;
            let sources = self.catalog.sources().await.unwrap_or_default();
            let mut step_details = crate::resolver::describe_steps(&plan, &sources);
            let current = plan.steps.iter().position(|s| !task.step_finished(&s.key()));
            for (i, detail) in step_details.iter_mut().enumerate() {
                detail["subject"] = json!(task.entities.iter().find(|e| e.entity_ref == plan.steps[i].entity_ref).map(|e| &e.label));
                detail["entity_ref"] = json!(plan.steps[i].entity_ref);
                if let Some(error) = task.failed_steps.get(&plan.steps[i].key()) { detail["error"] = crate::billing::display_step_error(error); }
                detail["status"] = json!(if task.failed_steps.contains_key(&plan.steps[i].key()) || (current == Some(i) && task.execution_error.is_some()) { "failed" }
                    else if task.skipped_steps.contains(&plan.steps[i].key()) { "skipped" }
                    else if task.completed.contains(&plan.steps[i].key()) { "completed" }
                    else if current == Some(i) && actions.iter().any(|a|matches!(a, Action::Poll {..})) { "running" }
                    else if task.cancelled { "cancelled" } else if paused_plan { "paused" } else { "pending" });
            }
            if let Some(model) = plan.compute.as_ref().and_then(crate::compute::Quote::model) {
                step_details.push(json!({"kind":"compute","number":plan.steps.len()+1,"title":"Extended analysis","source":{"name":model.name,"directory_name":format!("x402engine · {}", model.name),"slug":"analysis-compute","url":"https://x402engine.app"},"dependencies":[],"max_total_atomic":model.buyer_cap().to_string(),"currency":"USD","status":task.compute.as_ref().map(|run|run.status.as_str()).unwrap_or("pending")}));
            }
            r.proposal = Some(Proposal {
                label: if plan.steps.is_empty() && plan.compute.is_some() { "Extended analysis of saved results".into() } else { "Data request".into() },
                step_details,
                quote_ref: plan.quote_ref,
                price_status: "capped".into(),
                currency: "USD".into(),
                max_total_atomic: plan.cap_atomic.to_string(),
                expires_at: plan.expires_at,
                approval_url: approval_url(&self.app_url, id),
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
            r.result = Some(page(*result_ref)?);
        }
        if let Some(workflow)=&task.workflow {
            r.context_view["workflow"]=json!({"slug":workflow.slug,"title":workflow.title,"limitations":workflow.limitations});
            // Best-effort: a run row is a secondary index over the task, so a
            // failure here must never fail the view itself.
            if let Err(error) = self.record_named_workflow_run(p,id,revision,&task,&r).await {
                tracing::warn!(error=%error, task=%id, "workflow run registration failed");
            }
        }
        r.state = Some(self.signer.issue(p.owner, id, revision, task.focus));
        tracing::info!(trace = telemetry::id(), task = %id, view_ms = started.elapsed().as_millis() as u64,
            result_documents = documents.len(), fetched_documents = fetched_count, "task view completed");
        Ok(r)
    }
}

#[cfg(test)]
mod saved_analysis_retry_tests {
    use super::*;

    fn saved(status: &str) -> Task {
        let mut task = Task::default();
        task.active_results.push(Uuid::new_v4());
        task.intent = Some(json!({"goals":[]}));
        task.proposal = Some(json!({"quote_ref":Uuid::new_v4()}));
        task.analysis = Some(json!({"status":status,"observations":[{"text":"Previously verified finding","evidence":[]}],"limitations":[]}));
        task
    }

    #[test]
    fn saved_completed_and_partial_answers_offer_person_only_manual_recomposition() {
        for status in ["completed", "partial"] {
            let task = saved(status);
            let before = serde_json::to_value(&task).unwrap();
            assert!(saved_analysis_retry(&task, true));
            assert!(!saved_analysis_retry(&task, false));
            assert_eq!(serde_json::to_value(&task).unwrap(), before, "eligibility never starts work or changes the answer");
            assert!(task.actions.is_empty(), "view reconstructs a manual action; no execution action is scheduled");
        }
    }

    #[test]
    fn saved_answer_retry_does_not_accept_unfinished_or_missing_current_state() {
        for mutation in ["cancelled", "no_results", "no_intent", "no_plan", "pending_candidates", "pending_dispatch", "pending_action", "no_analysis", "unknown_status", "invalid_observations"] {
            let mut task = saved("partial");
            match mutation {
                "cancelled" => task.cancelled = true,
                "no_results" => task.active_results.clear(),
                "no_intent" => task.intent = None,
                "no_plan" => task.proposal = None,
                "pending_candidates" => task.pending_candidates.push(json!({"entity_ref":Uuid::new_v4()})),
                "pending_dispatch" => task.pending_dispatches.push(crate::state::PendingDispatch {step:0,action_id:Uuid::new_v4(),inputs:json!({})}),
                "pending_action" => task.actions.push(json!({"kind":"supply","action_id":Uuid::new_v4()})),
                "no_analysis" => task.analysis = None,
                "unknown_status" => task.analysis.as_mut().unwrap()["status"] = json!("running"),
                "invalid_observations" => task.analysis.as_mut().unwrap()["observations"] = Value::Null,
                _ => unreachable!(),
            }
            assert!(!saved_analysis_retry(&task, true), "{mutation}");
        }
    }

    #[test]
    fn old_answer_clarification_does_not_block_manual_recomposition_of_saved_sources() {
        let mut task = saved("partial");
        task.analysis.as_mut().unwrap()["follow_up_question"] = json!("What else should I check?");
        assert!(saved_analysis_retry(&task, true));
        task.analysis.as_mut().unwrap()["status"] = json!("unavailable");
        task.analysis.as_mut().unwrap()["observations"] = json!([]);
        assert!(!saved_analysis_retry(&task, true), "legacy unanswered clarification keeps its scope");
    }

    #[test]
    fn legacy_empty_answers_and_workflow_retry_keep_their_existing_scope() {
        let mut task = saved("unavailable");
        assert!(!saved_analysis_retry(&task, true));
        task.analysis.as_mut().unwrap()["observations"] = json!([]);
        assert!(saved_analysis_retry(&task, true));
        assert!(!saved_analysis_retry(&task, false));
        task.analysis = None;
        task.compute = None;
        task.workflow = Some(serde_json::from_value(json!({"slug":"fixture","title":"Fixture","input":{},"limitations":[]})).unwrap());
        assert!(saved_analysis_retry(&task, false));
        task.cancelled = true;
        assert!(!saved_analysis_retry(&task, true));
    }
}

#[cfg(test)]
mod account_navigation_tests {
    use super::{account_navigation, approval_url};

    #[test]
    fn revised_plan_keeps_deterministic_output_refusal_instead_of_model_success() {
        let mut task=crate::state::Task::default();
        task.record_turn("Give title and date",Some("Source does not declare publication date".into()));
        task.actions=vec![serde_json::json!({"kind":"planning_clarification","missing_output_contracts":["publication_dates"]})];
        super::planning_review_reply(&mut task,"Give title and date","Complete lookup is ready for approval".into());
        assert_eq!(task.conversation.last().unwrap().reply.as_deref(),Some("Source does not declare publication date"));
        assert!(task.proposal.is_none());
        assert_eq!(task.actions[0]["kind"],"planning_clarification");
        task.actions=vec![serde_json::json!({"kind":"planning_clarification","missing_input_bindings":["location"]})];
        super::planning_review_reply(&mut task,"Give title and date","Complete lookup is ready for approval".into());
        assert_eq!(task.conversation.last().unwrap().reply.as_deref(),Some("Source does not declare publication date"));
        task.actions.clear();
        super::planning_review_reply(&mut task,"Give title and date","Revised plan is ready".into());
        assert_eq!(task.conversation.last().unwrap().reply.as_deref(),Some("Revised plan is ready"));
    }

    #[test]
    fn approval_links_open_the_task_in_ask() {
        let task = uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000456").unwrap();
        for app in ["https://app.apiosk.com", "https://app.apiosk.com/"] {
            assert_eq!(approval_url(app, task), "https://app.apiosk.com/?task=00000000-0000-4000-8000-000000000456");
        }
        assert!(!approval_url("https://app.apiosk.com", task).contains("gateway-v2"));
    }

    #[test]
    fn account_shortcuts_are_absolute_and_stable() {
        let links = account_navigation("https://app.apiosk.com/", None);
        assert_eq!(links["balance_url"], "https://app.apiosk.com/settings/billing");
        assert_eq!(links["history_url"], "https://app.apiosk.com/usage/history");

        let workspace = uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000123").unwrap();
        let links = account_navigation("https://app.apiosk.com", Some(workspace));
        assert_eq!(links["balance_url"], "https://app.apiosk.com/settings/billing?workspace=00000000-0000-4000-8000-000000000123");
        assert_eq!(links["history_url"], "https://app.apiosk.com/usage/history?workspace=00000000-0000-4000-8000-000000000123");
    }
}
