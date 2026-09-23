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
        if let Some(response) = self.request_claim(p, req.request_id, &digest(&("continue", p.connection, &req))).await? {
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
            .request_claim(p, req.request_id, &digest(&("execute", p.connection, &req)))
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
        } else if task.workflow.is_some() && !task.active_results.is_empty()
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
                let mut sources = Vec::new();
                let documents = self.store.results(p.owner, id, &task.active_results).await?;
                for result in &task.active_results {
                    let document = &documents[result];
                    if plan.steps.iter().any(|step| {
                        document["source"]["endpoint_id"] == json!(step.service.endpoint_id)
                            && document["subject"]["entity_ref"] == json!(step.entity_ref)
                            && (step.browser_fallback.is_some()
                                || task.conversation.last().is_some_and(|turn| crate::report_assets::requested(&turn.question))
                                || crate::uk_supplier::is_check(&intent) || intent.goals.iter().any(|goal| {
                                goal.capability == step.service.contract.capability_slug
                                    && intent
                                        .subjects
                                        .get(goal.subject)
                                        .is_some_and(|s| s.entity_ref == Some(step.entity_ref))
                            }))
                    }) {
                        sources.push(json!({"result_ref":result,"document":document}));
                    }
                }
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
                let operations = json!(operations);
                source_ready = Some(std::time::Instant::now());
                tracing::info!(task = %id, quote = %plan.quote_ref, milestone = "sources_ready",
                    complete = task.failed_steps.is_empty() && !sources.is_empty(), "request milestone");
                let direct = if task.failed_steps.is_empty() && task.pending_dispatches.is_empty() && task.supplier_check.is_none() {
                    task.conversation.last().and_then(|turn| crate::direct_answer::compose(&turn.question, &intent, &sources))
                } else { None };
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
                    let answer = if task.workflow.is_some() || crate::uk_supplier::is_check(&intent) {
                        self.parser.analyze_workflow(&operations, &sources, &analysis_failures).await
                    } else {
                        self.parser.analyze(&operations, &sources, &analysis_failures).await
                    };
                    match answer {
                        Ok(analysis) => analysis,
                        Err(_) => {
                            if let Some(mut previous) = previous_analysis {
                                if !analysis_failures.is_empty() { previous["status"] = json!("partial"); }
                                let notice = json!("The refreshed analysis is unavailable. The previously verified findings are shown; no additional source purchase was made.");
                                if let Some(limitations) = previous["limitations"].as_array_mut() {
                                    if !limitations.contains(&notice) { limitations.push(notice); }
                                }
                                previous
                            } else {
                                json!({"status":"unavailable","limitations":["The analysis could not be generated. Your source results are saved; no additional source purchase was made."],"observations":[]})
                            }
                        }
                    }
                });
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
                    && task.analysis.as_ref().is_some_and(|a| a["status"] != "unavailable"
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
                        self.ingest(p, id, &mut task, selected, execution_id, body, &fresh)
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
                        self.ingest(p, id, &mut task, &step, execution_id, body, &catalog)
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
                tracing::info!(task = %id,
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
