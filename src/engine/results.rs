use super::*;

impl Engine {
    pub(super) async fn result_page(
        &self,
        owner: Uuid,
        task: Uuid,
        result_ref: Uuid,
        offset: usize,
    ) -> anyhow::Result<Value> {
        let data = self.store.result(owner, task, result_ref).await?;
        let sources = self.catalog.sources().await.unwrap_or_default();
        self.project_result_page(owner, task, result_ref, offset, data, &sources)
    }

    pub(super) fn project_result_page(
        &self,
        owner: Uuid,
        task: Uuid,
        result_ref: Uuid,
        offset: usize,
        mut data: Value,
        sources: &[crate::sources::Source],
    ) -> anyhow::Result<Value> {
        data = crate::report_assets::public_document(&data);
        data["result_ref"] = json!(result_ref);
        if crate::annual_report::eligible(&data) {
            let expires = (Utc::now() + chrono::Duration::hours(24)).timestamp();
            let signature = self
                .signer
                .report_signature(owner, task, result_ref, expires);
            data["report"] = json!({"format":"pdf", "filename":format!("jaarrekening-{result_ref}.pdf"), "expires_at":expires,
                "download_path":format!("/v2/tasks/{task}/results/{result_ref}/report.pdf?owner={owner}&expires={expires}&signature={signature}")});
        }
        crate::sources::brand_result(&mut data, sources);
        let serialized = serde_json::to_string(&data)?;
        if serialized.len() <= MAX_INLINE {
            return Ok(data);
        }
        let chars: Vec<char> = serialized.chars().collect();
        anyhow::ensure!(offset <= chars.len(), "invalid result offset");
        let end = (offset + 8000).min(chars.len());
        let preview = crate::source_preview::project(
            data["source"]["provider"].as_str().unwrap_or(""),
            &data["data"],
        );
        Ok(
            json!({"result_ref":result_ref,"encoding":"json_text","preview_data":preview,"coverage":data.get("coverage"),"coverage_note":data.get("coverage_note"),"subject":data.get("subject"),"source":data.get("source"),"report":data.get("report"),"text":chars[offset..end].iter().collect::<String>(),"next_offset":if end<chars.len(){Some(end)}else{None}}),
        )
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
        let incomplete_sirenic_search = step.service.api_slug == "sirenic"
            && matches!(
                step.service.path.as_str(),
                "/v1/recherche" | "/v1/eu/recherche"
            )
            && crate::source_preview::sirenic_search_incomplete(&data);
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
        if step.service.api_slug == "overheid-io"
            && step.service.path == "/bag"
            && data["_links"]["next"]["href"].is_string()
        {
            document["coverage"] = json!("partial");
            document["coverage_note"] = json!("One page of matching addresses. The total includes matches on later pages; address counts are not population counts.");
        }
        if matches!(
            step.service.api_slug.as_str(),
            "koop"
                | "rdw"
                | "rvo"
                | "ted"
                | "pdok"
                | "vies"
                | "eurlex-cellar"
                | "eu-funding-tenders"
                | "pdok-ruimtelijke-plannen"
                | "bodemloket"
        ) && data["coverage"]["complete"] == false
        {
            document["coverage"] = json!("partial");
            document["coverage_note"] = data["coverage"]["scope"].clone();
        }
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
            Ok(e) => missing_required_outputs(&step.service, e, &data),
            Err(_) => vec!["output_schema_mismatch".into()],
        };
        let missing_required = !missing.is_empty();
        if confirmed_empty_search(&step.service, &data) && !missing_required {
            document["coverage"] = json!("empty");
        }
        if !missing.is_empty() {
            document["missing_fields"] = json!(missing);
            document["coverage"] = json!("partial");
        }
        if body["result_truncated"] == true {
            document["coverage"] = json!("partial");
            document["missing_fields"] = json!(["upstream_result_truncated"]);
        }
        if incomplete_sirenic_search {
            document["coverage"] = json!("partial");
            document["coverage_note"] = json!("Sirenic reported an incomplete or unreliable search; dependent purchases were stopped.");
        }
        if capacity_exceeded {
            document["coverage"] = json!("partial");
            document["missing_fields"] = json!(["automatic_fact_binding_capacity_exceeded"]);
        }
        if task
            .conversation
            .last()
            .is_some_and(|t| crate::report_assets::requested(&t.question))
        {
            let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM results WHERE execution_id=$1 AND owner_id=$2 AND task_id=$3)")
                .bind(execution).bind(p.owner).bind(task_id).fetch_one(&self.store.pool).await?;
            if !exists {
                document[crate::report_assets::KEY] =
                    Box::pin(crate::report_assets::capture(&document)).await;
            }
        }
        let result_ref:Uuid=sqlx::query_scalar("INSERT INTO results(id,owner_id,task_id,execution_id,document) VALUES($1,$2,$3,$4,$5) ON CONFLICT(execution_id) DO UPDATE SET execution_id=EXCLUDED.execution_id RETURNING id")
            .bind(Uuid::new_v4()).bind(p.owner).bind(task_id).bind(execution).bind(&document).fetch_one(&self.store.pool).await?;
        if !task.result_refs.contains(&result_ref) {
            task.result_refs.push(result_ref)
        }
        if !task.active_results.contains(&result_ref) {
            task.active_results.push(result_ref);
        }
        task.completed.push(step.key());
        if single_call_mode() {
            task.actions.clear();
            task.pending_candidates.clear();
            return Ok(());
        }
        let extraction = match extraction {
            Ok(e)
                if body["result_truncated"] != true
                    && !missing_required
                    && !capacity_exceeded
                    && !incomplete_sirenic_search =>
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
        let selected = if extraction.is_ambiguous() {
            crate::entity_resolution::confident_candidate(
                task,
                step.entity_ref,
                &extraction.subjects,
            )
        } else {
            None
        };
        let ambiguous = extraction.is_ambiguous() && selected.is_none();
        // A completed search is a result in its own right. Only a remaining
        // dependency needs the user to disambiguate before spending again.
        let plan: Plan = serde_json::from_value(
            task.proposal
                .clone()
                .ok_or_else(|| anyhow::anyhow!("missing plan"))?,
        )?;
        let needs_choice = ambiguous
            && plan
                .steps
                .iter()
                .any(|s| s.entity_ref == step.entity_ref && !task.completed.contains(&s.key()));
        let mut candidate_ids = vec![];
        for (index, subject) in extraction.subjects.iter().enumerate() {
            if selected.is_some_and(|selected| selected != index) {
                continue;
            }
            let entity = if ambiguous {
                Uuid::new_v4()
            } else {
                step.entity_ref
            };
            if ambiguous {
                let label = subject
                    .facts
                    .iter()
                    .find(|f| {
                        matches!(
                            f.fact_type.as_str(),
                            "company.name" | "location.address_label"
                        )
                    })
                    .and_then(|f| f.value.as_str())
                    .map(str::to_owned)
                    .or_else(|| {
                        subject.identity.as_ref().map(|v| {
                            v.as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| v.to_string())
                        })
                    })
                    .unwrap_or_else(|| format!("Kandidaat {}", subject.index + 1));
                task.entities.push(Entity {
                    entity_ref: entity,
                    label: label.clone(),
                    selected: false,
                });
                if needs_choice {
                    task.pending_candidates.push(json!({"entity_ref":entity,"label":label,"facts":subject.facts.iter().map(|f|json!({"type":f.fact_type,"value":f.value})).collect::<Vec<_>>() }));
                }
                candidate_ids.push(entity);
            }
            for f in &subject.facts {
                if !ambiguous && f.fact_type == "company.name" {
                    // Keep the source's canonical name without creating a
                    // conflicting second name from the original search text.
                    task.facts.retain(|known| {
                        known.entity_ref != entity || known.semantic_type != "company.name"
                    });
                    if let Some(label) = f.value.as_str() {
                        if let Some(e) = task.entities.iter_mut().find(|e| e.entity_ref == entity) {
                            e.label = label.into();
                        }
                    }
                }
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

// Optional projections describe alternative record shapes (for example, the
// national identifier for each country). Their absence is not partial data.
fn missing_required_outputs(
    service: &crate::catalog::Service,
    extraction: &catalog_core::facts::contract::Extraction,
    data: &Value,
) -> Vec<String> {
    // A confirmed empty search has no address fields to extract. It must not
    // be reported as a broken response or used to fabricate dependent facts.
    if confirmed_empty_search(service, data)
        && extraction.subjects.is_empty()
        && extraction.rejected.is_empty()
    {
        return Vec::new();
    }
    service
        .contract
        .output_map
        .produces
        .keys()
        .filter(|kind| {
            !service.optional_outputs.contains(kind)
                && !extraction
                    .subjects
                    .iter()
                    .any(|subject| subject.facts.iter().any(|fact| fact.fact_type == **kind))
        })
        .cloned()
        .collect()
}

fn confirmed_empty_search(service: &crate::catalog::Service, data: &Value) -> bool {
    service.api_slug == "overheid-io"
        && service.path == "/bag"
        && data["totalItemCount"].as_u64() == Some(0)
        && data["size"].as_u64().is_some_and(|size| size > 0)
        && data["_links"]["self"]["href"]
            .as_str()
            .is_some_and(|href| href.starts_with("/bag?"))
        && (data.get("_embedded").is_none()
            || data["_embedded"]["adres"]
                .as_array()
                .is_some_and(|rows| rows.is_empty()))
}

#[cfg(test)]
mod completeness_tests {
    use super::*;
    #[test]
    fn foreign_optional_ids_do_not_make_a_complete_record_partial() {
        let catalog = Catalog::build(
            serde_json::from_str(include_str!(
                "../../tests/fixtures/sirenic/expanded-catalog.json"
            ))
            .unwrap(),
        );
        let service = catalog
            .services
            .iter()
            .find(|s| s.path == "/v1/eu/recherche")
            .unwrap();
        let result = json!({"resultats":[{"pays":"NO","id_national":"923609016","denomination":"EQUINOR ASA"}]});
        let mut extraction = service
            .contract
            .extract(&result, &catalog.vocabulary)
            .unwrap();
        assert!(extraction.rejected.is_empty());
        assert!(missing_required_outputs(service, &extraction, &result).is_empty());
        extraction.subjects[0]
            .facts
            .retain(|fact| fact.fact_type != "company.name");
        assert_eq!(
            missing_required_outputs(service, &extraction, &result),
            vec!["company.name"]
        );
    }

    #[test]
    fn empty_bag_search_does_not_claim_missing_address_fields() {
        let snapshot: crate::catalog::Snapshot = serde_json::from_str(include_str!(
            "../../tests/fixtures/overheid-bag/catalog.json"
        ))
        .unwrap();
        let catalog = Catalog::build(snapshot);
        let mut service = catalog.services[0].clone();
        let sample = service.sample_payload.as_ref().unwrap();
        let extracted = service
            .contract
            .extract(sample, &catalog.vocabulary)
            .unwrap();
        assert!(!extracted.subjects.is_empty());
        assert!(missing_required_outputs(&service, &extracted, sample).is_empty());
        let empty = json!({"totalItemCount":0,"size":100,"pageCount":1,"_links":{"self":{"href":"/bag?query=9999*&queryfields[0]=postcode"}}});
        let extraction = service
            .contract
            .extract(&empty, &catalog.vocabulary)
            .unwrap();
        assert!(missing_required_outputs(&service, &extraction, &empty).is_empty());
        for malformed in [
            json!({}),
            json!({"totalItemCount":0}),
            json!({"totalItemCount":0,"size":100,"_links":{"self":{"href":"/bag?x=y"}},"_embedded":{"adres":[{"postcode":"3011HA"}]}}),
        ] {
            assert!(!confirmed_empty_search(&service, &malformed));
            assert!(!missing_required_outputs(&service, &extraction, &malformed).is_empty());
        }
        service.api_slug = "another-source".into();
        assert!(!missing_required_outputs(&service, &extraction, &empty).is_empty());
    }
}
