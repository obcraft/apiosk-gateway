//! Recover explicit annual qualifiers after indexed observations are expanded.
//! A year must itself be cited from the saved source; requested years, amounts,
//! retrieval timestamps and shorter reporting periods are not annual evidence.
use crate::answer_blocks::Requirement;
use serde_json::{json, Value};

pub(super) fn hydrate(raw: &mut Value, requirements: &[Requirement], sources: &[Value]) {
    let observations = raw["observations"].as_array().cloned().unwrap_or_default();
    let Some(claims) = raw["requirement_status"].as_array_mut() else {
        return;
    };
    for claim in claims {
        let Some(requirement) = requirements
            .iter()
            .find(|requirement| claim["requirement_id"].as_str() == Some(&requirement.id))
        else {
            continue;
        };
        let Some(period) = requirement.period.as_deref() else {
            continue;
        };
        if claim["status"] != "answered" {
            continue;
        }
        let cited: Vec<_> = claim["answered_by"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|id| {
                id.as_str()?
                    .strip_prefix('o')?
                    .parse::<usize>()
                    .ok()?
                    .checked_sub(1)
            })
            .filter_map(|index| observations.get(index))
            .flat_map(|observation| observation["evidence"].as_array().into_iter().flatten())
            .collect();
        for fact in claim["evidence"].as_array_mut().into_iter().flatten() {
            // Indexed claims cannot supply trusted period metadata themselves.
            if let Some(object) = fact.as_object_mut() {
                object.remove("period");
            }
            if !cited
                .iter()
                .any(|reference| same_reference(fact, reference))
            {
                continue;
            }
            if let Some(qualifier) = annual_qualifier(fact, requirement, period, sources) {
                fact["period"] = qualifier;
            }
        }
    }
}

fn same_reference(left: &Value, right: &Value) -> bool {
    ["result_ref", "pointer", "quote"]
        .iter()
        .all(|key| left[*key] == right[*key])
}

fn annual_qualifier(
    fact: &Value,
    requirement: &Requirement,
    period: &str,
    sources: &[Value],
) -> Option<Value> {
    let reference = fact["result_ref"].as_str()?;
    let pointer = fact["pointer"].as_str()?.strip_prefix("/data/")?;
    let pointer = format!("/data/{pointer}");
    let mut matching = sources
        .iter()
        .filter(|source| source["result_ref"] == reference);
    let source = matching.next()?;
    if matching.next().is_some()
        || requirement.entity_ref.as_deref().is_some_and(|entity| {
            source["document"]["subject"]["entity_ref"].as_str() != Some(entity)
        })
    {
        return None;
    }
    let saved = source["document"].pointer(&pointer)?;
    if !fact["value"].is_null() && fact["value"] != *saved {
        return None;
    }
    if !fact["quote"].is_null() && fact["quote"].as_str() != saved.as_str() {
        return None;
    }
    let text = saved
        .as_str()
        .map(str::to_owned)
        .or_else(|| saved.as_u64().map(|year| year.to_string()))?;
    let year = text.trim().strip_suffix("JJ00").unwrap_or(text.trim());
    if year != period || year.len() != 4 || !year.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let (parent_path, key) = pointer.rsplit_once('/')?;
    let parent = source["document"].pointer(parent_path)?;
    let key = key.replace("~1", "/").replace("~0", "~");
    let field = if key == "value" {
        parent["key"].as_str().unwrap_or(&key)
    } else {
        &key
    };
    let normalized: String = field
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect();
    let named_period = matches!(
        normalized.as_str(),
        "year"
            | "financialyear"
            | "reportingyear"
            | "fiscalyear"
            | "boekjaar"
            | "jaar"
            | "period"
            | "perioden"
            | "time"
            | "date"
            | "observationdate"
            | "requestdate"
    );
    let eurostat_label = source["document"]["source"]["provider"] == "eurostat"
        && parent_path == "/data/dimension/time/category/label"
        && key == year;
    if !named_period && !eurostat_label {
        return None;
    }
    Some(
        json!({"result_ref":reference,"pointer":pointer,"value":text,"key":year,
        "granularity":"year","basis":"evidence"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::answer_blocks::{RequirementClaim, UnknownReason};

    fn requirement() -> Requirement {
        serde_json::from_value(
            json!({"id":"r1","criterion":"statistics.example","criterion_kind":"capability",
            "entity_ref":"entity","period":"2024","status":"pending"}),
        )
        .unwrap()
    }

    fn source(data: Value) -> Value {
        json!({"result_ref":"saved","document":{"data":data,"subject":{"entity_ref":"entity"},"source":{"provider":"example"}}})
    }

    fn answer(pointer: &str) -> Value {
        let evidence = json!([{"result_ref":"saved","pointer":pointer}]);
        json!({"observations":[{"text":"The reporting period is {{a}}.","evidence":evidence}],
            "requirement_status":[{"requirement_id":"r1","status":"answered","answered_by":["o1"],"evidence":evidence}]})
    }

    fn reconciled(mut raw: Value, sources: &[Value]) -> crate::answer_blocks::Completeness {
        hydrate(&mut raw, &[requirement()], sources);
        let claims: Vec<RequirementClaim> =
            serde_json::from_value(raw["requirement_status"].clone()).unwrap();
        crate::completeness::reconcile(vec![requirement()], &claims, sources)
    }

    #[test]
    fn indexed_periods_use_only_cited_explicit_annual_source_values() {
        for (data, pointer) in [
            (json!({"year":2024}), "/data/year"),
            (json!({"period":"2024"}), "/data/period"),
            (json!({"Perioden":"2024JJ00"}), "/data/Perioden"),
            (
                json!({"opendataFields":[{"key":"FinancialYear","value":"2024"}]}),
                "/data/opendataFields/0/value",
            ),
        ] {
            let original = source(data);
            let mut raw = answer(pointer);
            hydrate(&mut raw, &[requirement()], &[original.clone()]);
            let qualifier = &raw["requirement_status"][0]["evidence"][0]["period"];
            assert_eq!(qualifier["pointer"], pointer);
            assert_eq!(qualifier["key"], "2024");
            assert_eq!(qualifier["granularity"], "year");
            assert_eq!(qualifier["basis"], "evidence");
            assert!(reconciled(raw, &[original]).complete);
        }
    }

    #[test]
    fn indexed_eurostat_year_labels_keep_the_original_dimension_pointer() {
        let mut original = source(
            json!({"dimension":{"time":{"category":{"label":{"2024":"2024"},"index":{"2024":0}}}}}),
        );
        original["document"]["source"]["provider"] = json!("eurostat");
        assert!(
            reconciled(
                answer("/data/dimension/time/category/label/2024"),
                &[original.clone()]
            )
            .complete
        );
        assert!(
            !reconciled(
                answer("/data/dimension/time/category/index/2024"),
                &[original]
            )
            .complete
        );
    }

    #[test]
    fn indexed_periods_do_not_invent_annual_coverage_from_amounts_dates_or_other_years() {
        for (data, pointer) in [
            (json!({"amount":2024}), "/data/amount"),
            (json!({"id":"2024"}), "/data/id"),
            (json!({"text":"The year is 2024"}), "/data/text"),
            (json!({"year":2023}), "/data/year"),
            (json!({"date":"2024-02-12"}), "/data/date"),
            (json!({"Perioden":"2024MM01"}), "/data/Perioden"),
            (json!({"Perioden":"2024KW01"}), "/data/Perioden"),
            (json!({"retrieved_at":"2024"}), "/data/retrieved_at"),
            (
                json!({"opendataFields":[{"key":"Assets","value":"2024"}]}),
                "/data/opendataFields/0/value",
            ),
        ] {
            let completeness = reconciled(answer(pointer), &[source(data)]);
            assert!(!completeness.complete, "{pointer}");
            assert_eq!(
                completeness.requirements[0].reason,
                Some(UnknownReason::PeriodMismatch)
            );
        }
    }

    #[test]
    fn indexed_periods_reject_wrong_identity_ambiguous_documents_and_uncited_or_modified_values() {
        let original = source(json!({"year":"2024","name":"Example"}));
        let raw = answer("/data/year");
        let mut other = original.clone();
        other["document"]["subject"]["entity_ref"] = json!("another");
        assert!(!reconciled(raw.clone(), &[other]).complete);
        assert!(!reconciled(raw.clone(), &[original.clone(), original.clone()]).complete);
        let mut uncited = raw.clone();
        uncited["observations"][0]["evidence"][0]["pointer"] = json!("/data/name");
        // Even pre-existing model metadata cannot turn an uncited year into proof.
        uncited["requirement_status"][0]["evidence"][0]["period"] = json!({"value":"2024"});
        assert!(!reconciled(uncited, &[original.clone()]).complete);
        let mut modified = raw.clone();
        modified["requirement_status"][0]["evidence"][0]["value"] = json!("2023");
        assert!(!reconciled(modified, &[original.clone()]).complete);
        let mut quoted = raw;
        for entry in ["observations", "requirement_status"] {
            quoted[entry][0]["evidence"][0]["quote"] = json!("202");
        }
        assert!(!reconciled(quoted, &[original]).complete);
    }
}
