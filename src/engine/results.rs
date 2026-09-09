use super::*;

impl Engine {
    pub(super) async fn result_page(&self, owner: Uuid, task: Uuid, result_ref: Uuid, offset: usize) -> anyhow::Result<Value> {
        let mut data = self.store.result(owner, task, result_ref).await?;
        data["result_ref"] = json!(result_ref);
        if crate::annual_report::eligible(&data) {
            let expires = (Utc::now() + chrono::Duration::hours(24)).timestamp();
            let signature = self.signer.report_signature(owner, task, result_ref, expires);
            data["report"] = json!({"format":"pdf", "filename":format!("jaarrekening-{result_ref}.pdf"), "expires_at":expires,
                "download_path":format!("/v2/tasks/{task}/results/{result_ref}/report.pdf?owner={owner}&expires={expires}&signature={signature}")});
        }
        if let Ok(sources) = self.catalog.sources().await { crate::sources::brand_result(&mut data, &sources); }
        let serialized = serde_json::to_string(&data)?;
        if serialized.len() <= MAX_INLINE { return Ok(data); }
        let chars: Vec<char> = serialized.chars().collect();
        anyhow::ensure!(offset <= chars.len(), "invalid result offset");
        let end = (offset + 8000).min(chars.len());
        Ok(json!({"result_ref":result_ref,"encoding":"json_text","subject":data.get("subject"),"source":data.get("source"),"report":data.get("report"),"text":chars[offset..end].iter().collect::<String>(),"next_offset":if end<chars.len(){Some(end)}else{None}}))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn ingest(
        &self,
        p: &Principal,
        task_id: Uuid,
        task: &mut Task,
        step: &resolver::Step,
        execution: Uuid,
        body: Value,
        catalog: &Catalog,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            body["ok"] == true,
            "provider did not return a complete result"
        );
        let data = body.get("result").cloned().unwrap_or(Value::Null);
        let extraction = step.service.contract.extract(&data, &catalog.vocabulary);
        let capacity_exceeded = extraction.as_ref().is_ok_and(|e| {
            task.entities.len() + e.subjects.len() > 100
                || task.facts.len() + e.subjects.iter().map(|s| s.facts.len()).sum::<usize>() > 1000
                || serde_json::to_vec(&data).is_ok_and(|b| b.len() > 512 * 1024)
        });
        let mut document = json!({"data":data,"source":{"provider":step.service.api_slug,"name":step.service.name,"url":step.service.source_url,"endpoint_id":step.service.endpoint_id,"contract_version":step.service.contract.version,"retrieved_at":Utc::now()},"charged_atomic":body["charged_micro_usd"].as_i64().unwrap_or(0).to_string(),"currency":"USD","missing_fields":[],"coverage":"source_response"});
        document["execution_id"] = json!(execution);
        document["subject"] = json!({"entity_ref":step.entity_ref,"label":task.entities.iter().find(|e| e.entity_ref == step.entity_ref).map(|e| &e.label)});
        document["source"]["path"] = json!(step.service.path);
        if step.service.path == crate::annual_report::ENDPOINT {
            document["report_subject"] = json!({
                "name": task.entities.iter().find(|e| e.entity_ref == step.entity_ref).map(|e| &e.label),
                "identifiers": task.facts.iter().filter(|f| f.entity_ref == step.entity_ref && matches!(f.semantic_type.as_str(), "company.registration.nl.kvk" | "company_registry.kvknummer" | "company.kvknummer")).map(|f| &f.value).collect::<Vec<_>>()
            });
        }
        document["receipt_ref"] = body.get("execution_id").cloned().unwrap_or(Value::Null);
        document["provider_cost_micro_usd"] =
            json!(body["provider_micro_usd"].as_i64().map(|v| v.to_string()));
        document["apiosk_fee_micro_usd"] =
            json!(body["fee_micro_usd"].as_i64().map(|v| v.to_string()));
        let missing: Vec<String> = match &extraction {
            Ok(e) => step
                .service
                .contract
                .output_map
                .produces
                .keys()
                .filter(|kind| {
                    !e.subjects
                        .iter()
                        .any(|s| s.facts.iter().any(|f| f.fact_type == **kind))
                })
                .cloned()
                .collect(),
            Err(_) => vec!["output_schema_mismatch".into()],
        };
        if !missing.is_empty() {
            document["missing_fields"] = json!(missing);
            document["coverage"] = json!("partial");
        }
        if body["result_truncated"] == true {
            document["coverage"] = json!("partial");
            document["missing_fields"] = json!(["upstream_result_truncated"]);
        }
        if capacity_exceeded {
            document["coverage"] = json!("partial");
            document["missing_fields"] = json!(["automatic_fact_binding_capacity_exceeded"]);
        }
        let result_ref:Uuid=sqlx::query_scalar("INSERT INTO gateway_v2.results(id,owner_id,task_id,execution_id,document) VALUES($1,$2,$3,$4,$5) ON CONFLICT(execution_id) DO UPDATE SET execution_id=EXCLUDED.execution_id RETURNING id")
            .bind(Uuid::new_v4()).bind(p.owner).bind(task_id).bind(execution).bind(&document).fetch_one(&self.store.pool).await?;
        if !task.result_refs.contains(&result_ref) {
            task.result_refs.push(result_ref)
        }
        if !task.active_results.contains(&result_ref) { task.active_results.push(result_ref); }
        task.completed.push(step.key());
        if single_call_mode() {
            task.actions.clear();
            task.pending_candidates.clear();
            return Ok(());
        }
        let extraction = match extraction {
            Ok(e)
                if body["result_truncated"] != true && missing.is_empty() && !capacity_exceeded =>
            {
                e
            }
            _ => {
                self.fail_step(task, step, json!({"code":"incomplete_source","message":"The source response is incomplete; dependent calls were not made."}))?;
                return Ok(());
            }
        };
        if extraction.picked_out_of.is_some() {
            self.fail_step(task, step, json!({"code":"incomplete_selection","message":"The source did not return all candidates; dependent calls were not made."}))?;
            return Ok(());
        }
        let ambiguous = extraction.is_ambiguous();
        // A completed search is a result in its own right. Only a remaining
        // dependency needs the user to disambiguate before spending again.
        let plan: Plan = serde_json::from_value(task.proposal.clone().ok_or_else(|| anyhow::anyhow!("missing plan"))?)?;
        let needs_choice = ambiguous && plan.steps.iter().any(|s| s.entity_ref == step.entity_ref && !task.completed.contains(&s.key()));
        let mut candidate_ids = vec![];
        for subject in &extraction.subjects {
            let entity = if ambiguous {
                Uuid::new_v4()
            } else {
                step.entity_ref
            };
            if ambiguous {
                let label = subject.facts.iter()
                    .find(|f| f.fact_type == "company.name")
                    .and_then(|f| f.value.as_str()).map(str::to_owned)
                    .or_else(|| subject.identity.as_ref().map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string())))
                    .unwrap_or_else(|| format!("Kandidaat {}", subject.index + 1));
                task.entities.push(Entity {
                    entity_ref: entity,
                    label: label.clone(),
                    selected: false,
                });
                if needs_choice { task.pending_candidates.push(json!({"entity_ref":entity,"label":label,"facts":subject.facts.iter().map(|f|json!({"type":f.fact_type,"value":f.value})).collect::<Vec<_>>() })); }
                candidate_ids.push(entity);
            }
            for f in &subject.facts {
                task.facts.push(Fact{entity_ref:entity,semantic_type:f.fact_type.clone(),value:f.value.clone(),provenance:json!({"kind":"provider_response","result_ref":result_ref,"source_pointer":f.extraction_base,"contract_id":step.service.id}),observed_at:Utc::now(),expires_at:Utc::now()+chrono::Duration::seconds(step.service.freshness_seconds.unwrap_or(3600).clamp(0,86400)),verification:"source_observed".into()});
            }
        }
        if needs_choice {
            task.original_subject = Some(step.entity_ref);
            task.actions = vec![serde_json::to_value(Action::Choose {
                action_id: Uuid::new_v4(),
                candidates: candidate_ids,
            })?];
        } else {
            self.advance(task)?;
        }
        Ok(())
    }
}
