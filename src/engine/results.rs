use super::*;

impl Engine {
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
        let mut document = json!({"data":data,"source":{"provider":step.service.api_slug,"url":step.service.source_url,"endpoint_id":step.service.endpoint_id,"contract_version":step.service.contract.version,"retrieved_at":Utc::now()},"charged_atomic":body["charged_micro_usd"].as_i64().unwrap_or(0).to_string(),"currency":"USDC","missing_fields":[],"coverage":"source_response"});
        document["execution_id"] = json!(execution);
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
        task.completed.push(step.key());
        let extraction = match extraction {
            Ok(e)
                if body["result_truncated"] != true && missing.is_empty() && !capacity_exceeded =>
            {
                e
            }
            _ => {
                task.cancelled = true;
                task.actions.clear();
                return Ok(());
            }
        };
        if extraction.picked_out_of.is_some() {
            task.cancelled = true;
            task.actions.clear();
            return Ok(());
        }
        let ambiguous = extraction.is_ambiguous();
        let mut candidate_ids = vec![];
        for subject in &extraction.subjects {
            let entity = if ambiguous {
                Uuid::new_v4()
            } else {
                step.entity_ref
            };
            if ambiguous {
                let label = subject
                    .identity
                    .as_ref()
                    .map(Value::to_string)
                    .unwrap_or_else(|| format!("Kandidaat {}", subject.index + 1));
                task.entities.push(Entity {
                    entity_ref: entity,
                    label: label.clone(),
                    selected: false,
                });
                task.pending_candidates.push(json!({"entity_ref":entity,"label":label,"facts":subject.facts.iter().map(|f|json!({"type":f.fact_type,"value":f.value})).collect::<Vec<_>>() }));
                candidate_ids.push(entity);
            }
            for f in &subject.facts {
                task.facts.push(Fact{entity_ref:entity,semantic_type:f.fact_type.clone(),value:f.value.clone(),provenance:json!({"kind":"provider_response","result_ref":result_ref,"source_pointer":f.extraction_base,"contract_id":step.service.id}),observed_at:Utc::now(),expires_at:Utc::now()+chrono::Duration::seconds(step.service.freshness_seconds.unwrap_or(3600).clamp(0,86400)),verification:"source_observed".into()});
            }
        }
        if ambiguous {
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
