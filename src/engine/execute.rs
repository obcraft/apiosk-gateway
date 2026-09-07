use super::*;

impl Engine {
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
    async fn execute_inner(
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
        let (revision, mut task, connection) = self.store.load(p.owner, id).await?;
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
        } else {
            task.actions
                .iter()
                .filter_map(|v| serde_json::from_value::<Action>(v.clone()).ok())
                .find(|a| a.id() == req.action_id)
                .ok_or_else(|| anyhow::anyhow!("unknown or stale action"))?
        };
        if single_call_mode() && matches!(action, Action::Run {..} | Action::Choose {..} | Action::Supply {..}) {
            anyhow::ensure!(!task.proposal.as_ref().and_then(|v|v["steps"].as_array()).is_some_and(|v|v.len()>1), "Multi-step research is paused. Read the existing result or start a single-call question.");
        }
        match action {
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
                r.result = Some(self.result_page(p.owner, id, result_ref, req.input["offset"].as_u64().unwrap_or(0) as usize).await?);
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
                    task.facts.push(fact)
                }
                if let Some(e) = task.entities.iter_mut().find(|e| e.entity_ref == selected) {
                    e.selected = true;
                }
                task.pending_candidates.clear();
                self.advance(&mut task)?;
            }
            Action::Run { step, .. } => {
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
                    sqlx::query_scalar("SELECT id FROM gateway_v2.authorizations WHERE task_id=$1 AND owner_id=$2 AND quote_ref=$3 AND revoked_at IS NULL AND expires_at>now()").bind(id).bind(p.owner).bind(plan.quote_ref).fetch_optional(&self.store.pool).await?
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
                let fresh = self.catalog.fresh().await?;
                anyhow::ensure!(
                    fresh.services.iter().any(|s| s.id == selected.service.id
                        && s.version_hash() == selected.service.version_hash()),
                    "catalog or price changed; request a new quote"
                );
                let inputs =
                    selected
                        .service
                        .inputs(&task, selected.entity_ref, &fresh.vocabulary)?;
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
                    Dispatch::Failed { body } => {
                        task.cancelled = true;
                        task.actions.clear();
                        task.execution_error = Some(crate::billing::public_error(&body));
                    }
                    Dispatch::Done { execution_id, body } => {
                        self.ingest(p, id, &mut task, selected, execution_id, body, &fresh)
                            .await?;
                    }
                    Dispatch::Pending { execution_id } => {
                        task.actions = vec![serde_json::to_value(Action::Poll {
                            action_id: Uuid::new_v4(),
                            execution_id,
                        })?];
                    }
                }
            }
            Action::Poll { execution_id, .. } => {
                anyhow::ensure!(req.input.is_null(), "unexpected poll input");
                if let Some(body) = self.executor.poll(p, id, execution_id).await? {
                    if body["ok"] == true {
                        let plan: Plan = serde_json::from_value(
                            task.proposal
                                .clone()
                                .ok_or_else(|| anyhow::anyhow!("missing quote"))?,
                        )?;
                        let step = plan
                            .steps
                            .iter()
                            .find(|s| !task.completed.contains(&s.key()))
                            .ok_or_else(|| anyhow::anyhow!("missing step"))?
                            .clone();
                        let catalog = self.catalog.get().await?;
                        self.ingest(p, id, &mut task, &step, execution_id, body, &catalog)
                            .await?;
                    } else {
                        task.cancelled = true;
                        task.execution_error = Some(crate::billing::public_error(&body));
                        task.actions.clear();
                    }
                }
            }
        }
        self.store.save(p.owner, id, revision, &task).await?;
        self.view(p, id, req.request_id, None).await
    }
    pub(super) fn advance(&self, task: &mut Task) -> anyhow::Result<()> {
        let plan: Plan = serde_json::from_value(
            task.proposal
                .clone()
                .ok_or_else(|| anyhow::anyhow!("missing plan"))?,
        )?;
        task.actions.clear();
        if !task.cancelled {
            if let Some((step, _)) = plan
                .steps
                .iter()
                .enumerate()
                .find(|(_, s)| !task.completed.contains(&s.key()))
            {
                task.actions.push(serde_json::to_value(Action::Run {
                    action_id: Uuid::new_v4(),
                    step,
                })?);
            }
        }
        Ok(())
    }
}
