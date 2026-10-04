use super::*;
use catalog_core::facts::contract::{ContractError, Extraction, SubjectFacts};

/// The provider's own response, trimmed to its shape: three items per array,
/// 600 characters per string, 60 keys per object, nine levels.
fn evidence(value: &Value, depth: usize) -> Value {
    match value {
        Value::String(s) if s.chars().count() > 600 => Value::String(s.chars().take(600).collect()),
        Value::Array(items) if depth <= 8 => Value::Array(items.iter().take(3).map(|v| evidence(v, depth + 1)).collect()),
        Value::Array(_) => json!([]),
        Value::Object(map) if depth <= 8 => Value::Object(map.iter().take(60).map(|(k, v)| (k.clone(), evidence(v, depth + 1))).collect()),
        Value::Object(_) => json!({}),
        other => other.clone(),
    }
}

#[cfg(test)]
mod page_metadata_tests {
    use super::*;
    #[tokio::test]
    async fn paged_saved_result_preserves_exact_charge_and_missing_fields() {
        let pool=sqlx::postgres::PgPoolOptions::new().connect_lazy("postgresql://fixture:fixture@127.0.0.1:1/fixture").unwrap();
        let store=Store{pool:pool.clone()};let http=reqwest::Client::new();
        let engine=Engine{store:store.clone(),catalog:CatalogCache::new(pool),parser:Parser::new(http.clone(),"http://127.0.0.1:1".into(),"fixture".into(),"fixture".into()),
            signer:Signer::new(vec![7;32]).unwrap(),executor:Executor{store,http,supabase_url:"http://127.0.0.1:1".into(),enabled:false},app_url:"https://example.test".into()};
        let owner=Uuid::new_v4();let task=Uuid::new_v4();let result=Uuid::new_v4();
        let document=json!({"source":{"provider":"companies-house","path":"/company/{company_number}/officers"},"coverage":"source_response",
            "charged_atomic":"54348","currency":"USD","missing_fields":["some_optional_field"],
            "data":{"items":(0..82).map(|i|json!({"name":format!("Officiër {i}"),"padding":"é".repeat(1000)})).collect::<Vec<_>>()}});
        let mut text=String::new();
        let mut offset=0;
        let mut pages=0;
        loop {
            let page=engine.project_result_page(owner,task,result,offset,document.clone(),&[]).unwrap();
            assert_eq!(page["encoding"],"json_text");
            assert_eq!(page["result_ref"],json!(result));
            for key in ["charged_atomic","currency","missing_fields"] {assert_eq!(page[key],document[key],"{key}");}
            text.push_str(page["text"].as_str().unwrap());
            pages+=1;
            let Some(next)=page["next_offset"].as_u64() else {break};
            assert!(next as usize>offset,"page offsets must advance");
            offset=next as usize;
        }
        assert!(pages>2);
        let rebuilt:Value=serde_json::from_str(&text).unwrap();
        assert_eq!(rebuilt["data"],document["data"],"all rows and Unicode source text survive paging");
        assert_eq!(rebuilt["data"]["items"][81]["name"],"Officiër 81");
        assert!(engine.project_result_page(owner,task,result,text.chars().count()+1,document,&[]).is_err());
    }
}

impl Engine {
    /// A first live call to an unverified source settles it: a usable
    /// response promotes the contract with that response as evidence; a
    /// failure is counted, and the second one withdraws it. Never fails the run.
    async fn settle_unverified(&self, service: &crate::catalog::Service, usable: Option<&Value>) {
        let outcome = match usable {
            Some(data) => sqlx::query("SELECT catalog.promote_source_operation($1,$2)")
                .bind(service.id).bind(evidence(data, 0)).execute(&self.store.pool).await,
            None => sqlx::query("SELECT catalog.record_source_operation_failure($1)")
                .bind(service.id).execute(&self.store.pool).await,
        };
        if let Err(error) = outcome {
            tracing::warn!(contract = %service.id, error = %error, "unverified source outcome not recorded");
        }
    }

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
        // Captured maps are shown inline in chat; the bytes stay server-side
        // behind a scoped, expiring link, like the PDF report.
        let expires = (Utc::now() + chrono::Duration::hours(24)).timestamp();
        if let Some(assets) = data[crate::report_assets::KEY]["assets"].as_array_mut() {
            for (index, asset) in assets.iter_mut().enumerate() {
                if asset["kind"] == "map" && asset["mime_type"] == "image/png" {
                    let signature = self.signer.asset_signature(owner, task, result_ref, index, expires);
                    asset["download_path"] = json!(format!("/v2/tasks/{task}/results/{result_ref}/assets/{index}?owner={owner}&expires={expires}&signature={signature}"));
                }
            }
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
            json!({"result_ref":result_ref,"encoding":"json_text","preview_data":preview,"coverage":data.get("coverage"),"coverage_note":data.get("coverage_note"),"subject":data.get("subject"),"source":data.get("source"),"report":data.get("report"),
                "charged_atomic":data.get("charged_atomic"),"currency":data.get("currency"),"missing_fields":data.get("missing_fields"),
                "text":chars[offset..end].iter().collect::<String>(),"next_offset":if end<chars.len(){Some(end)}else{None}}),
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
        request_inputs: &Value,
        catalog: &Catalog,
    ) -> anyhow::Result<()> {
        if step.service.unverified && body["ok"] != true {
            self.settle_unverified(&step.service, None).await;
        }
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
        if step.service.unverified {
            // The document adapter yields its fact only for a usable body: not
            // an upstream error, a pending job or an empty response.
            let usable = extraction.as_ref().is_ok_and(|e| e.rejected.is_empty()
                && e.subjects.iter().any(|s| !s.facts.is_empty()));
            self.settle_unverified(&step.service, usable.then_some(&data)).await;
        }
        let capacity_exceeded = extraction.as_ref().is_ok_and(|e| capacity_exceeded(task, e, &data));
        let mut document = json!({"data":data,"source":step.service.result_source(),"charged_atomic":body["charged_micro_usd"].as_i64().unwrap_or(0).to_string(),"currency":"USD","missing_fields":[],"coverage":"source_response"});
        document["execution_id"] = json!(execution);
        document["subject"] = json!({"entity_ref":step.entity_ref,"label":task.entities.iter().find(|e| e.entity_ref == step.entity_ref).map(|e| &e.label)});
        document["source"]["path"] = json!(step.service.path);
        crate::tenderned::record_request(&step.service, request_inputs, &mut document);
        if step.service.is_document_retrieval() {
            document["coverage_note"] = json!("Document returned by the named source. Provider assertions and generated summaries are not independently verified; cite original filings/records only when the returned evidence identifies them. This retrieval does not establish exhaustive historical coverage.");
            document["source"]["output_kind"] = json!("source_document");
        }
        if let Some(fallback) = &step.browser_fallback {
            document["source"]["fallback_for"] = json!(fallback.after.iter().filter(|key|task.failed_steps.contains_key(*key)).collect::<Vec<_>>());
            document["source"]["search_query"] = json!(crate::browser_fallback::query(fallback, task));
            document["coverage_note"] = json!("Browser search for alternative evidence. Search results do not establish official filing status, ownership title or completed screening.");
        }
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
        let mut missing: Vec<String> = match &extraction {
            Ok(e) => missing_required_outputs(&step.service, e, &data),
            Err(_) => vec!["output_schema_mismatch".into()],
        };
        if let Some(gap) = crate::company_name_request::query_identity_gap(&step.service, request_inputs, &data) {
            missing.push(gap.into());
        }
        let missing_required = !missing.is_empty();
        let truncated = body["result_truncated"] == true;
        // A remaining call that needs one of these rows continues on the
        // person's choice among the distinct ones, however many the source
        // returned or the task could hold: only the listed ones are saved.
        let plan = task.proposal.clone().and_then(|plan| serde_json::from_value::<Plan>(plan).ok());
        let choices = if missing_required || incomplete_sirenic_search { Vec::new() } else { choices(task, plan.as_ref(), step, &extraction) };
        let empty_records = crate::browser_fallback::empty_records(step, &data);
        if empty_records { document["coverage"] = json!("empty"); }
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
        if capacity_exceeded && choices.is_empty() {
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
        if empty_records {
            self.fail_step(task, step, json!({"code":"empty_source","message":"The source returned no usable records for this check. Any quoted browser fallback can search for alternative evidence."}))?;
            return Ok(());
        }
        anyhow::ensure!(plan.is_some(), "missing plan");
        let complete = !truncated && !missing_required && !capacity_exceeded && !incomplete_sirenic_search;
        let (subjects, ambiguous, needs_choice) = match rows(task, step, extraction, choices, complete, truncated, &data) {
            Rows::Incomplete => {
                self.fail_step(task, step, json!({"code":"incomplete_source","message":"The source response is incomplete; dependent calls were not made."}))?;
                return Ok(());
            }
            Rows::IncompleteSelection => {
                self.fail_step(task, step, json!({"code":"incomplete_selection","message":"The source did not return all candidates; dependent calls were not made."}))?;
                return Ok(());
            }
            Rows::Bind(subjects) => (subjects, false, false),
            Rows::Evidence(subjects) => (subjects, true, false),
            Rows::Choose(subjects, note) => {
                task.candidate_note = note;
                (subjects, true, true)
            }
        };
        let mut candidate_ids = vec![];
        for subject in &subjects {
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
                            "company.name" | "location.address_label" | "web.title" | "web_tools.title"
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
                    .unwrap_or_else(|| format!("Result {}", subject.index + 1));
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

/// Saving every row as its own subject would outgrow the task.
fn capacity_exceeded(task: &Task, extraction: &Extraction, data: &Value) -> bool {
    task.entities.len() + extraction.subjects.len() > 100
        || task.facts.len() + extraction.subjects.iter().map(|s| s.facts.len()).sum::<usize>() > 1000
        || serde_json::to_vec(data).is_ok_and(|b| b.len() > 512 * 1024)
}

/// The distinct rows another call of the plan could continue with.
fn choices(task: &Task, plan: Option<&Plan>, step: &resolver::Step, extraction: &Result<Extraction, ContractError>) -> Vec<SubjectFacts> {
    match (extraction, plan) {
        (Ok(e), Some(plan)) if e.picked_out_of.is_none() => {
            let types = e.subjects.iter().flat_map(|s| s.facts.iter().map(|f| f.fact_type.as_str())).collect();
            let mut candidates = selection::distinct_candidates(&e.subjects, &selection::needed_facts(task, plan, step, &types));
            if step.service.api_slug == "pdok" && step.service.path == "/apiosk/bag-address"
                && step.service.contract.capability_slug == "location.bag-address-context"
                && e.subjects.len() > 1 && candidates.len() == 1 {
                // One BAG unit can have several address aliases in the same
                // neighbourhood. Continue with their common facts, without
                // rebinding the selected address to whichever alias came first.
                candidates[0].facts.retain(|fact| e.subjects.iter().all(|subject|
                    subject.fact(&fact.fact_type).is_some_and(|other| other.value == fact.value)));
            }
            candidates
        }
        _ => Vec::new(),
    }
}

/// What a source step's rows do next.
#[derive(Debug)]
enum Rows {
    /// The subject's own facts: no row, one row, or the one sure match.
    Bind(Vec<SubjectFacts>),
    /// Several rows kept as evidence; no remaining call needs one of them.
    Evidence(Vec<SubjectFacts>),
    /// The person picks one; the note says when these are not every match.
    Choose(Vec<SubjectFacts>, Option<String>),
    Incomplete,
    IncompleteSelection,
}

/// A completed search is a result in its own right. Only a remaining
/// dependency needs the person to disambiguate before spending again, and
/// then among the distinct `choices` only, however many rows the source sent
/// or the task can hold. A name match must not silently choose an identifier
/// for another paid call. Only one distinct match in a complete list can
/// continue by itself.
fn rows(task: &Task, step: &resolver::Step, extraction: Result<Extraction, ContractError>, mut choices: Vec<SubjectFacts>,
    complete: bool, truncated: bool, data: &Value) -> Rows {
    let extraction = match extraction {
        Ok(e) if complete || !choices.is_empty() => e,
        _ => return Rows::Incomplete,
    };
    if extraction.picked_out_of.is_some() {
        return Rows::IncompleteSelection;
    }
    if !choices.is_empty() {
        let address_search = pdok_address_search(&step.service);
        let address_complete = !address_search || pdok_address_complete(data);
        let more = more_pages(&step.service, data) || !address_complete;
        // PDOK's free search is fuzzy. One returned BAG ID is not proof that
        // it belongs to the supplied street, number, suffix and locality.
        if address_search && complete && !truncated && !more && extraction.rejected.is_empty() {
            if let Some(index) = exact_pdok_address(task, step, data, &choices) {
                return Rows::Bind(vec![choices.remove(index)]);
            }
        }
        if !address_search && choices.len() == 1 && !truncated && !more {
            return Rows::Bind(choices);
        }
        let listed = selection::offered(task, &choices);
        if listed == 0 {
            return Rows::Incomplete;
        }
        let note = (truncated || listed < choices.len() || more).then(|| format!(
            "{} returned more matches than are listed. Only the first {listed} distinct {} shown; if yours is not here, ask again with a more specific name or its identifier. Nothing more is charged until you choose.",
            step.service.name, if listed == 1 { "match is" } else { "matches are" }))
            .or_else(|| address_search.then(|| "The address search did not establish one exact match for the supplied address. Check the listed addresses and choose the intended one. Nothing more is charged until you choose.".into()));
        let note = if address_search && !address_complete {
            Some("PDOK did not confirm that every matching address was returned. Check the listed addresses before continuing; if yours is absent, use a more specific address. Nothing more is charged until you choose.".into())
        } else { note };
        choices.truncate(listed);
        return Rows::Choose(choices, note);
    }
    if pdok_address_search(&step.service) {
        if complete && !truncated && pdok_address_complete(data) && extraction.rejected.is_empty() {
            if let Some(index) = exact_pdok_address(task, step, data, &extraction.subjects) {
                return Rows::Bind(vec![extraction.subjects[index].clone()]);
            }
        }
        // A standalone search may display its hits without asking a choice,
        // but must not save a fuzzy hit as the requested address's identity.
        return Rows::Evidence(extraction.subjects);
    }
    if !extraction.is_ambiguous() {
        return Rows::Bind(extraction.subjects);
    }
    match crate::entity_resolution::confident_candidate(task, step.entity_ref, &extraction.subjects) {
        Some(index) => Rows::Bind(vec![extraction.subjects[index].clone()]),
        None => Rows::Evidence(extraction.subjects),
    }
}

fn pdok_address_search(service: &crate::catalog::Service) -> bool {
    service.api_slug == "pdok"
        && service.path == "/bzk/locatieserver/search/v3_1/free"
        && service.contract.capability_slug == "location.property-search"
}

fn pdok_address_complete(data: &Value) -> bool {
    data["response"]["docs"].as_array().is_some_and(|docs|
        data["response"]["numFound"].as_u64() == Some(docs.len() as u64)
            && data["response"].get("start").is_none_or(|start| start.as_u64() == Some(0))
            && data["response"].get("numFoundExact").is_none_or(|exact| exact == true))
}

/// Only explicit address components can select a fuzzy search hit. Punctuation
/// and case may vary; missing locality, different numbers and suffixes cannot.
/// Match the same source row that supplied the candidate's identifier and label.
fn exact_pdok_address(task: &Task, step: &resolver::Step, data: &Value, candidates: &[SubjectFacts]) -> Option<usize> {
    let query = crate::catalog::fact_value(&task.facts, step.entity_ref, "geo.input.q").ok()??;
    let query = query.as_str()?;
    let docs = data["response"]["docs"].as_array()?;
    let mut matching = candidates.iter().enumerate().filter(|(_, candidate)| {
        let Some(id) = candidate.fact("location.bag_object_id").and_then(|fact| fact.value.as_str()) else { return false };
        let Some(label) = candidate.fact("location.address_label").and_then(|fact| fact.value.as_str()) else { return false };
        docs.iter().any(|row| row["adresseerbaarobject_id"].as_str() == Some(id)
            && row["weergavenaam"].as_str() == Some(label) && pdok_literal_address(query, row))
    });
    let (index, _) = matching.next()?;
    matching.next().is_none().then_some(index)
}

fn pdok_literal_address(query: &str, row: &Value) -> bool {
    fn normalized(value: &str) -> String {
        value.to_lowercase().split(|ch: char| !ch.is_alphanumeric())
            .filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" ")
    }
    if row["type"] != "adres" { return false; }
    let (Some(street), Some(city), Some(number)) =
        (row["straatnaam"].as_str(), row["woonplaatsnaam"].as_str(), row["huisnummer"].as_u64())
        else { return false };
    if street.trim().is_empty() || city.trim().is_empty() { return false; }
    let suffix = |key: &str| match row.get(key) {
        None | Some(Value::Null) => Some(""),
        Some(Value::String(value)) => Some(value.trim()),
        _ => None,
    };
    let (Some(letter), Some(addition)) = (suffix("huisletter"), suffix("huisnummertoevoeging")) else { return false };
    let number = number.to_string();
    let houses = [format!("{number}{letter} {addition}"), format!("{number} {letter} {addition}")];
    let query = normalized(query);
    let postcode = row["postcode"].as_str().map(|value| value.chars().filter(|ch| !ch.is_whitespace()).collect::<String>());
    let postcode = postcode.filter(|value| value.len() == 6 && value.as_bytes()[..4].iter().all(u8::is_ascii_digit)
        && value.as_bytes()[4..].iter().all(u8::is_ascii_alphabetic));
    houses.iter().any(|house| {
        query == normalized(&format!("{street} {house} {city}")) || postcode.as_ref().is_some_and(|postcode| {
            [postcode.clone(), format!("{} {}", &postcode[..4], &postcode[4..])].iter().any(|postcode|
                query == normalized(&format!("{street} {house} {postcode} {city}"))
                    || query == normalized(&format!("{street} {house} {postcode}")))
        })
    })
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

/// The page links to a next one, by the contract's own pagination path.
fn more_pages(service: &crate::catalog::Service, data: &Value) -> bool {
    // Older KVK catalog snapshots omit pagination metadata. The official
    // search response still declares its next page in `volgende`.
    if service.api_slug == "kvk-dutch-business-register" && service.path == "/api/v2/zoeken"
        && data["volgende"].as_str().is_some_and(|link| !link.is_empty()) {
        return true;
    }
    service.pagination["next"].as_str()
        .and_then(|next| catalog_core::facts::path::parse(next).ok())
        .is_some_and(|path| path.select(data).iter().any(|link| link.as_str().is_some_and(|link| !link.is_empty())))
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

    fn repaired_kvk_catalog() -> Catalog {
        let mut snapshot: crate::catalog::Snapshot = serde_json::from_str(include_str!(
            "../../tests/fixtures/research-report-catalog.json"
        )).unwrap();
        let service = snapshot.services.iter_mut().find(|s| s.path == "/api/v1/basisprofielen").unwrap();
        // Execute the exact output definition reviewed in the versioned repair.
        let migration = include_str!("../../supabase/migrations/20261003203509_support_kvk_sole_trader_profiles.sql");
        service.contract.output_map = serde_json::from_str(migration.split("$repaired_output$").nth(1).unwrap()).unwrap();
        service.contract.version = 8;
        service.optional_outputs.push("company.profile".into());
        Catalog::build(snapshot)
    }

    #[test]
    fn kvk_sole_trader_without_statutory_name_is_a_complete_source_record() {
        let catalog = repaired_kvk_catalog();
        let service = catalog.services.iter().find(|s| s.path == "/api/v1/basisprofielen").unwrap();
        // Minimal observed sole-trader shape: no statutory name and no RSIN.
        let data = json!({"kvkNummer":"87560488","naam":"Bas Grave Web Development",
            "handelsnamen":[{"naam":"Bas Grave Web Development"},{"naam":"Onderstreept"}],
            "_embedded":{"eigenaar":{"rechtsvorm":"Eenmanszaak"}},
            "sbiActiviteiten":[{"sbiCode":"62100"}],"totaalWerkzamePersonen":1});
        let extracted = service.contract.extract(&data, &catalog.vocabulary).unwrap();
        assert!(missing_required_outputs(service, &extracted, &data).is_empty());
        let subject = extracted.single().unwrap();
        assert_eq!(subject.fact("company.name").unwrap().value, "Bas Grave Web Development");
        assert_eq!(subject.fact("company_registry.kvknummer").unwrap().value, "87560488");
        // A trade name never silently becomes a legal name or normalized profile.
        assert!(subject.fact("company.nl.registered_name").is_none());
        assert!(subject.fact("company.nl.rsin").is_none());
        assert!(subject.fact("company.profile").is_none());
        assert_eq!(extracted.rejected.len(), 1);
        assert_eq!(extracted.rejected[0].fact_type, "company.profile");
        let step = resolver::Step { service: service.clone(), entity_ref: Uuid::new_v4(), goal_index: 0, browser_fallback: None };
        assert!(matches!(rows(&Task::default(), &step, Ok(extracted), Vec::new(), true, false, &data), Rows::Bind(_)),
            "an optional legal-name projection cannot fail the source step");
    }

    #[test]
    fn kvk_repair_keeps_bv_profile_and_still_rejects_missing_identity_or_name() {
        let catalog = repaired_kvk_catalog();
        let service = catalog.services.iter().find(|s| s.path == "/api/v1/basisprofielen").unwrap();
        let data = service.sample_payload.as_ref().unwrap();
        let extracted = service.contract.extract(data, &catalog.vocabulary).unwrap();
        assert!(missing_required_outputs(service, &extracted, data).is_empty());
        assert!(extracted.rejected.is_empty());
        assert_eq!(extracted.single().unwrap().fact("company.profile").unwrap().value["legal_name"], "Mollie B.V.");
        for (field, value, missing) in [
            ("naam", Value::Null, "company.name"),
            ("naam", json!(""), "company.name"),
            ("naam", json!(["Multiple", "Names"]), "company.name"),
            ("kvkNummer", Value::Null, "company_registry.kvknummer"),
            ("kvkNummer", json!(87560488), "company_registry.kvknummer"),
            ("kvkNummer", json!("8756048"), "company_registry.kvknummer"),
        ] {
            let mut malformed = data.clone();
            malformed[field] = value;
            let extracted = service.contract.extract(&malformed, &catalog.vocabulary).unwrap();
            assert!(missing_required_outputs(service, &extracted, &malformed).contains(&missing.to_owned()), "{field}: {malformed}");
        }
        let mut leading_zero = data.clone();
        leading_zero["kvkNummer"] = json!("00123456");
        let extracted = service.contract.extract(&leading_zero, &catalog.vocabulary).unwrap();
        assert_eq!(extracted.single().unwrap().fact("company_registry.kvknummer").unwrap().value, "00123456");
    }

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

#[cfg(test)]
mod candidate_tests {
    use super::*;

    /// The production OpenKvK search and profile contracts.
    fn catalog() -> Catalog {
        Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/overheid-openkvk/catalog.json")).unwrap())
    }

    /// The reported page: 22 establishments, bol.com's four under one KVK number.
    fn page(rows: usize) -> Value {
        let mut bedrijf = vec![json!({"kvknummer":"63768917","naam":"Bol Holding B.V.","slug":"hoofdvestiging-bol-holding-63768917-000033089418"})];
        for branch in 0..4 {
            let kind = if branch == 0 { "hoofdvestiging" } else { "nevenvestiging" };
            bedrijf.push(json!({"kvknummer":"24330087","naam":"bol.com B.V.","slug":format!("{kind}-bol-com-24330087-0000198177{branch:02}")}));
        }
        for n in 0..rows.saturating_sub(5) {
            bedrijf.push(json!({"kvknummer":format!("{:08}", 70000000 + n),"naam":format!("Bol Winkel {n}"),"slug":format!("hoofdvestiging-bol-winkel-{n}")}));
        }
        bedrijf.truncate(rows);
        json!({"totalItemCount":rows,"pageCount":1,"size":100,"_links":{"self":{"href":"/v3/openkvk?query=bol"}},"_embedded":{"bedrijf":bedrijf}})
    }

    /// A name search followed by the call that needs one of its rows.
    fn plan(search: crate::catalog::Service, dependent: crate::catalog::Service, name: &str) -> (Task, resolver::Step) {
        let subject = Uuid::new_v4();
        let mut task = Task::default();
        task.entities.push(Entity { entity_ref: subject, label: name.into(), selected: false });
        task.facts.push(Fact { entity_ref: subject, semantic_type: "company.name".into(), value: json!(name),
            provenance: json!({"kind":"user"}), observed_at: Utc::now(), expires_at: Utc::now() + chrono::Duration::hours(1), verification: "user".into() });
        let step = |service| resolver::Step { service, entity_ref: subject, goal_index: 0, browser_fallback: None };
        let plan = Plan { quote_ref: Uuid::new_v4(), steps: vec![step(search), step(dependent)], cap_atomic: 200_000,
            expires_at: Utc::now() + chrono::Duration::minutes(10), hash: "fixture".into(), fee_bps: 1500, workflow_slug: None };
        task.proposal = Some(serde_json::to_value(&plan).unwrap());
        (task, plan.steps[0].clone())
    }

    fn openkvk(name: &str) -> (Task, resolver::Step, Catalog) {
        let catalog = catalog();
        let (task, step) = plan(catalog.services[0].clone(), catalog.services[1].clone(), name);
        (task, step, catalog)
    }

    /// The decision `ingest` takes for this response, from the same parts.
    fn decide(task: &Task, step: &resolver::Step, catalog: &Catalog, data: &Value, truncated: bool) -> Rows {
        let extraction = step.service.contract.extract(data, &catalog.vocabulary);
        let missing = extraction.as_ref().map_or(true, |e| !missing_required_outputs(&step.service, e, data).is_empty());
        let capacity = extraction.as_ref().is_ok_and(|e| capacity_exceeded(task, e, data));
        let plan: Plan = serde_json::from_value(task.proposal.clone().unwrap()).unwrap();
        let options = if missing { Vec::new() } else { choices(task, Some(&plan), step, &extraction) };
        rows(task, step, extraction, options, !truncated && !missing && !capacity, truncated, data)
    }

    fn kvk(candidate: &SubjectFacts) -> &str {
        candidate.fact("company_registry.kvknummer").unwrap().value.as_str().unwrap()
    }

    #[test]
    fn a_page_of_companies_is_one_choice_per_kvk_number() {
        let (task, step, catalog) = openkvk("bol");
        let Rows::Choose(candidates, note) = decide(&task, &step, &catalog, &page(22), false) else { panic!("expected a choice") };
        assert_eq!(candidates.len(), 19, "four establishments of 24330087 are one candidate");
        assert_eq!(candidates.iter().filter(|c| kvk(c) == "24330087").count(), 1);
        let bol = candidates.iter().find(|c| kvk(c) == "24330087").unwrap();
        assert_eq!(bol.fact("company_registry.openkvk_slug").unwrap().value, "hoofdvestiging-bol-com-24330087-000019817700", "the first row the source ranked");
        assert_eq!(kvk(&candidates[0]), "63768917", "source order is kept");
        assert!(note.is_none(), "the page held every match");
    }

    #[test]
    fn a_task_near_capacity_offers_what_fits_instead_of_failing() {
        let (mut task, step, catalog) = openkvk("bol");
        // Earlier turns of the same chat left 90 subjects on the task: the
        // automatic binding capacity is exceeded, which used to fail the step.
        for n in 0..89 { task.entities.push(Entity { entity_ref: Uuid::new_v4(), label: format!("Earlier {n}"), selected: false }); }
        let data = page(22);
        let extraction = step.service.contract.extract(&data, &catalog.vocabulary).unwrap();
        assert!(capacity_exceeded(&task, &extraction, &data));
        let Rows::Choose(candidates, note) = decide(&task, &step, &catalog, &data, false) else { panic!("expected a choice") };
        assert_eq!(candidates.len(), 10);
        assert!(note.unwrap().contains("Only the first 10 distinct matches are shown"));
    }

    #[test]
    fn a_long_list_is_bounded_and_says_so() {
        let (task, step, catalog) = openkvk("bol");
        let mut data = page(60);
        let Rows::Choose(candidates, note) = decide(&task, &step, &catalog, &data, false) else { panic!("expected a choice") };
        assert_eq!(candidates.len(), selection::MAX_CANDIDATES);
        assert!(note.unwrap().starts_with("Overheid.io Public Data returned more matches than are listed. Only the first 20 distinct matches"));
        // A further page is also said, even when this page fits the list.
        data = page(3);
        data["_links"]["next"] = json!({"href":"/v3/openkvk?query=bol&page=2"});
        let Rows::Choose(candidates, note) = decide(&task, &step, &catalog, &data, false) else { panic!("expected a choice") };
        assert_eq!(candidates.len(), 2);
        assert!(note.unwrap().contains("Only the first 2 distinct matches"));
    }

    #[test]
    fn one_distinct_company_continues_without_asking() {
        let (task, step, catalog) = openkvk("bol");
        let mut data = page(5);
        data["_embedded"]["bedrijf"].as_array_mut().unwrap().remove(0);
        let Rows::Bind(subjects) = decide(&task, &step, &catalog, &data, false) else { panic!("expected a binding") };
        assert_eq!(subjects.len(), 1);
        assert_eq!(kvk(&subjects[0]), "24330087");
        // Without a declared identity the repeated number is still one company
        // for a call that needs the number.
        let mut search = catalog.services[0].clone();
        search.contract.identifies = None;
        let mut profile = catalog.services[1].clone();
        profile.contract.requires[0].fact_type = "company_registry.kvknummer".into();
        let (task, step) = plan(search, profile, "bol");
        assert_eq!(step.service.contract.extract(&data, &catalog.vocabulary).unwrap().subjects.len(), 4);
        let Rows::Bind(subjects) = decide(&task, &step, &catalog, &data, false) else { panic!("expected a binding") };
        assert_eq!(kvk(&subjects[0]), "24330087");
        let Rows::Choose(candidates, _) = decide(&task, &step, &catalog, &page(22), false) else { panic!("expected a choice") };
        assert_eq!(candidates.len(), 19);
    }

    #[test]
    fn an_exact_name_does_not_choose_between_distinct_identifiers() {
        let (task, step, catalog) = openkvk("bol.com B.V.");
        let Rows::Choose(candidates, _) = decide(&task, &step, &catalog, &page(22), false) else { panic!("expected a choice") };
        assert_eq!(candidates.len(), 19);
        assert_eq!(candidates.iter().filter(|candidate| kvk(candidate) == "24330087").count(), 1);
    }

    #[test]
    fn one_match_on_an_incomplete_page_still_needs_confirmation() {
        let (task, step, catalog) = openkvk("bol.com B.V.");
        let mut data = page(5);
        data["_embedded"]["bedrijf"].as_array_mut().unwrap().remove(0);
        data["_links"]["next"] = json!({"href":"/v3/openkvk?page=2"});
        let Rows::Choose(candidates, note) = decide(&task, &step, &catalog, &data, false) else { panic!("expected a choice") };
        assert_eq!(candidates.len(), 1);
        assert!(note.is_some());
    }

    #[test]
    fn kvk_financials_wait_for_a_distinct_number_even_with_an_exact_name() {
        let catalog = catalog();
        let mut search = catalog.services[0].clone();
        search.api_slug = "kvk-dutch-business-register".into();
        search.name = "KVK Business Register".into();
        search.path = "/api/v2/zoeken".into();
        search.pagination = json!({});
        search.contract.output_map = serde_json::from_value(json!({"base":"$.resultaten[*]", "produces":{
            "company.name":{"path":"$.naam"}, "company_registry.kvknummer":{"path":"$.kvkNummer"}
        }})).unwrap();
        let mut financials = catalog.services[1].clone();
        financials.contract.requires[0].fact_type = "company_registry.kvknummer".into();
        let (task, step) = plan(search, financials, "Coolblue B.V.");
        let mut records = vec![json!({"naam":"Coolblue Verwonderdiensten B.V.","kvkNummer":"63768917"})];
        records.extend((0..9).map(|_| json!({"naam":"Coolblue B.V.","kvkNummer":"24330087"})));
        let data = json!({"pagina":1,"totaal":43,"volgende":"https://api.kvk.nl/api/v2/zoeken?pagina=2","resultaten":records});
        let Rows::Choose(candidates, note) = decide(&task, &step, &catalog, &data, false) else { panic!("expected a KVK choice") };
        assert_eq!(candidates.iter().map(kvk).collect::<Vec<_>>(), ["63768917", "24330087"]);
        assert!(note.unwrap().contains("Only the first 2 distinct matches"));
    }

    #[test]
    fn a_cut_short_list_is_offered_and_never_guessed() {
        let (task, step, catalog) = openkvk("bol.com B.V.");
        let Rows::Choose(candidates, note) = decide(&task, &step, &catalog, &page(22), true) else { panic!("expected a choice") };
        assert_eq!(candidates.len(), 19, "no automatic match from a truncated list");
        assert!(note.is_some());
        let mut single = page(5);
        single["_embedded"]["bedrijf"].as_array_mut().unwrap().remove(0);
        let Rows::Choose(candidates, note) = decide(&task, &step, &catalog, &single, true) else { panic!("expected a confirmation") };
        assert_eq!(candidates.len(), 1);
        assert!(note.unwrap().contains("Only the first 1 distinct match is shown"));
    }

    #[test]
    fn genuinely_incomplete_responses_still_stop_dependent_calls() {
        let (task, step, catalog) = openkvk("bol");
        // Storage kept only a text preview: nothing to choose from.
        let preview = json!({"truncated_preview":"{\"_embedded\":{\"bedrijf\":[","original_bytes":900000});
        assert!(matches!(decide(&task, &step, &catalog, &preview, true), Rows::Incomplete));
        // Rows without the identifier the dependent call needs.
        let mut unusable = page(22);
        for row in unusable["_embedded"]["bedrijf"].as_array_mut().unwrap() { row.as_object_mut().unwrap().remove("slug"); }
        assert!(matches!(decide(&task, &step, &catalog, &unusable, false), Rows::Incomplete));
        // No room left on the task for even one listed match.
        let mut full = task.clone();
        for n in 0..99 { full.entities.push(Entity { entity_ref: Uuid::new_v4(), label: format!("Earlier {n}"), selected: false }); }
        assert!(matches!(decide(&full, &step, &catalog, &page(22), false), Rows::Incomplete));
    }

    #[test]
    fn rows_no_remaining_call_needs_stay_evidence() {
        let (mut task, step, catalog) = openkvk("bol");
        task.completed.push(serde_json::from_value::<Plan>(task.proposal.clone().unwrap()).unwrap().steps[1].key());
        assert!(matches!(decide(&task, &step, &catalog, &page(22), false), Rows::Evidence(subjects) if subjects.len() == 19));
    }

    fn damrak() -> Value {
        serde_json::from_str(include_str!("../../tests/fixtures/dutch-links/damrak-public-chain.json")).unwrap()
    }

    fn pdok_lookup(query: &str) -> (Task, resolver::Step, Catalog) {
        let catalog = crate::ask_v2::test_catalog::locations();
        let service = |capability: &str| catalog.services.iter().find(|service| service.contract.capability_slug == capability).unwrap().clone();
        let (mut task, step) = plan(service("location.property-search"), service("building.bag-unit"), query);
        task.facts[0].semantic_type = "geo.input.q".into();
        (task, step, catalog)
    }

    #[test]
    fn pdok_exact_literal_address_continues_with_its_own_identifier() {
        let data = &damrak()["address"];
        for query in ["Damrak 1, Amsterdam", "damrak 1 amsterdam", "Damrak 1, 1012LG Amsterdam", "Damrak 1, 1012 LG Amsterdam"] {
            let (task, step, catalog) = pdok_lookup(query);
            let Rows::Bind(subjects) = decide(&task, &step, &catalog, data, false) else { panic!("exact address should bind: {query}") };
            assert_eq!(subjects.len(), 1);
            assert_eq!(subjects[0].fact("location.bag_object_id").unwrap().value, "0363010012111931");
            assert_eq!(subjects[0].fact("location.address_label").unwrap().value, "Damrak 1, 1012LG Amsterdam");
        }
    }

    #[test]
    fn pdok_single_fuzzy_or_differently_suffixed_address_needs_a_choice() {
        let data = &damrak()["address"];
        for query in ["Damrak 1A, Amsterdam", "Damrak 1-1, Amsterdam", "Damrak 12, Amsterdam", "Damrak 1, Rotterdam", "Damrak 1, 1012AA Amsterdam", "Damrak 1", "Dam 1, Amsterdam"] {
            let (task, step, catalog) = pdok_lookup(query);
            assert!(matches!(decide(&task, &step, &catalog, data, false), Rows::Choose(_, Some(_))), "must not bind fuzzy address: {query}");
        }
        let mut suffixed = data.clone();
        suffixed["response"]["docs"][0]["huisletter"] = json!("A");
        suffixed["response"]["docs"][0]["huisnummertoevoeging"] = json!("01");
        suffixed["response"]["docs"][0]["weergavenaam"] = json!("Damrak 1A-01, 1012LG Amsterdam");
        for query in ["Damrak 1A-01, Amsterdam", "Damrak 1 A-01, Amsterdam"] {
            let (task, step, catalog) = pdok_lookup(query);
            assert!(matches!(decide(&task, &step, &catalog, &suffixed, false), Rows::Bind(_)), "explicit suffix: {query}");
        }
        let (mut task, step, catalog) = pdok_lookup("Damrak 1, Amsterdam");
        assert!(matches!(decide(&task, &step, &catalog, &suffixed, false), Rows::Choose(_, _)));
        task.facts.clear();
        assert!(matches!(decide(&task, &step, &catalog, data, false), Rows::Choose(_, _)), "entity label cannot replace the supplied query fact");
        task.completed.push(serde_json::from_value::<Plan>(task.proposal.clone().unwrap()).unwrap().steps[1].key());
        assert!(matches!(decide(&task, &step, &catalog, data, false), Rows::Evidence(_)), "standalone fuzzy hits must not bind an identifier for a later request");
    }

    #[test]
    fn pdok_count_completeness_and_unique_exact_candidate_are_required() {
        let (task, step, catalog) = pdok_lookup("Damrak 1, Amsterdam");
        let data = &damrak()["address"];
        for (key, value) in [("numFound", json!(916474)), ("numFound", Value::Null), ("start", json!(1)), ("numFoundExact", json!(false))] {
            let mut incomplete = data.clone();
            incomplete["response"][key] = value;
            assert!(matches!(decide(&task, &step, &catalog, &incomplete, false), Rows::Choose(_, Some(_))), "incomplete count: {key}");
        }
        assert!(matches!(decide(&task, &step, &catalog, data, true), Rows::Choose(_, Some(_))), "truncation never proves identity");
        let mut multiple = data.clone();
        let mut other = multiple["response"]["docs"][0].clone();
        other["adresseerbaarobject_id"] = json!("0363010012111932");
        other["huisletter"] = json!("A");
        other["weergavenaam"] = json!("Damrak 1A, 1012LG Amsterdam");
        multiple["response"]["docs"].as_array_mut().unwrap().insert(0, other);
        multiple["response"]["numFound"] = json!(2);
        let Rows::Bind(subjects) = decide(&task, &step, &catalog, &multiple, false) else { panic!("one fully explicit exact address may exclude a different suffix") };
        assert_eq!(subjects[0].fact("location.bag_object_id").unwrap().value, "0363010012111931");
        multiple["response"]["docs"][0]["huisletter"] = Value::Null;
        multiple["response"]["docs"][0]["weergavenaam"] = json!("Damrak 1, 1012LG Amsterdam");
        assert!(matches!(decide(&task, &step, &catalog, &multiple, false), Rows::Choose(_, _)), "same literal address with distinct IDs is still ambiguous");
        multiple["response"]["docs"][0]["huisletter"] = json!("A");
        multiple["response"]["docs"][0]["weergavenaam"] = json!("Damrak 1A, 1012LG Amsterdam");
        multiple["response"]["numFound"] = json!(3);
        assert!(matches!(decide(&task, &step, &catalog, &multiple, false), Rows::Choose(_, _)), "unseen results cannot be excluded");
    }

    #[test]
    fn pdok_shared_neighbourhood_aliases_preserve_the_selected_address() {
        let catalog = crate::ask_v2::test_catalog::locations();
        let service = |capability: &str| catalog.services.iter().find(|service| service.contract.capability_slug == capability).unwrap().clone();
        let (mut task, step) = plan(service("location.bag-address-context"), service("area.cbs-neighbourhood-2025"), "Damrak 1, 1012LG Amsterdam");
        task.facts[0].semantic_type = "location.address_label".into();
        // The adapter's published output shape, populated from all four real
        // source aliases. Adapter identity validation is covered in dutch_links.
        let mut items: Vec<Value> = damrak()["bag-address"]["response"]["docs"].as_array().unwrap().iter().map(|row| json!({
            "address":row["weergavenaam"], "bag_object_id":row["adresseerbaarobject_id"], "bag_address_id":row["nummeraanduiding_id"],
            "municipality_code":row["gemeentecode"], "municipality":row["gemeentenaam"], "neighbourhood_code":row["buurtcode"],
            "neighbourhood":row["buurtnaam"], "district_code":row["wijkcode"], "parcel_designations":row["gekoppeld_perceel"],
            "apartment_right_designations":row["gekoppeld_appartement"]
        })).collect();
        items.reverse();
        assert_eq!(items.len(), 4);
        let data = json!({"count":4,"matched":null,"items":items,"coverage":{"complete":true}});
        let Rows::Bind(subjects) = decide(&task, &step, &catalog, &data, false) else { panic!("same unit and same neighbourhood need no alias choice") };
        assert_eq!(subjects.len(), 1);
        assert_eq!(subjects[0].fact("location.neighbourhood_code").unwrap().value, "BU0363AD03");
        assert_eq!(subjects[0].fact("location.bag_object_id").unwrap().value, "0363010012111931");
        assert!(subjects[0].fact("location.address_label").is_none(), "no alias may replace the selected address");
        assert!(subjects[0].fact("location.bag_address_id").is_none(), "no alias is assumed to be the primary address");
        assert_eq!(task.facts[0].value, "Damrak 1, 1012LG Amsterdam");
        let mut different_neighbourhood = data;
        different_neighbourhood["items"][0]["neighbourhood_code"] = json!("BU0363AD04");
        assert!(matches!(decide(&task, &step, &catalog, &different_neighbourhood, false), Rows::Choose(_, _)), "different needed facts still require a choice");
    }
}
