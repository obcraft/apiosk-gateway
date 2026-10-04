use super::*;

impl Engine {
    /// Buyer API continuation only accepts the saved choice/input actions.
    /// Claim before checking current actions so a retry can recover its receipt
    /// after the action has already been consumed. Never approves a new plan.
    pub async fn continue_task(
        &self,
        p: &Principal,
        bearer: &str,
        req: ExecuteRequest,
    ) -> anyhow::Result<Response> {
        if let Some(response) = self.request_claim(p, req.request_id, &digest(&("continue", p.connection, &req)), "continue").await? {
            return Ok(response);
        }
        let result = async {
            self.signer.verify(p.owner, &req.state, true)?;
            let id = req.state.state_ref;
            let (revision, task, connection) = self.store.load_scoped(p, id).await?;
            anyhow::ensure!(connection == p.connection, "task belongs to another connection");
            if task.cancelled {
                return Ok(Response::error(req.request_id, Status::Cancelled, "run_cancelled", "This run was cancelled."));
            }
            if revision != req.state.revision || req.state.expires_at <= Utc::now() {
                return self.view(p, id, req.request_id, Some(Status::StateConflict)).await;
            }
            if self.worker_active(id).await? {
                return Ok(Response::error(req.request_id, Status::StateConflict, "continuation_busy", "The run is still updating. Read its status before continuing."));
            }
            if task.proposal.as_ref().map(|v| serde_json::from_value::<Plan>(v.clone())).transpose()?
                .is_some_and(|plan| plan.expires_at <= Utc::now()) {
                return Ok(Response::error(req.request_id, Status::StateConflict, "plan_expired", "The original quote expired. Saved results remain available; no source call was repeated."));
            }
            let action = task.actions.iter().filter_map(|v| serde_json::from_value::<Action>(v.clone()).ok())
                .find(|action| action.id() == req.action_id);
            let Some(action @ (Action::Choose { .. } | Action::Supply { .. })) = action else {
                return Ok(Response::error(req.request_id, Status::StateConflict, "continuation_not_available", "Use a current select_entity or supply_input action returned by this run."));
            };
            if catalog_core::facts::schema::validate(&action.public().input_schema, &req.input).is_err() {
                return Ok(Response::error(req.request_id, Status::NeedsInput, "invalid_continuation_input", "Input must match the returned action schema and candidate identifiers."));
            }
            self.execute_inner(p, bearer, &req).await
        }.await;
        let result = match result {
            Err(error) if error.to_string() == "state_conflict" => Ok(Response::error(req.request_id, Status::StateConflict, "continuation_changed", "Another request changed this run. Read its current state.")),
            result => result,
        };
        Ok(self.complete(p, req.request_id, result).await)
    }

    pub async fn execute(
        &self,
        p: &Principal,
        bearer: &str,
        req: ExecuteRequest,
    ) -> anyhow::Result<Response> {
        if let Some(r) = self
            .request_claim(p, req.request_id, &digest(&("execute", p.connection, &req)), "execute")
            .await?
        {
            return Ok(r);
        }
        let result = self.execute_inner(p, bearer, &req).await;
        Ok(self.complete(p, req.request_id, result).await)
    }
    pub(super) async fn execute_inner(
        &self,
        p: &Principal,
        bearer: &str,
        req: &ExecuteRequest,
    ) -> anyhow::Result<Response> {
        self.signer.verify(p.owner, &req.state, true)?;
        if req.state.expires_at <= Utc::now() {
            return self
                .view(
                    p,
                    req.state.state_ref,
                    req.request_id,
                    Some(Status::StateConflict),
                )
                .await;
        }
        let id = req.state.state_ref;
        let (revision, mut task, connection) = self.store.load_scoped(&p, id).await?;
        anyhow::ensure!(
            connection == p.connection,
            "task belongs to another connection"
        );
        if revision != req.state.revision {
            return self
                .view(p, id, req.request_id, Some(Status::StateConflict))
                .await;
        }
        let action = if task.result_refs.contains(&req.action_id) {
            Action::Read {
                action_id: req.action_id,
                result_ref: req.action_id,
            }
        } else if req.action_id == id && task.proposal.is_some() {
            Action::Cancel { action_id: id }
        } else if saved_analysis_retry(&task, p.is_person())
            && task.proposal.as_ref().is_some_and(|plan| plan["quote_ref"] == json!(req.action_id)) {
            anyhow::ensure!(!self.worker_active(id).await?, "execution is still active");
            Action::RetryAnalysis { action_id: req.action_id }
        } else if task.pending_dispatches.iter().any(|pending| pending.action_id == req.action_id) {
            // A stopped worker can leave an already-journaled call outside the
            // current action list. Recovery reads its receipt only, even after
            // the quote expires; it never dispatches another source request.
            anyhow::ensure!(!self.worker_active(id).await?, "execution is still active");
            Action::Poll { action_id: req.action_id, execution_id: req.action_id }
        } else if self.executor.recoverable_receipts(p.owner, id).await?.contains(&req.action_id) {
            anyhow::ensure!(!self.worker_active(id).await?, "execution is still active");
            Action::Poll { action_id: req.action_id, execution_id: req.action_id }
        } else {
            task.actions
                .iter()
                .filter_map(|v| serde_json::from_value::<Action>(v.clone()).ok())
                .find(|a| a.id() == req.action_id)
                .ok_or_else(|| anyhow::anyhow!("unknown or stale action"))?
        };
        if single_call_mode()
            && matches!(
                action,
                Action::Run { .. } | Action::Choose { .. } | Action::Supply { .. }
            )
        {
            anyhow::ensure!(!task.proposal.as_ref().and_then(|v|v["steps"].as_array()).is_some_and(|v|v.len()>1), "Multi-step research is paused. Read the existing result or start a single-call question.");
        }
        let previous_analysis = if matches!(action, Action::RetryAnalysis { .. }) {
            task.analysis.clone().filter(|analysis| matches!(analysis["status"].as_str(), Some("completed" | "partial")))
        } else { None };
        let answering = matches!(action, Action::Analyze { .. } | Action::RetryAnalysis { .. });
        if matches!(action, Action::RetryAnalysis { .. }) {
            telemetry::set_segment(telemetry::Segment::RetryAnalysis);
        }
        let mut source_ready = None;
        match action {
            Action::Wait { until, .. } => {
                anyhow::ensure!(req.input.is_null() && !task.cancelled, "invalid wait input");
                if until > Utc::now() {
                    return self.view(p, id, req.request_id, None).await;
                }
                self.advance(&mut task)?;
            }
            Action::Analyze { .. } | Action::RetryAnalysis { .. } => {
                anyhow::ensure!(
                    req.input.is_null() && !task.cancelled,
                    "invalid analysis input"
                );
                let plan: Plan = serde_json::from_value(
                    task.proposal
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("missing plan"))?,
                )?;
                let intent: Intent = serde_json::from_value(
                    task.intent
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("missing intent"))?,
                )?;
                let documents = self.store.results(p.owner, id, &task.active_results).await?;
                let sources = answer_sources(&plan, &intent, &task, &documents);
                let operations = analysis_operations(&task);
                source_ready = Some(std::time::Instant::now());
                tracing::info!(trace = telemetry::id(), task = %id, quote = %plan.quote_ref, milestone = "sources_ready",
                    complete = task.failed_steps.is_empty() && !sources.is_empty(), "request milestone");
                telemetry::milestone(telemetry::Milestone::SourcesReady, id, Some(plan.quote_ref), telemetry::MilestoneFields {
                    sources: Some(sources.len()), failed_steps: Some(task.failed_steps.len()), pending: Some(task.pending_dispatches.len()), ..Default::default() });
                // Reviewed comparisons fail closed on incomplete saved source
                // slices; a model must not turn a truncated page into a complete answer.
                let reviewed = task.conversation.last()
                    .map(|turn| crate::direct_answer::reviewed_compose_with_intake(&turn.question, &intent, &sources, task.capability_intake.as_ref()))
                    .unwrap_or(Ok(None));
                let reviewed = match reviewed {
                    Ok(Some(answer)) if crate::direct_answer::reviewed_sources_ready(&answer, &task, &sources) => Some(answer),
                    Ok(Some(_)) => Some(json!({"status":"unavailable","limitations":["Not all source requests have completed successfully. Your source results are saved; no additional source purchase was made."],"observations":[]})),
                    Err(error) => {
                        tracing::warn!(task = %id, error = %error, "saved source projection could not be verified");
                        Some(json!({"status":"unavailable","limitations":["The saved source results are incomplete or inconsistent for this answer. Your source results are saved; no additional source purchase was made."],"observations":[]}))
                    }
                    Ok(None) => None,
                };
                let reviewed_source_answer = reviewed.is_some();
                let direct = reviewed.or_else(|| if task.failed_steps.is_empty() && task.pending_dispatches.is_empty() && task.supplier_check.is_none() {
                    // An explicit retry of a previously failed synthesis can
                    // recover supported registry fields without another model.
                    task.analysis.as_ref().filter(|answer|answer["status"]=="unavailable")
                        .and_then(|_|crate::direct_answer::registry_fallback(&intent,&sources,
                            task.capability_intake.as_ref().is_some_and(|intake|intake.parsed_request.language=="nl")))
                        .or_else(||task.conversation.last().and_then(|turn| crate::direct_answer::compose(&turn.question, &intent, &sources)))
                } else { None });
                let answering_span = telemetry::span(if direct.is_some() { Phase::DirectAnswer } else { Phase::Analysis }).task(id);
                let mut preserved_previous_analysis = false;
                task.analysis = Some(if let Some(answer) = direct {
                    tracing::info!(task = %id, "saved source answered directly");
                    answer
                } else if let Some(check) = task.supplier_check.clone() {
                    // Deterministic verdict from saved documents; no model and no purchase.
                    match self.supplier_verdict(p, id, &task, &plan, check).await {
                        Ok(analysis) => analysis,
                        Err(error) => {
                            tracing::warn!(error=%error, "supplier verdict failed");
                            json!({"status":"unavailable","limitations":["The supplier check could not be evaluated. Your source results are saved; no additional source purchase was made."],"observations":[]})
                        }
                    }
                } else {
                    let mut analysis_failures = task.failed_steps.clone();
                    for pending in &task.pending_dispatches {
                        if let Some(step) = plan.steps.get(pending.step) {
                            analysis_failures.insert(step.key(), json!({"code":"source_result_pending", "source":step.service.name, "message":"This source result is still pending. Its outcome is unknown; do not infer a passed check or a negative match."}));
                        }
                    }
                    let requirements = crate::completeness::requirements(&intent);
                    let answer = if task.workflow.is_some() || crate::uk_supplier::is_check(&intent) {
                        self.parser.analyze_workflow(&operations, &sources, &analysis_failures, &requirements).await
                    } else {
                        self.parser.analyze(&operations, &sources, &analysis_failures, &requirements).await
                    };
                    match answer {
                        Ok(analysis) => analysis,
                        Err(_) => {
                            if let Some(mut previous) = previous_analysis {
                                preserved_previous_analysis = true;
                                if !analysis_failures.is_empty() { previous["status"] = json!("partial"); }
                                let notice = json!("The refreshed analysis is unavailable. The previously verified findings are shown; no additional source purchase was made.");
                                if let Some(limitations) = previous["limitations"].as_array_mut() {
                                    if !limitations.contains(&notice) { limitations.push(notice); }
                                }
                                previous
                            } else if analysis_failures.is_empty() {
                                crate::direct_answer::registry_fallback(&intent,&sources,
                                    task.capability_intake.as_ref().is_some_and(|intake|intake.parsed_request.language=="nl"))
                                    .unwrap_or_else(||json!({"status":"unavailable","limitations":["The analysis could not be generated. Your source results are saved; no additional source purchase was made."],"observations":[]}))
                            } else {
                                json!({"status":"unavailable","limitations":["The analysis could not be generated. Your source results are saved; no additional source purchase was made."],"observations":[]})
                            }
                        }
                    }
                });
                answering_span.finish(telemetry::Outcome::Ok);
                if let Some(workflow) = &mut task.workflow {
                    let selected: Vec<_> = plan.steps.iter().map(|step| step.service.api_slug.as_str()).collect();
                    if let Some(limitations) = crate::workflow_catalog::selected_limitations(&workflow.slug, &selected) {
                        workflow.limitations = limitations;
                    }
                }
                if let (Some(workflow),Some(analysis))=(&task.workflow,task.analysis.as_mut()) {
                    if let Some(limitations)=analysis.get_mut("limitations").and_then(Value::as_array_mut) {
                        for limitation in &workflow.limitations {
                            let value=json!(limitation);
                            if !limitations.contains(&value) {limitations.push(value);}
                        }
                    }
                }
                if task.workflow.is_none() && task.supplier_check.is_none()
                    // A failed manual refresh retains the prior answer. It is
                    // not a new request for clarification or another model call.
                    && !preserved_previous_analysis
                    // Missing values in a verified source projection are
                    // source limitations, not missing information from the user.
                    && !reviewed_source_answer
                    && !crate::billing::purchase_policy_blocked(&task.failed_steps)
                    && task.analysis.as_ref().is_some_and(|a| a["status"] != "unavailable"
                        // Recovery from a failed synthesis already has the
                        // requested entity and saved fields. Asking the model
                        // for more context would turn this read-only recovery
                        // into another model call and an irrelevant question.
                        && a["fallback"] != "saved_registry_fields"
                        && (a["observations"].as_array().is_some_and(Vec::is_empty)
                            || (a["status"] == "partial" && a["limitations"].as_array().is_some_and(|notes| !notes.is_empty())))) {
                    if let Some(question) = task.conversation.last().map(|turn| turn.question.clone()).filter(|q| !q.is_empty()) {
                        let gap = json!({"stage":"after_source_result",
                            "limitations":task.analysis.as_ref().map(|a| &a["limitations"]),
                            "source_outcomes":sources.iter().map(|source| json!({
                                "provider":source["document"]["source"]["provider"],
                                "subject":source["document"]["subject"]["label"],
                                "coverage":source["document"]["coverage"],
                                "coverage_note":source["document"]["coverage_note"]
                            })).collect::<Vec<_>>(),
                            "failed_checks":task.failed_steps.values().take(4).map(|failure| json!({
                                "code":failure["code"],"message":failure["message"]
                            })).collect::<Vec<_>>()});
                        let fallback = if question.to_ascii_lowercase().contains("filing") {
                            "Which filing form or disclosure topic should I check next?"
                        } else {
                            "What detail can you add to narrow down the result you need?"
                        };
                        let existing = task.analysis.as_ref().and_then(|a| a["follow_up_question"].as_str()).map(str::to_owned);
                        let follow_up = if let Some(question) = existing { question }
                            else if source_ready.as_ref().is_some_and(|started| started.elapsed() < std::time::Duration::from_secs(45)) {
                                self.parser.ask_for_missing_context(&question, &task, &gap).await
                                    .unwrap_or_else(|_| fallback.to_owned())
                            } else { fallback.to_owned() };
                        if let Some(analysis) = task.analysis.as_mut() {
                            analysis["status"] = json!("partial");
                            analysis["follow_up_question"] = json!(follow_up);
                        }
                    }
                }
                task.actions.clear();
                if task
                    .analysis
                    .as_ref()
                    .is_some_and(|a| a["status"] == "unavailable")
                {
                    // Only an explicit click retries saved documents. Never poll-loop or re-purchase.
                    task.actions
                        .push(serde_json::to_value(Action::RetryAnalysis {
                            action_id: Uuid::new_v4(),
                        })?);
                }
            }
            Action::Upload { .. } => {
                // Uploads are read by the App through /v2/documents; the read
                // question is sent as a new chat turn, never executed here.
                anyhow::bail!("upload the document in the App; it is read there and sent as the next question");
            }
            Action::Read { result_ref, .. } => {
                anyhow::ensure!(
                    req.input.is_null()
                        || req
                            .input
                            .as_object()
                            .is_some_and(|m| m.keys().all(|k| k == "offset")),
                    "unexpected read input"
                );
                let mut r = self.view(p, id, req.request_id, None).await?;
                r.result = Some(
                    self.result_page(
                        p.owner,
                        id,
                        result_ref,
                        req.input["offset"].as_u64().unwrap_or(0) as usize,
                    )
                    .await?,
                );
                return Ok(r);
            }
            Action::Cancel { .. } => {
                anyhow::ensure!(req.input.is_null(), "unexpected cancel input");
                task.cancelled = true;
                task.actions.retain(|a| a["kind"] == "poll");
            }
            Action::Supply {
                entity_ref,
                semantic_type,
                schema,
                ..
            } => {
                anyhow::ensure!(
                    req.input
                        .as_object()
                        .is_some_and(|m| m.len() == 1 && m.contains_key("value")),
                    "send only value"
                );
                catalog_core::facts::schema::validate(&schema, &req.input["value"])?;
                let catalog = self.catalog.get().await?;
                self.user_fact(
                    &mut task,
                    &catalog,
                    entity_ref,
                    &semantic_type,
                    req.input["value"].clone(),
                )?;
                let intent: Intent = serde_json::from_value(
                    task.intent
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("missing intent"))?,
                )?;
                self.plan(&mut task, &catalog, &intent)?;
            }
            Action::Choose { candidates, .. } => {
                anyhow::ensure!(
                    req.input
                        .as_object()
                        .is_some_and(|m| m.len() == 1 && m.contains_key("entity_ref")),
                    "send only entity_ref"
                );
                let selected: Uuid = req.input["entity_ref"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("select an entity"))?
                    .parse()?;
                anyhow::ensure!(candidates.contains(&selected), "unknown candidate");
                let original = task
                    .original_subject
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("missing subject"))?;
                let chosen: Vec<Fact> = task
                    .facts
                    .iter()
                    .filter(|f| f.entity_ref == selected)
                    .cloned()
                    .collect();
                for mut fact in chosen {
                    fact.entity_ref = original;
                    task.facts.retain(|known| {
                        known.entity_ref != original || known.semantic_type != fact.semantic_type
                    });
                    task.facts.push(fact)
                }
                if let Some(label) = task
                    .entities
                    .iter()
                    .find(|e| e.entity_ref == selected)
                    .map(|e| e.label.clone())
                {
                    if let Some(entity) =
                        task.entities.iter_mut().find(|e| e.entity_ref == original)
                    {
                        entity.label = label;
                    }
                }
                if let Some(e) = task.entities.iter_mut().find(|e| e.entity_ref == selected) {
                    e.selected = true;
                }
                task.pending_candidates.clear();
                task.candidate_note = None;
                self.advance(&mut task)?;
            }
            Action::Run { step, .. } => {
                if !self.executor.recoverable_receipts(p.owner, id).await?.is_empty() {
                    return self.view(p, id, req.request_id, None).await;
                }
                anyhow::ensure!(
                    task.result_refs.len() < 100,
                    "task result capacity reached; start a new task"
                );
                anyhow::ensure!(
                    req.input.is_null() && !task.cancelled,
                    "invalid execution input or cancelled task"
                );
                let plan: Plan = serde_json::from_value(
                    task.proposal
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("missing quote"))?,
                )?;
                anyhow::ensure!(req.quote_ref == Some(plan.quote_ref), "quote_ref required");
                let auth = if let Some(id) = req.authorization_ref {
                    Some(id)
                } else {
                    sqlx::query_scalar("SELECT id FROM authorizations WHERE task_id=$1 AND owner_id=$2 AND quote_ref=$3 AND revoked_at IS NULL AND expires_at>now()").bind(id).bind(p.owner).bind(plan.quote_ref).fetch_optional(&self.store.pool).await?
                };
                let Some(auth) = auth else {
                    return self
                        .view(p, id, req.request_id, Some(Status::RequiresApproval))
                        .await;
                };
                let selected = plan
                    .steps
                    .get(step)
                    .ok_or_else(|| anyhow::anyhow!("invalid step"))?;
                anyhow::ensure!(!task.step_finished(&selected.key()), "step already finished");
                let fresh = self.catalog.fresh_execution(&[selected.service.id]).await?;
                // The executor validates the entire remaining quote before
                // reservation and returns a saved, user-visible refusal.
                anyhow::ensure!(crate::browser_fallback::ready(&task, selected), "fallback is not eligible");
                let inputs = crate::browser_fallback::inputs(selected, &task, &fresh.vocabulary)?;
                match self
                    .executor
                    .dispatch(
                        p,
                        bearer,
                        id,
                        revision,
                        req.action_id,
                        req.idempotency_key,
                        auth,
                        &plan,
                        selected,
                        &inputs,
                    )
                    .await?
                {
                    Dispatch::Waiting { until } => {
                        task.actions = vec![serde_json::to_value(Action::Wait {
                            action_id: Uuid::new_v4(),
                            until,
                        })?];
                    }
                    Dispatch::Failed { body } => {
                        self.fail_step(&mut task, selected, crate::billing::public_error(&body))?;
                    }
                    Dispatch::Done { execution_id, body } => {
                        self.ingest(p, id, &mut task, selected, execution_id, body, &inputs, &fresh)
                            .await?;
                    }
                    Dispatch::Pending { execution_id } => {
                        task.pending_dispatches.push(crate::state::PendingDispatch {
                            step, action_id: execution_id, inputs,
                        });
                        task.actions = vec![serde_json::to_value(Action::Poll {
                            action_id: Uuid::new_v4(),
                            execution_id,
                        })?];
                    }
                }
            }
            Action::Poll { execution_id, .. } => {
                anyhow::ensure!(req.input.is_null(), "unexpected poll input");
                let plan: Plan = serde_json::from_value(task.proposal.clone().ok_or_else(|| anyhow::anyhow!("missing quote"))?)?;
                selection::pending_choice(&mut task, &plan);
                let catalog = self.catalog.get().await?;
                let receipt = self.executor.receipt_step(p.owner, id, execution_id, &plan, &task, &catalog).await?;
                if !task.pending_dispatches.iter().any(|pending| pending.action_id == execution_id) {
                    task.pending_dispatches.push(receipt.clone());
                }
                if let Some(body) = self.executor.poll(p, id, execution_id).await? {
                    task.pending_dispatches.retain(|pending| pending.action_id != execution_id);
                    if body["ok"] == true {
                        let step = plan.steps.get(receipt.step)
                            .ok_or_else(|| anyhow::anyhow!("missing step"))?
                            .clone();
                        self.ingest(p, id, &mut task, &step, execution_id, body, &receipt.inputs, &catalog)
                            .await?;
                    } else {
                        let step = plan.steps.get(receipt.step)
                            .ok_or_else(|| anyhow::anyhow!("missing step"))?;
                        // Retry one confirmed, fully refunded KVK throttle under
                        // the original quote. Unknown payment outcomes never enter
                        // this branch. Dispatch rechecks consent, price and budget.
                        let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM executions e JOIN user_selected_apis s ON s.id=e.selection_id WHERE e.task_id=$1 AND e.owner_id=$2 AND s.candidate_id=$3 AND e.response->>'error'='rate_limited_refunded'")
                            .bind(id).bind(p.owner).bind(step.service.endpoint_id).fetch_one(&self.store.pool).await?;
                        if body["error"] == "rate_limited_refunded"
                            && step.service.path == crate::annual_report::ENDPOINT
                            && attempts == 1
                            && !task.cancelled
                        {
                            task.actions = vec![serde_json::to_value(Action::Wait {
                                action_id: Uuid::new_v4(),
                                until: Utc::now() + chrono::Duration::seconds(65),
                            })?];
                            telemetry::source_retry();
                        } else {
                            self.fail_step(&mut task, step, crate::billing::public_error(&body))?;
                        }
                    }
                }
            }
        }
        self.store.save(p.owner, id, revision, &task).await?;
        let response = self.view(p, id, req.request_id, None).await?;
        if answering {
            if let Some(started) = source_ready {
                telemetry::answer(id, task.proposal.as_ref().and_then(|p| p["quote_ref"].as_str()).and_then(|q| q.parse().ok()), &task);
                tracing::info!(trace = telemetry::id(), task = %id,
                    quote = task.proposal.as_ref().and_then(|p| p["quote_ref"].as_str()).unwrap_or_default(),
                    stage = "sources_to_answer", stage_ms = started.elapsed().as_millis() as u64,
                    success = task.analysis.as_ref().is_some_and(|a| a["status"] == "completed"),
                    "request stage completed");
            }
        }
        Ok(response)
    }
    pub(super) fn advance(&self, task: &mut Task) -> anyhow::Result<()> {
        let plan: Plan = serde_json::from_value(
            task.proposal
                .clone()
                .ok_or_else(|| anyhow::anyhow!("missing plan"))?,
        )?;
        crate::browser_fallback::settle_unused(task, &plan);
        // An expired spending window must not strand purchased evidence behind
        // another approval. Finish a partial report; pending receipts are still
        // recoverable, while unstarted sources are explicitly unavailable.
        if plan.expires_at <= Utc::now() && !task.active_results.is_empty() {
            for (index, step) in plan.steps.iter().enumerate() {
                if !task.step_finished(&step.key())
                    && !task.pending_dispatches.iter().any(|pending| pending.step == index) {
                    task.failed_steps.insert(step.key(), json!({
                        "code":"quote_expired",
                        "message":"This source was not requested before the approved spending window expired. The report uses saved results only."
                    }));
                }
            }
        }
        let choice = selection::pending_choice(task, &plan);
        task.actions.clear();
        if !task.cancelled {
            let mut next_step = None;
            for (index, candidate) in plan.steps.iter().enumerate() {
                if task.step_finished(&candidate.key()) || !crate::browser_fallback::ready(task, candidate) {
                    continue;
                }
                if task.pending_dispatches.iter().any(|pending| pending.step == index) {
                    continue;
                }
                if choice.is_some() && selection::step_waits_for_choice(task, candidate) {
                    continue;
                }
                if candidate.browser_fallback.is_some() {
                    next_step = Some(index);
                    break;
                }
                let missing: Vec<_> = candidate
                    .service
                    .contract
                    .requires
                    .iter()
                    .filter(|r| !r.optional)
                    .filter(|r| {
                        !crate::catalog::fact_value(&task.facts, candidate.entity_ref, &r.fact_type)
                            .is_ok_and(|v| v.is_some())
                    })
                    .filter(|r| {
                        !plan.steps.iter().any(|parent| {
                            parent.entity_ref == candidate.entity_ref
                                && !task.step_finished(&parent.key())
                                && parent.key() != candidate.key()
                                && parent
                                    .service
                                    .contract
                                    .output_map
                                    .produces
                                    .contains_key(&r.fact_type)
                        })
                    })
                    .map(|r| r.fact_type.clone())
                    .collect();
                if !missing.is_empty() {
                    if crate::uk_supplier::unused_screening_slot(task, candidate) {
                        task.completed.push(candidate.key());
                        continue;
                    }
                    task.failed_steps.insert(candidate.key(), json!({"code":"missing_source_fact","message":"Not requested because the required source identifier is unavailable.","fields":missing}));
                    continue;
                }
                next_step = Some(index);
                break;
            }
            if let Some(step) = next_step {
                task.actions.push(serde_json::to_value(Action::Run {
                    action_id: Uuid::new_v4(),
                    step,
                })?);
            } else if choice.is_none() && task.pending_dispatches.is_empty() && task.analysis.is_none()
                && (task
                    .conversation
                    .last()
                    .is_some_and(|turn| turn.reply.is_some())
                    || task.intent.as_ref().is_some_and(|v| {
                        v["answer_operations"]
                            .as_array()
                            .is_some_and(|a| !a.is_empty())
                    }))
            {
                task.actions.push(serde_json::to_value(Action::Analyze {
                    action_id: Uuid::new_v4(),
                })?);
            }
            if let Some(choice) = choice {
                task.actions.push(choice);
            }
            for pending in &task.pending_dispatches {
                task.actions.push(serde_json::to_value(Action::Poll {
                    action_id: pending.action_id, execution_id: pending.action_id,
                })?);
            }
        }
        Ok(())
    }

    pub(super) fn fail_step(
        &self,
        task: &mut Task,
        failed: &resolver::Step,
        error: Value,
    ) -> anyhow::Result<()> {
        task.failed_steps.insert(failed.key(), error);
        let plan: Plan = serde_json::from_value(
            task.proposal
                .clone()
                .ok_or_else(|| anyhow::anyhow!("missing plan"))?,
        )?;
        // Skip only descendants of this failure. Independent companies and
        // goals remain executable under the original approved cap.
        for step in &plan.steps {
            if task.step_finished(&step.key()) || step.browser_fallback.is_some() {
                continue;
            }
            if plan.steps.iter().any(|parent| {
                parent.entity_ref == step.entity_ref
                    && task.failed_steps.contains_key(&parent.key())
                    && step.service.contract.requires.iter().any(|r| {
                        !r.optional
                            && parent
                                .service
                                .contract
                                .output_map
                                .produces
                                .contains_key(&r.fact_type)
                    })
            }) {
                task.failed_steps.insert(step.key(), json!({"code":"dependency_failed","message":"Not requested because a required earlier source step failed."}));
            }
        }
        self.advance(task)
    }
}

/// The saved documents an answer may cite: results of the asked steps, of a
/// quoted fallback, of a requested map, of a location identity dependency and
/// of a UK supplier check, in the
/// order they were saved. The offline replay (`crate::replay`) uses the same
/// selection as a live answer.
pub(crate) fn answer_sources(plan: &Plan, intent: &Intent, task: &Task, documents: &std::collections::BTreeMap<Uuid, Value>) -> Vec<Value> {
    let mut sources = Vec::new();
    for result in &task.active_results {
        let document = &documents[result];
        if plan.steps.iter().any(|step| {
            document["source"]["endpoint_id"] == json!(step.service.endpoint_id)
                && document["subject"]["entity_ref"] == json!(step.entity_ref)
                && (step.browser_fallback.is_some()
                    || task.conversation.last().is_some_and(|turn| crate::report_assets::requested(&turn.question))
                    || (document["source"]["provider"] == "pdok" && task.conversation.last().is_some_and(|turn| crate::location_answer::includes_dependency(&turn.question, intent, step)))
                    || (location_identity_dependency(plan, intent, step)
                        && retrieval_scope(step, task, document).is_some())
                    || crate::uk_supplier::is_check(intent) || intent.goals.iter().any(|goal| {
                    goal.capability == step.service.contract.capability_slug
                        && intent
                            .subjects
                            .get(goal.subject)
                            .is_some_and(|s| s.entity_ref == Some(step.entity_ref))
                }))
        }) {
            let mut source = json!({"result_ref":result,"document":document});
            if let Some(scope) = plan.steps.iter().find_map(|step| retrieval_scope(step, task, document)) {
                source["retrieval_scope"] = scope;
            }
            sources.push(source);
        }
    }
    sources
}

/// A neighbourhood record is joined to an address by a quoted BAG-address
/// dependency. Keep that original evidence even when the user also requests
/// another source: prose grammar must not erase an already-established join.
/// This admits context only, never an inferred identity or a completeness pass.
fn location_identity_dependency(plan: &Plan, intent: &Intent, step: &crate::resolver::Step) -> bool {
    let service = &step.service;
    let requested = |target: &crate::resolver::Step| intent.goals.iter().any(|goal| {
        goal.capability == target.service.contract.capability_slug
            && intent.subjects.get(goal.subject)
                .is_some_and(|subject| subject.entity_ref == Some(target.entity_ref))
            && goal.constraints.get("source").is_none_or(|source| source == &json!(target.service.api_slug))
            && goal.constraints.get("endpoint_id").is_none_or(|id| id == &json!(target.service.endpoint_id))
    });
    if !plan.steps.iter().any(|target| target.service.api_slug != "pdok" && requested(target))
        || service.api_slug != "pdok"
        || service.method != "GET"
        || service.path != "/apiosk/bag-address"
        || service.contract.capability_slug != "location.bag-address-context"
        || !service.contract.output_map.produces.contains_key("location.neighbourhood_code")
    {
        return false;
    }
    plan.steps.iter().any(|target| {
        target.entity_ref == step.entity_ref
            && target.service.api_slug == "pdok"
            && target.service.method == "GET"
            && target.service.path == "/apiosk/neighbourhood"
            && target.service.contract.capability_slug == "area.cbs-neighbourhood-2025"
            && target.service.contract.requires.iter().any(|input| {
                !input.optional && input.fact_type == "location.neighbourhood_code"
            })
            && requested(target)
    })
}

#[cfg(test)]
#[path = "answer_context_tests.rs"]
mod answer_context_tests;

/// Retrieval semantics come from the approved service snapshot, never from a
/// provider response or the current catalog. They explain what was fetched;
/// only the saved document's facts may substantiate answer values.
fn retrieval_scope(step: &crate::resolver::Step, task: &Task, document: &Value) -> Option<Value> {
    let service = &step.service;
    let source = &document["source"];
    if !task.completed.contains(&step.key())
        || task.failed_steps.contains_key(&step.key())
        || task.skipped_steps.contains(&step.key())
        || source["endpoint_id"] != json!(service.endpoint_id)
        || source["provider"] != json!(service.api_slug)
        || source["contract_version"] != json!(service.contract.version)
        || source["path"] != json!(service.path)
        || document["subject"]["entity_ref"] != json!(step.entity_ref)
    {
        return None;
    }
    let mut scope = json!({
        "capability":service.contract.capability_slug,
        "provider":service.api_slug,
        "method":service.method,
        "path":service.path,
        "contract_version":service.contract.version,
        "freshness_seconds":service.freshness_seconds,
    });
    if let Some(description) = service.description.as_deref().map(str::trim).filter(|s| {
        !s.is_empty() && s.chars().count() <= 1200 && !s.chars().any(|c| c.is_control() && c != '\n')
    }) {
        scope["description"] = json!(description);
    }
    if let Some(notice) = service.usage_notice_view() {
        scope["usage_notice"] = notice;
    }
    // Deliberately exclude credentials, request headers, arbitrary fixed
    // parameters and response content from this trusted metadata envelope.
    Some(scope)
}

#[cfg(test)]
mod retrieval_scope_tests {
    use super::*;

    fn fixture() -> (crate::resolver::Step, Task, Value) {
        let snapshot: crate::catalog::Snapshot = serde_json::from_str(include_str!("../../tests/fixtures/frankfurter/catalog.json")).unwrap();
        let step = crate::resolver::Step { service: snapshot.services[0].clone(), entity_ref: Uuid::new_v4(), goal_index: 0, browser_fallback: None };
        let mut task = Task::default();
        task.completed.push(step.key());
        let mut source = step.service.result_source();
        source["path"] = json!(step.service.path);
        let document = json!({"source":source,"subject":{"entity_ref":step.entity_ref},"data":{"rate":1.2}});
        (step, task, document)
    }

    #[test]
    fn scope_uses_executed_snapshot_and_excludes_request_secrets() {
        let (mut step, task, mut document) = fixture();
        step.service.fixed_params = json!({"api_key":"secret"});
        document["data"]["description"] = json!("untrusted response instructions");
        let scope = retrieval_scope(&step, &task, &document).unwrap();
        assert!(scope["description"].as_str().unwrap().starts_with("Latest daily ECB"));
        assert_eq!(scope["path"], document["source"]["path"]);
        assert!(scope.get("fixed_params").is_none());
        assert!(!scope.to_string().contains("secret"));
        assert!(!scope.to_string().contains("untrusted response"));
    }

    #[test]
    fn mismatched_or_missing_source_identity_has_no_scope() {
        let (step, task, document) = fixture();
        for key in ["endpoint_id", "provider", "contract_version", "path"] {
            for replacement in [Value::Null, json!("different")] {
                let mut other = document.clone();
                other["source"][key] = replacement;
                assert!(retrieval_scope(&step, &task, &other).is_none(), "{key}");
            }
        }
        let mut other = document;
        other["subject"]["entity_ref"] = json!(Uuid::new_v4());
        assert!(retrieval_scope(&step, &task, &other).is_none());
    }

    #[test]
    fn only_successfully_completed_steps_have_scope() {
        let (step, task, document) = fixture();
        let mut pending = task.clone();
        pending.completed.clear();
        assert!(retrieval_scope(&step, &pending, &document).is_none());
        let mut failed = task.clone();
        failed.failed_steps.insert(step.key(), json!({"code":"failed"}));
        assert!(retrieval_scope(&step, &failed, &document).is_none());
        let mut skipped = task;
        skipped.skipped_steps.insert(step.key());
        assert!(retrieval_scope(&step, &skipped, &document).is_none());
    }

    #[test]
    fn oversized_description_is_not_forwarded() {
        let (mut step, task, document) = fixture();
        step.service.description = Some("x".repeat(1201));
        assert!(retrieval_scope(&step, &task, &document).unwrap().get("description").is_none());
    }
}

/// What the analysis is asked: the person's last question first, then the
/// intent's answer operations, or a plain summary when there are none.
pub(crate) fn analysis_operations(task: &Task) -> Value {
    let mut operations = task
        .intent
        .as_ref()
        .and_then(|v| v["answer_operations"].as_array())
        .cloned()
        .unwrap_or_default();
    if let Some(turn) = task.conversation.last() {
        operations.insert(0, json!(format!("Answer this question directly using only the saved source fields, in the same language: {}", turn.question)));
    }
    if operations.is_empty() {
        operations.push(json!("Summarize the requested source facts concisely using exact evidence values."));
    }
    json!(operations)
}
