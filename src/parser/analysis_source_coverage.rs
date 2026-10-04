//! A multi-source answer must account for each distinct saved provider.
//! This checks coverage only after ordinary citation validation: it never
//! creates facts, treats a retrieved source as a passed check, or buys data.
use anyhow::{ensure, Context};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Gap {
    result_ref: String,
    reason: GapReason,
    limitation_index: usize,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum GapReason {
    NotInSource,
    EntityMismatch,
    PeriodUnavailable,
    OutsideCoverage,
}

fn provider_key(source: &Value) -> String {
    // Several endpoints or codebooks from the same provider are one source.
    // Missing provider metadata must not merge unrelated saved results.
    source["document"]["source"]["provider"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("provider:{}", value.trim().to_lowercase()))
        .unwrap_or_else(|| format!("result:{}", source["result_ref"]))
}

fn meaningful(value: &Value) -> bool {
    value.is_number()
        || value.is_boolean()
        || value.as_str().is_some_and(|text| !text.trim().is_empty())
}

fn cited_refs(checked: &Value) -> BTreeSet<&str> {
    let observations = checked["observations"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|observation| observation["evidence"].as_array().into_iter().flatten());
    let cells = checked["presentation"]["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|row| row.as_array().into_iter().flatten());
    // A source name in a column heading alone does not answer any question.
    observations
        .chain(cells)
        .filter(|fact| {
            meaningful(&fact["value"])
                && fact["pointer"]
                    .as_str()
                    .is_some_and(crate::report_assets::evidence_pointer)
        })
        .filter_map(|fact| fact["result_ref"].as_str())
        .collect()
}

pub(super) fn validate(
    raw: &str,
    checked: &mut Value,
    sources: &[Value],
    failures: &BTreeMap<String, Value>,
) -> anyhow::Result<()> {
    let mut groups: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for source in sources {
        groups.entry(provider_key(source)).or_default().push(source);
    }
    // Preserve the established single-provider answer contract.
    if groups.len() < 2 && failures.is_empty() {
        return Ok(());
    }

    if groups.len() >= 2 {
        let raw: Value = serde_json::from_str(raw)?;
        let gaps: Vec<Gap> = raw
            .get("source_gaps")
            .filter(|value| !value.is_null())
            .map(|value| {
                serde_json::from_value(value.clone()).context("invalid source coverage gap")
            })
            .transpose()?
            .unwrap_or_default();
        ensure!(gaps.len() <= sources.len(), "invalid source coverage gap");
        let mut gaps_by_provider = BTreeMap::new();
        for gap in &gaps {
            let source = sources
                .iter()
                .find(|source| source["result_ref"].as_str() == Some(gap.result_ref.as_str()))
                .context("invalid source coverage gap")?;
            ensure!(
                raw["limitations"]
                    .get(gap.limitation_index)
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.trim().is_empty()),
                "invalid source coverage gap"
            );
            ensure!(
                gaps_by_provider.insert(provider_key(source), gap).is_none(),
                "invalid source coverage gap"
            );
        }
        let cited = cited_refs(checked);
        let mut coverage = Vec::new();
        let mut has_gap = false;
        for (provider, documents) in groups {
            let used: Vec<_> = documents
                .iter()
                .filter_map(|source| source["result_ref"].as_str())
                .filter(|reference| cited.contains(reference))
                .collect();
            let mut entry = json!({"source":documents[0]["document"]["source"],
                "result_refs":documents.iter().map(|source| &source["result_ref"]).collect::<Vec<_>>()});
            if !used.is_empty() {
                entry["status"] = json!("cited");
                entry["cited_result_refs"] = json!(used);
            } else {
                ensure!(
                    gaps_by_provider.contains_key(&provider),
                    "missing multi-source coverage"
                );
                entry["status"] = json!("not_used");
            }
            if let Some(gap) = gaps_by_provider.get(&provider) {
                entry["reason"] = json!(gap.reason);
                entry["limitation_index"] = json!(gap.limitation_index);
                has_gap = true;
            }
            coverage.push(entry);
        }
        checked["source_coverage"] = json!(coverage);
        if has_gap {
            checked["status"] = json!("partial");
        }
    }

    // These are already public operational errors, not model claims. One
    // successful result must not make another source's failure disappear.
    let notices: Vec<_> = failures
        .values()
        .filter_map(|failure| {
            let message = failure["message"]
                .as_str()
                .filter(|text| !text.trim().is_empty())?;
            Some(json!(match failure["source"]
                .as_str()
                .filter(|text| !text.trim().is_empty())
            {
                Some(source) => format!("{source}: {message}"),
                None => message.to_owned(),
            }))
        })
        .collect();
    if !notices.is_empty() {
        let has_bound_gap = checked["source_coverage"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|entry| entry.get("limitation_index").is_some());
        let limitations = checked["limitations"]
            .as_array_mut()
            .context("missing limitations")?;
        // Preserve the indices of explicitly bound limitations.
        if !has_bound_gap {
            limitations.retain(|value| value != "Some source information is unavailable. The answer uses the available results.");
        }
        for notice in notices {
            if !limitations.contains(&notice) {
                limitations.push(notice);
            }
        }
        checked["status"] = json!("partial");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources() -> Vec<Value> {
        vec![
            json!({"result_ref":"company", "document":{"source":{"provider":"registry","name":"Company registry"},"data":{"name":"Example Ltd","active":true}}}),
            json!({"result_ref":"screening", "document":{"source":{"provider":"screening","name":"Screening source"},"data":{"matches":0}}}),
        ]
    }
    fn answer(evidence: Value) -> Value {
        json!({"observations":[{"text":"The registration and screening results support a limited company check, not a clearance.","evidence":evidence}],"limitations":[]})
    }
    fn checked(raw: &Value, sources: &[Value]) -> Value {
        super::super::validate_analysis(&raw.to_string(), sources, false).unwrap()
    }

    #[test]
    fn joined_findings_retain_citations_and_exact_values_from_each_provider() {
        let sources = sources();
        let raw = answer(
            json!([{"result_ref":"company","pointer":"/data/active"},{"result_ref":"screening","pointer":"/data/matches"}]),
        );
        let mut result = checked(&raw, &sources);
        validate(&raw.to_string(), &mut result, &sources, &BTreeMap::new()).unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["observations"][0]["evidence"][0]["value"], true);
        assert_eq!(result["observations"][0]["evidence"][1]["value"], 0);
        assert!(result["source_coverage"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["status"] == "cited"));
    }

    #[test]
    fn a_successful_source_cannot_be_silently_omitted_or_hidden_in_a_heading() {
        let sources = sources();
        let mut raw = answer(json!([{"result_ref":"company","pointer":"/data/name"}]));
        raw["presentation"] = json!({"kind":"comparison","title":"Overview","columns":[{"label":"Company"},{"result_ref":"screening","pointer":"/data/matches"}],"rows":[[{"label":"Name"},{"result_ref":"company","pointer":"/data/name"}]]});
        let mut result = checked(&raw, &sources);
        assert_eq!(
            validate(&raw.to_string(), &mut result, &sources, &BTreeMap::new())
                .unwrap_err()
                .to_string(),
            "missing multi-source coverage"
        );
        raw["presentation"]["rows"][0][1] =
            json!({"result_ref":"screening","pointer":"/data/matches"});
        let mut result = checked(&raw, &sources);
        validate(&raw.to_string(), &mut result, &sources, &BTreeMap::new()).unwrap();
    }

    #[test]
    fn visible_source_bound_gaps_are_partial_and_cannot_invent_a_source_failure() {
        let sources = sources();
        let mut raw = answer(json!([{"result_ref":"company","pointer":"/data/name"}]));
        raw["limitations"] = json!(["The screening record belongs to a different entity, so it cannot establish this company's screening outcome."]);
        raw["source_gaps"] =
            json!([{"result_ref":"screening","reason":"entity_mismatch","limitation_index":0}]);
        let mut result = checked(&raw, &sources);
        validate(&raw.to_string(), &mut result, &sources, &BTreeMap::new()).unwrap();
        assert_eq!(result["status"], "partial");
        assert_eq!(result["limitations"], raw["limitations"]);
        raw["observations"][0]["evidence"] = json!([{"result_ref":"company","pointer":"/data/name"},{"result_ref":"screening","pointer":"/data/matches"}]);
        let mut result = checked(&raw, &sources);
        validate(&raw.to_string(), &mut result, &sources, &BTreeMap::new()).unwrap();
        assert_eq!(
            result["status"], "partial",
            "a citation must not erase a separately declared source gap"
        );
        for invalid in [
            json!([{"result_ref":"invented","reason":"entity_mismatch","limitation_index":0}]),
            json!([{"result_ref":"screening","reason":"entity_mismatch","limitation_index":1}]),
            json!([{"result_ref":"screening","reason":"source_failed","limitation_index":0}]),
        ] {
            raw["source_gaps"] = invalid;
            assert!(validate(
                &raw.to_string(),
                &mut checked(&raw, &sources),
                &sources,
                &BTreeMap::new()
            )
            .is_err());
        }
    }

    #[test]
    fn source_branding_cannot_count_as_a_factual_contribution() {
        let sources = sources();
        let raw = answer(json!([{"result_ref":"company","pointer":"/data/name"}]));
        let mut result = checked(&raw, &sources);
        // Even a previously materialized metadata reference must not stand in
        // for actual source data at this coverage boundary.
        result["observations"][0]["evidence"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "result_ref":"screening", "pointer":"/source/name", "value":"Screening source"
            }));
        assert_eq!(
            validate(&raw.to_string(), &mut result, &sources, &BTreeMap::new())
                .unwrap_err()
                .to_string(),
            "missing multi-source coverage"
        );
        // The normal validator already rejects this pointer as well.
        let raw = answer(json!([{"result_ref":"screening","pointer":"/source/name"}]));
        assert!(super::super::validate_analysis(&raw.to_string(), &sources, false).is_err());
    }

    #[test]
    fn same_provider_codebooks_do_not_require_duplicate_citations_and_single_source_is_unchanged() {
        let mut sources = sources();
        sources.push(json!({"result_ref":"codebook","document":{"source":{"provider":"registry"},"data":{"description":"Column definitions"}}}));
        let raw = answer(
            json!([{"result_ref":"company","pointer":"/data/name"},{"result_ref":"screening","pointer":"/data/matches"}]),
        );
        let mut result = checked(&raw, &sources);
        validate(&raw.to_string(), &mut result, &sources, &BTreeMap::new()).unwrap();
        assert_eq!(result["source_coverage"].as_array().unwrap().len(), 2);
        let raw = answer(json!([{"result_ref":"company","pointer":"/data/name"}]));
        let mut result = checked(&raw, &sources[..1]);
        let original = result.clone();
        validate(
            &raw.to_string(),
            &mut result,
            &sources[..1],
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(result, original);
    }

    #[test]
    fn actual_failure_stays_visible_alongside_a_successful_source() {
        let sources = sources();
        let raw = answer(json!([{"result_ref":"company","pointer":"/data/name"}]));
        let failures = BTreeMap::from([(
            "screening".into(),
            json!({"source":"Screening source","message":"The source request failed."}),
        )]);
        let mut result = checked(&raw, &sources[..1]);
        validate(&raw.to_string(), &mut result, &sources[..1], &failures).unwrap();
        assert_eq!(result["status"], "partial");
        assert_eq!(
            result["limitations"],
            json!(["Screening source: The source request failed."])
        );
        assert_eq!(
            result["observations"][0]["evidence"][0]["value"],
            "Example Ltd"
        );
    }

    #[tokio::test]
    async fn omitted_provider_uses_the_existing_repair_and_returns_a_joined_supported_answer() {
        use axum::{routing::post, Json, Router};
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let requests = calls.clone();
        let app = Router::new().route("/", post(move |Json(request): Json<Value>| {
            let requests = requests.clone();
            async move {
                let first = requests.fetch_add(1, Ordering::SeqCst) == 0;
                let messages = request["messages"].as_array().unwrap();
                let evidence = if first {
                    assert!(messages[0]["content"].as_str().unwrap().contains("join the relevant source findings"));
                    json!([{"result_ref":"company","pointer":"/data/active"}])
                } else {
                    assert_eq!(messages.len(), 4);
                    let repair = messages[3]["content"].as_str().unwrap();
                    assert!(repair.contains("source_coverage_missing"));
                    assert!(repair.contains("Preserve evidence from each distinct provider"));
                    json!([{"result_ref":"company","pointer":"/data/active"},{"result_ref":"screening","pointer":"/data/matches"}])
                };
                Json(json!({"choices":[{"message":{"content":answer(evidence).to_string()}}]}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let parser = super::super::Parser::new(
            reqwest::Client::new(),
            url,
            "fixture".into(),
            "fixture".into(),
        );
        let result = parser
            .analyze(
                &json!(["Combine registry and screening findings into a limited company check"]),
                &sources(),
                &BTreeMap::new(),
                &[],
            )
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(result["status"], "completed");
        assert_eq!(
            result["observations"][0]["evidence"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        server.abort();
    }
}
