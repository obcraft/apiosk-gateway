//! Optional news is saved alongside the answer, never admitted as primary
//! evidence, a completed requirement, a supplier verdict or a paid source.
use crate::{parser::Intent, state::Task};
use serde_json::{json, Value};

pub(super) fn enrich(task: &mut Task, intent: &Intent, previous: Option<&Value>) {
    enrich_with(task, intent, previous, |question, subjects| {
        crate::supporting_news::relevant(&question, &subjects)
    });
}

fn enrich_with<F>(task: &mut Task, intent: &Intent, previous: Option<&Value>, fetch: F)
where
    F: FnOnce(String, Vec<String>) -> Vec<Value>,
{
    let Some(answer) = task.analysis.as_ref() else { return };
    // An unavailable/empty answer must never become a successful result just
    // because a public news feed happens to mention the subject.
    if !matches!(answer["status"].as_str(), Some("completed" | "partial"))
        || (!answer["observations"].as_array().is_some_and(|v| !v.is_empty())
            && !answer["verdict"].is_object())
    {
        return;
    }
    if answer.get("supporting_news").is_some() { return; }
    if let Some(saved) = previous.filter(|v| v.is_array()) {
        task.analysis.as_mut().unwrap()["supporting_news"] = saved.clone();
        return;
    }
    let Some(question) = enrichment_question(task, intent) else {
        task.analysis.as_mut().unwrap()["supporting_news"] = json!([]);
        return;
    };
    let subjects = named_subjects(task, intent);
    let articles = fetch(question, subjects);
    // Save empty selections too, so retries do not silently change the context
    // of an already saved answer. The App hides an empty list.
    task.analysis.as_mut().unwrap()["supporting_news"] = json!(articles);
}

fn enrichment_question(task: &Task, intent: &Intent) -> Option<String> {
    let question = task.conversation.last()?.question.trim();
    if question.is_empty() { return None; }
    // A selected endpoint saves only an execution label as its question. Its
    // schema-checked literal query remains in the intent. Recover that query
    // only for this exact direct-call shape; ordinary questions keep all their
    // scope and constraints, even when source inputs contain another query.
    let [goal] = intent.goals.as_slice() else { return Some(question.to_owned()); };
    let Some(source) = goal.constraints.get("source").and_then(Value::as_str) else { return Some(question.to_owned()); };
    if task.capability_intake.is_some()
        || question != format!("Run {} on {}", goal.capability, source)
    {
        return Some(question.to_owned());
    }
    let subject = intent.subjects.get(goal.subject)?;
    let mut query: Option<&str> = None;
    for (field, value) in &subject.values {
        let search_input = matches!(field.as_str(), "web.query" | "web.source_search_query" | "web.search_query" | "web.input.query");
        // Other inputs may narrow a source to a country, historical period or
        // language. Do not silently discard those constraints for current RSS.
        if !search_input { return None; }
        let value = value.as_str()?.trim();
        // Never truncate away a date, geography or another query constraint.
        // Ambiguous multiple queries and non-text inputs remain unsupplemented.
        if value.is_empty() || value.chars().count() > 2000 { return None; }
        if query.is_some_and(|existing| existing != value) { return None; }
        query = Some(value);
    }
    query.map(str::to_owned)
}

fn named_subjects(task: &Task, intent: &Intent) -> Vec<String> {
    // A topical subject such as "European AI regulation" is a search topic,
    // not a named entity whose full phrase must appear in an article.
    if let Some(intake) = &task.capability_intake {
        return intake.parsed_request.subjects.iter()
            .filter(|s| matches!(s.kind.as_str(), "company" | "organization" | "organisation" | "person"))
            .take(16).map(|s| s.label.clone()).collect();
    }
    intent.subjects.iter().enumerate().filter(|(index, subject)| {
        subject.values.keys().any(|key| key.starts_with("company.") || key.starts_with("person."))
            || intent.goals.iter().any(|goal| goal.subject == *index
                && (goal.capability.starts_with("company.") || goal.capability.starts_with("person.")))
    }).take(16).map(|(_, subject)| subject.label.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Subject;
    use std::collections::BTreeMap;

    fn fixture() -> (Task, Intent) {
        let mut task = Task::default();
        task.record_turn("What are Acme's recent semiconductor investments?", None);
        task.active_results.push(uuid::Uuid::new_v4());
        task.analysis = Some(json!({"status":"partial", "observations":[{"text":"Saved primary finding", "evidence":[{"value":"unchanged"}]}],
            "limitations":["A requested source is unavailable."], "completeness":{"complete":false}}));
        let intent = Intent { subjects:vec![Subject { entity_ref:None, label:"Acme".into(), values:BTreeMap::from([("company.name".into(), json!("Acme"))]) }],
            goals:vec![], answer_operations:vec![], unresolved_references:vec![] };
        (task, intent)
    }

    fn article() -> Value {
        json!({"publisher":"BBC News", "title":"Acme expands semiconductor investment", "summary":"A publisher feed excerpt.",
            "url":"https://www.bbc.com/news/articles/example", "published_at":"2026-10-09T12:00:00Z", "feed_url":"https://feeds.bbci.co.uk/news/rss.xml"})
    }

    #[test]
    fn supporting_context_is_additive_persisted_and_cannot_complete_primary_checks() {
        let (mut task, intent) = fixture();
        task.intent = Some(serde_json::to_value(&intent).unwrap());
        task.proposal = Some(json!({"quote_ref":uuid::Uuid::new_v4(),"cap_atomic":100_000,"fee_bps":1500}));
        task.failed_steps.insert("missing-check".into(), json!({"code":"unavailable"}));
        let before = serde_json::to_value(&task).unwrap();
        let parser_context = task.parser_context();
        enrich_with(&mut task, &intent, None, |q, subjects| {
            assert!(q.contains("semiconductor"));
            assert_eq!(subjects, ["Acme"]);
            vec![article()]
        });
        assert_eq!(task.analysis.as_ref().unwrap()["supporting_news"], json!([article()]));
        assert_eq!(task.parser_context(), parser_context, "optional news cannot enter follow-up parsing or planning");
        let mut without_news = task.clone();
        without_news.analysis.as_mut().unwrap().as_object_mut().unwrap().remove("supporting_news");
        assert_eq!(serde_json::to_value(without_news).unwrap(), before, "no billing, evidence, source IDs, completeness or status changes");
        let mut reloaded: Task = serde_json::from_value(serde_json::to_value(&task).unwrap()).unwrap();
        enrich_with(&mut reloaded, &intent, None, |_, _| { panic!("a saved answer must not refetch") });
        assert_eq!(reloaded.analysis, task.analysis);
        assert_eq!(reloaded.context_view()["analysis"]["supporting_news"], json!([article()]));
    }

    #[test]
    fn empty_or_unavailable_answers_never_fetch_news() {
        for analysis in [Value::Null, json!({"status":"unavailable","observations":[]}),
            json!({"status":"partial","observations":[],"limitations":["No source result."]})] {
            let (mut task, intent) = fixture();
            task.analysis = Some(analysis.clone());
            enrich_with(&mut task, &intent, None, |_, _| { panic!("no enrichment without primary findings") });
            assert_eq!(task.analysis, Some(analysis));
        }
    }

    #[test]
    fn unavailable_feeds_or_no_match_save_an_invisible_empty_snapshot() {
        let (mut task, intent) = fixture();
        enrich_with(&mut task, &intent, None, |_, _| { vec![] });
        assert_eq!(task.analysis.as_ref().unwrap()["supporting_news"], json!([]));
        assert_eq!(task.analysis.as_ref().unwrap()["status"], "partial");
        enrich_with(&mut task, &intent, None, |_, _| { panic!("empty snapshots also remain stable") });
    }

    #[test]
    fn manual_recomposition_reuses_the_saved_selection_even_when_empty() {
        for saved in [json!([]), json!([article()])] {
            let (mut task, intent) = fixture();
            enrich_with(&mut task, &intent, Some(&saved), |_, _| { panic!("manual retry cannot refresh news") });
            assert_eq!(task.analysis.as_ref().unwrap()["supporting_news"], saved);
        }
    }

    #[test]
    fn direct_supplier_answer_can_have_context_without_changing_its_verdict() {
        let (mut task, intent) = fixture();
        let verdict = json!({"decision":"hold", "reason":"Source evidence is incomplete"});
        task.analysis = Some(json!({"status":"completed","observations":[],"verdict":verdict}));
        enrich_with(&mut task, &intent, None, |_, _| { vec![article()] });
        assert_eq!(task.analysis.as_ref().unwrap()["verdict"], verdict);
        assert_eq!(task.analysis.as_ref().unwrap()["supporting_news"], json!([article()]));
    }

    #[test]
    fn topical_subjects_are_not_treated_as_named_entities() {
        let (task, mut intent) = fixture();
        assert_eq!(named_subjects(&task, &intent), ["Acme"]);
        intent.subjects[0].label = "European AI regulation".into();
        intent.subjects[0].values.clear();
        assert!(named_subjects(&task, &intent).is_empty());
    }

    fn direct_web_fixture() -> (Task, Intent) {
        let (mut task, _) = fixture();
        task.conversation.clear();
        task.record_turn("Run web.search on exa-search", None);
        task.analysis = Some(json!({"status":"completed","observations":[{"text":"Saved primary web search finding","evidence":[{"value":"unchanged"}]}]}));
        let intent = Intent {
            subjects:vec![Subject { entity_ref:None, label:"Batavus doorstart fietsmerken".into(),
                values:BTreeMap::from([("web.query".into(), json!("Batavus doorstart fietsmerken"))]) }],
            goals:vec![crate::parser::Goal { capability:"web.search".into(), subject:0,
                constraints:BTreeMap::from([("source".into(),json!("exa-search"))]) }],
            answer_operations:vec![], unresolved_references:vec![],
        };
        (task, intent)
    }

    #[test]
    fn prepared_direct_web_query_supplies_news_context_and_persists_supplement() {
        let (mut task, intent) = direct_web_fixture();
        let before = serde_json::to_value(&task).unwrap();
        let news = json!({"publisher":"NOS","title":"Fietsmerken Batavus, KOGA en Babboe blijven bestaan, verkocht aan Dutech",
            "summary":"De fietsmerken Batavus, KOGA en Babboe maken een doorstart.","url":"https://nos.nl/l/2634305",
            "published_at":"2026-10-09T15:28:04Z","feed_url":"https://feeds.nos.nl/nosnieuwsalgemeen"});
        let returned = news.clone();
        enrich_with(&mut task, &intent, None, |question, subjects| {
            assert_eq!(question, "Batavus doorstart fietsmerken");
            assert!(subjects.is_empty(), "a literal search string must not become a company name");
            vec![returned]
        });
        assert_eq!(task.analysis.as_ref().unwrap()["supporting_news"], json!([news]));
        let mut without_news = task.clone();
        without_news.analysis.as_mut().unwrap().as_object_mut().unwrap().remove("supporting_news");
        assert_eq!(serde_json::to_value(without_news).unwrap(), before, "direct quote, query, primary finding and billing remain unchanged");
        enrich_with(&mut task, &intent, None, |_, _| { panic!("saved direct result must not refetch") });
    }

    #[test]
    fn direct_inputs_never_replace_an_actual_question() {
        let (mut task, intent) = direct_web_fixture();
        task.conversation.clear();
        let original = "Find bicycle manufacturers in Belgium in 2024";
        task.record_turn(original, None);
        enrich_with(&mut task, &intent, None, |question, subjects| {
            assert_eq!(question, original);
            assert!(subjects.is_empty());
            vec![]
        });
        assert_eq!(task.analysis.as_ref().unwrap()["supporting_news"], json!([]));
    }

    #[test]
    fn direct_results_labels_and_ambiguous_inputs_do_not_supply_news_queries() {
        for values in [
            BTreeMap::from([("source_input.exa.url".into(),json!("https://example.com/Batavus-doorstart"))]),
            BTreeMap::from([("web.query".into(),json!({"text":"Batavus doorstart"}))]),
            BTreeMap::from([("web.query".into(),json!("Batavus doorstart")),("web.search_query".into(),json!("Microsoft immigration"))]),
            BTreeMap::from([("web.query".into(),json!("x".repeat(2001)))]),
            BTreeMap::from([("web.query".into(),json!("climate emissions")),("web.country".into(),json!("DE"))]),
            BTreeMap::from([("source_input.chronica.q".into(),json!("climate emissions")),("source_input.chronica.year-end".into(),json!(1920))]),
        ] {
            let (mut task, mut intent) = direct_web_fixture();
            intent.subjects[0].values = values;
            task.analysis.as_mut().unwrap()["observations"][0]["text"] = json!("Batavus doorstart fietsmerken");
            enrich_with(&mut task, &intent, None, |_, _| { panic!("results or labels cannot substitute for a literal query") });
            assert_eq!(task.analysis.as_ref().unwrap()["supporting_news"], json!([]));
        }
    }
}
