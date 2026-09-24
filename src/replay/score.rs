//! Deterministic quality scores for the quality set (#4), per dimension and
//! never collapsed into one number. A dimension that does not apply to a
//! case is `None`. Only a human sets a case's review status to `approved`,
//! and only approved cases gate a change.

use super::{scalar, Outcome};
use crate::telemetry::classify;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityCase {
    pub id: String,
    pub lang: String,
    pub question: String,
    pub query_class: String,
    pub data_condition: String,
    pub expect: Expect,
    /// The replay case that supplies its sources and model tape.
    pub replay: Option<String>,
    pub review: Review,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    pub entities: Vec<ExpectedEntity>,
    pub sources: ExpectedSources,
    pub capabilities: Vec<String>,
    pub period: Option<Period>,
    pub required_fields: Vec<RequiredField>,
    pub cap_micro_usd: Option<i64>,
    pub must_not: Vec<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedEntity {
    pub label: String,
    #[serde(default)]
    pub identifiers: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedSources {
    pub required: Vec<String>,
    pub forbidden: Vec<String>,
    pub official_required: bool,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Period {
    pub from: String,
    pub to: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequiredField {
    pub pointer_suffix: String,
    pub label: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub status: String,
    pub by: Option<String>,
    pub at: Option<String>,
}

/// The data conditions and forbidden behaviours a case may declare.
pub const CONDITIONS: [&str; 7] = ["normal", "empty", "missing_field", "contradictory", "partial_failure", "partial_coverage", "ambiguous_entity"];
pub const MUST_NOT: [&str; 3] = ["negative_claim_from_empty_page", "like_for_like_across_periods", "purchase_before_selection"];

fn normal(text: &str) -> String {
    text.to_lowercase().trim().trim_end_matches('.').split_whitespace().collect::<Vec<_>>().join(" ")
}
fn fraction(hits: usize, total: usize) -> Option<f64> {
    (total > 0).then(|| hits as f64 / total as f64)
}
fn flag(ok: bool) -> Option<f64> {
    Some(if ok { 1.0 } else { 0.0 })
}

/// One cited value: source, subject, pointer and the stored value.
struct Cited {
    subject: String,
    pointer: String,
    value: Value,
}

fn citations(outcome: &Outcome) -> Vec<Cited> {
    let Some(analysis) = &outcome.analysis else { return vec![] };
    let mut references: Vec<&Value> = analysis["observations"].as_array().into_iter().flatten()
        .flat_map(|observation| observation["evidence"].as_array().into_iter().flatten()).collect();
    references.extend(analysis["presentation"]["rows"].as_array().into_iter().flatten()
        .filter_map(Value::as_array).flatten().filter(|cell| cell.get("pointer").is_some()));
    references.into_iter().map(|reference| {
        let pointer = reference["pointer"].as_str().unwrap_or_default().to_owned();
        let document = reference["result_ref"].as_str().and_then(|id| id.parse::<Uuid>().ok()).and_then(|id| outcome.documents.get(&id));
        Cited {
            subject: document.and_then(|document| document["subject"]["label"].as_str()).unwrap_or_default().to_owned(),
            value: scalar(document.and_then(|document| document.pointer(&pointer))),
            pointer,
        }
    }).collect()
}

/// Scores one replayed case on every dimension.
pub fn score(case: &QualityCase, outcome: &Outcome) -> BTreeMap<&'static str, Option<f64>> {
    let expect = &case.expect;
    let cited = citations(outcome);
    let analysis = outcome.analysis.as_ref();
    let limitations = analysis.and_then(|analysis| analysis["limitations"].as_array()).map_or(0, Vec::len);
    let status = analysis.and_then(|analysis| analysis["status"].as_str());
    let intent = outcome.intent.as_ref();
    let plan_slugs: BTreeSet<&str> = outcome.plan.iter().flat_map(|plan| plan.steps.iter().map(|step| step.service.api_slug.as_str())).collect();
    let plan_capabilities: BTreeSet<&str> = outcome.plan.iter().flat_map(|plan| plan.steps.iter().map(|step| step.service.contract.capability_slug.as_str())).collect();
    let documents = || outcome.documents.values().map(Value::to_string).collect::<Vec<_>>();

    let entity = fraction(expect.entities.iter().filter(|entity| {
        let label = normal(&entity.label);
        let named = intent.is_some_and(|intent| intent.subjects.iter().any(|subject| {
            let subject = normal(&subject.label);
            subject.contains(&label) || label.contains(&subject)
        }));
        let identified = entity.identifiers.iter().all(|(kind, value)| {
            intent.is_some_and(|intent| intent.subjects.iter().any(|subject| subject.values.get(kind).is_some_and(|given| given == &json!(value))))
                || cited.iter().any(|cited| cited.value == json!(value))
                || documents().iter().any(|document| document.contains(&format!("\"{value}\"")))
        });
        named && identified
    }).count(), expect.entities.len());

    let sources = &expect.sources;
    let source = (outcome.plan.is_some() && (!sources.required.is_empty() || !sources.forbidden.is_empty())).then(|| {
        let required = sources.required.iter().all(|slug| plan_slugs.contains(slug.as_str()));
        let forbidden = sources.forbidden.iter().any(|slug| plan_slugs.contains(slug.as_str()));
        let official = !sources.official_required || plan_slugs.iter().all(|slug| sources.required.iter().any(|required| required == slug));
        if required && !forbidden && official { 1.0 } else { 0.0 }
    });

    let capability = outcome.plan.as_ref().and_then(|_| fraction(expect.capabilities.iter().filter(|capability| plan_capabilities.contains(capability.as_str())).count(), expect.capabilities.len()));

    let period = intent.and_then(|intent| {
        let said = serde_json::to_string(&(&intent.goals, &intent.subjects)).unwrap_or_default();
        match &expect.period {
            Some(period) => flag(said.contains(&period.from) && said.contains(&period.to)),
            // No period asked: none may be invented.
            None => flag(!intent.goals.iter().any(|goal| goal.constraints.keys().any(|key| ["period", "year", "date", "from", "to"].contains(&key.as_str())))),
        }
    });

    let required_fields = analysis.and_then(|_| fraction(expect.required_fields.iter()
        .filter(|field| cited.iter().any(|cited| cited.pointer.ends_with(&field.pointer_suffix) && !cited.value.is_null())).count(), expect.required_fields.len()));

    let completeness = match (intent, analysis) {
        (Some(intent), Some(_)) => {
            let asked: BTreeSet<&str> = intent.goals.iter().filter_map(|goal| intent.subjects.get(goal.subject)).map(|subject| subject.label.as_str()).collect();
            fraction(asked.iter().filter(|label| limitations > 0 || cited.iter().any(|cited| normal(&cited.subject) == normal(label))).count(), asked.len())
        }
        _ => None,
    };

    let evidence = analysis.and_then(|_| flag(cited.iter().all(|cited| !cited.value.is_null())));

    let cost = expect.cap_micro_usd.and_then(|cap| flag(outcome.plan.as_ref().is_some_and(|plan| plan.cap_atomic == cap)));

    let values_for = |suffixes: &[&RequiredField]| cited.iter().filter(|cited| suffixes.is_empty() || suffixes.iter().any(|field| cited.pointer.ends_with(&field.pointer_suffix)))
        .map(|cited| cited.value.to_string()).collect::<BTreeSet<_>>().len();
    let fields: Vec<&RequiredField> = expect.required_fields.iter().collect();
    let mut condition = match case.data_condition.as_str() {
        "normal" => flag(status == Some("completed")),
        "empty" => flag(status != Some("completed") && cited.is_empty()),
        "partial_coverage" | "missing_field" => flag(limitations > 0),
        "contradictory" => flag(values_for(&fields) >= 2),
        "partial_failure" => flag(!outcome.task.failed_steps.is_empty() && limitations > 0),
        "ambiguous_entity" => flag(outcome.plan.is_none() && outcome.status == "needs_input"),
        _ => None,
    };
    let violated = expect.must_not.iter().any(|rule| match rule.as_str() {
        "negative_claim_from_empty_page" => status == Some("completed") && outcome.documents.values().any(|document| document["coverage"] == "empty"),
        "like_for_like_across_periods" => status == Some("completed") && limitations == 0,
        "purchase_before_selection" => outcome.plan.is_some(),
        _ => false,
    });
    if violated {
        condition = Some(0.0);
    }

    let class = outcome.plan.as_ref().and_then(|_| flag(classify(&outcome.task).as_str() == case.query_class));

    BTreeMap::from([
        ("entity", entity), ("source", source), ("capability", capability), ("period", period),
        ("required_fields", required_fields), ("completeness", completeness), ("evidence", evidence),
        ("cost", cost), ("condition", condition), ("class", class),
    ])
}

/// Approved cases whose dimension fell from a full score to zero against the
/// accepted baseline. Draft cases never gate.
pub fn regressions(cases: &[QualityCase], baseline: &Value, scores: &BTreeMap<String, BTreeMap<&'static str, Option<f64>>>) -> Vec<String> {
    let mut found = Vec::new();
    for case in cases.iter().filter(|case| case.review.status == "approved") {
        let Some(now) = scores.get(&case.id) else { continue };
        for (dimension, value) in now {
            if baseline["cases"][&case.id][*dimension].as_f64() == Some(1.0) && *value == Some(0.0) {
                found.push(format!("{} {dimension}: 1 -> 0", case.id));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Task;

    fn case(condition: &str, must_not: &[&str]) -> QualityCase {
        serde_json::from_value(json!({"id":"q","lang":"en","question":"?","query_class":"simple_lookup","data_condition":condition,
            "expect":{"entities":[{"label":"Example B.V.","identifiers":{"company.id":"123"}}],"sources":{"required":["fixture"],"forbidden":["reseller"],"official_required":true},
                "capabilities":["company.accounts"],"period":null,"required_fields":[{"pointer_suffix":"/accounts","label":"accounts"}],"cap_micro_usd":null,"must_not":must_not},
            "replay":null,"review":{"status":"draft","by":null,"at":null}})).unwrap()
    }
    fn outcome(analysis: Value, coverage: &str) -> Outcome {
        let reference = crate::replay::result_ref(0);
        Outcome {
            status: "answered", message: None,
            intent: Some(serde_json::from_value(json!({"subjects":[{"entity_ref":null,"label":"Example B.V.","values":{"company.name":"Example B.V."}}],
                "goals":[{"capability":"company.accounts","subject":0,"constraints":{}}],"answer_operations":[],"unresolved_references":[]})).unwrap()),
            plan: None, task: Task::default(),
            documents: BTreeMap::from([(reference, json!({"data":{"accounts":"2025","id":"123"},"subject":{"label":"Example B.V."},"coverage":coverage}))]),
            analysis: Some(analysis),
        }
    }

    #[test]
    fn cited_fields_entities_and_conditions_score_per_dimension() {
        let reference = crate::replay::result_ref(0);
        let answered = json!({"status":"completed","limitations":[],"observations":[{"text":"x","evidence":[{"result_ref":reference,"pointer":"/data/accounts"}]}]});
        let scores = score(&case("normal", &[]), &outcome(answered, "source_response"));
        assert_eq!(scores["entity"], Some(1.0), "the identifier is in the stored source");
        assert_eq!(scores["required_fields"], Some(1.0));
        assert_eq!(scores["evidence"], Some(1.0));
        assert_eq!(scores["completeness"], Some(1.0));
        assert_eq!(scores["condition"], Some(1.0));
        assert_eq!(scores["period"], Some(1.0), "no period was asked and none was invented");
        assert_eq!(scores["source"], None, "no plan to judge");
        let invented = json!({"status":"completed","limitations":[],"observations":[{"text":"x","evidence":[{"result_ref":reference,"pointer":"/data/missing"}]}]});
        let scores = score(&case("normal", &[]), &outcome(invented, "source_response"));
        assert_eq!(scores["evidence"], Some(0.0), "a pointer to nothing is not evidence");
        assert_eq!(scores["required_fields"], Some(0.0));
    }

    #[test]
    fn an_empty_page_is_never_a_negative_answer() {
        let claimed = json!({"status":"completed","limitations":[],"observations":[]});
        assert_eq!(score(&case("empty", &["negative_claim_from_empty_page"]), &outcome(claimed, "empty"))["condition"], Some(0.0));
        let honest = json!({"status":"partial","limitations":["No records were returned."],"observations":[]});
        assert_eq!(score(&case("empty", &["negative_claim_from_empty_page"]), &outcome(honest, "empty"))["condition"], Some(1.0));
    }

    #[test]
    fn only_approved_cases_gate_and_only_on_a_full_to_zero_drop() {
        let mut approved = case("normal", &[]);
        approved.review.status = "approved".into();
        let draft = QualityCase { id: "draft".into(), ..case("normal", &[]) };
        let baseline = json!({"cases":{"q":{"evidence":1.0,"entity":0.5},"draft":{"evidence":1.0}}});
        let scores = BTreeMap::from([
            ("q".to_owned(), BTreeMap::from([("evidence", Some(0.0)), ("entity", Some(0.0))])),
            ("draft".to_owned(), BTreeMap::from([("evidence", Some(0.0))])),
        ]);
        assert_eq!(regressions(&[approved, draft], &baseline, &scores), ["q evidence: 1 -> 0"]);
    }
}
