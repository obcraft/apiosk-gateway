use super::*;
use std::collections::BTreeSet;

// Multiple search hits are evidence, not automatically an unresolved company.
// Ask only when choosing supplies a missing fact needed by an unfinished call.
pub(super) fn needs_candidate_facts(
    task: &Task,
    step: &resolver::Step,
    subject: Uuid,
    candidate_types: &BTreeSet<&str>,
) -> bool {
    step.entity_ref == subject
        && !task.step_finished(&step.key())
        && step.service.contract.requires.iter().any(|requirement| {
            !requirement.optional
                && candidate_types.contains(requirement.fact_type.as_str())
                && !crate::catalog::fact_value(&task.facts, subject, &requirement.fact_type)
                    .is_ok_and(|value| value.is_some())
        })
}

pub(super) fn step_waits_for_choice(task: &Task, step: &resolver::Step) -> bool {
    let Some(subject) = task.original_subject else { return false };
    let candidate_types = task.pending_candidates.iter()
        .filter_map(|candidate| candidate["facts"].as_array())
        .flatten()
        .filter_map(|fact| fact["type"].as_str())
        .collect();
    needs_candidate_facts(task, step, subject, &candidate_types)
}

// Polling an independent result must neither erase a real selection nor retain
// an obsolete one after its dependent step failed or gained its identifier.
pub(super) fn pending_choice(task: &mut Task, plan: &Plan) -> Option<Value> {
    if task.cancelled || !plan.steps.iter().any(|step| step_waits_for_choice(task, step)) {
        task.pending_candidates.clear();
        task.original_subject = None;
        return None;
    }
    let candidates: Vec<Uuid> = task.pending_candidates.iter()
        .filter_map(|candidate| candidate["entity_ref"].as_str()?.parse().ok())
        .collect();
    if candidates.is_empty() {
        task.pending_candidates.clear();
        task.original_subject = None;
        return None;
    }
    let action_id = task.actions.iter().find_map(|action| {
        match serde_json::from_value::<Action>(action.clone()).ok()? {
            Action::Choose { action_id, candidates: existing } if existing == candidates => Some(action_id),
            _ => None,
        }
    }).unwrap_or_else(Uuid::new_v4);
    Some(json!({"kind":"choose", "action_id":action_id, "candidates":candidates}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Task, Plan) {
        let catalog = Catalog::build(serde_json::from_str(include_str!(
            "../../tests/fixtures/sirenic/expanded-catalog.json"
        )).unwrap());
        let service = catalog.services.iter().find(|service| {
            service.contract.requires.iter().any(|r| !r.optional && r.fact_type == "company.registration.fr.siren")
        }).unwrap().clone();
        let subject = Uuid::new_v4();
        let task = Task { original_subject: Some(subject), ..Task::default() };
        let plan = Plan {
            quote_ref: Uuid::new_v4(),
            steps: vec![resolver::Step {service, entity_ref: subject, goal_index: 0}],
            cap_atomic: 100, expires_at: Utc::now() + chrono::Duration::minutes(10), hash: "fixture".into(),
            fee_bps: 1500, workflow_slug: None,
        };
        (task, plan)
    }

    fn add_candidates(task: &mut Task, kind: &str) {
        for value in ["123456789", "987654321"] {
            task.pending_candidates.push(json!({"entity_ref":Uuid::new_v4(),"facts":[{"type":kind,"value":value}]}));
        }
    }

    #[test]
    fn unrelated_web_hits_do_not_require_a_company_selection() {
        let (mut task, plan) = setup();
        add_candidates(&mut task, "web.url");
        assert!(pending_choice(&mut task, &plan).is_none());
        assert!(task.pending_candidates.is_empty());
        assert!(task.original_subject.is_none());
    }

    #[test]
    fn real_identifier_choice_survives_and_recovers_lost_actions() {
        let (mut task, plan) = setup();
        add_candidates(&mut task, "company.registration.fr.siren");
        let action = pending_choice(&mut task, &plan).unwrap();
        task.actions.push(action.clone());
        assert_eq!(pending_choice(&mut task, &plan).unwrap(), action);
        task.actions.clear();
        let recovered = pending_choice(&mut task, &plan).unwrap();
        assert_eq!(recovered["candidates"], action["candidates"]);
        assert!(step_waits_for_choice(&task, &plan.steps[0]));
    }

    #[test]
    fn failed_or_resolved_dependencies_release_stale_selection() {
        for resolved in [false, true] {
            let (mut task, plan) = setup();
            add_candidates(&mut task, "company.registration.fr.siren");
            assert!(pending_choice(&mut task, &plan).is_some());
            if resolved {
                task.facts.push(Fact {
                    entity_ref: plan.steps[0].entity_ref,
                    semantic_type: "company.registration.fr.siren".into(),
                    value: json!("123456789"), provenance: json!({"kind":"provider_response"}),
                    observed_at: Utc::now(), expires_at: Utc::now() + chrono::Duration::hours(1),
                    verification: "source_observed".into(),
                });
            } else {
                task.failed_steps.insert(plan.steps[0].key(), json!({"code":"missing_source_fact"}));
            }
            assert!(pending_choice(&mut task, &plan).is_none());
            assert!(task.pending_candidates.is_empty());
        }
    }

    #[tokio::test]
    async fn advancing_preserves_required_choice_and_releases_obsolete_choice() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://unused@127.0.0.1:1/unused").unwrap();
        let store = Store { pool: pool.clone() };
        let http = reqwest::Client::new();
        let engine = Engine {
            store: store.clone(), catalog: CatalogCache::new(pool),
            parser: Parser::new(http.clone(), "http://127.0.0.1:1".into(), "fixture".into(), "fixture".into()),
            signer: Signer::new(vec![3;32]).unwrap(),
            executor: Executor { store, http, supabase_url: "http://127.0.0.1:1".into(), enabled: false },
            app_url: "http://127.0.0.1:1".into(),
        };
        let (mut task, plan) = setup();
        task.proposal = Some(serde_json::to_value(&plan).unwrap());
        task.intent = Some(json!({"answer_operations":["Summarize saved evidence"]}));
        add_candidates(&mut task, "company.registration.fr.siren");
        let action = pending_choice(&mut task, &plan).unwrap();
        task.actions.push(action.clone());
        engine.advance(&mut task).unwrap();
        assert_eq!(task.actions, vec![action]);
        assert!(task.failed_steps.is_empty(), "selection-dependent calls must wait");
        task.failed_steps.insert(plan.steps[0].key(), json!({"code":"missing_source_fact"}));
        engine.advance(&mut task).unwrap();
        assert!(task.pending_candidates.is_empty());
        assert_eq!(task.actions.len(), 1);
        assert_eq!(task.actions[0]["kind"], "analyze");
        // Expiry allows a free partial answer, never another paid call, while
        // an already dispatched receipt remains pollable under its original ID.
        let (mut expired, mut plan) = setup();
        plan.expires_at = Utc::now() - chrono::Duration::minutes(1);
        expired.proposal = Some(serde_json::to_value(&plan).unwrap());
        expired.active_results.push(Uuid::new_v4());
        expired.intent = Some(json!({"answer_operations":["Summarize saved evidence"]}));
        expired.pending_dispatches.push(crate::state::PendingDispatch {
            step:0, action_id:Uuid::new_v4(), inputs:json!({}),
        });
        engine.advance(&mut expired).unwrap();
        assert!(expired.failed_steps.is_empty());
        assert_eq!(expired.actions.len(),1);
        assert_eq!(expired.actions[0]["kind"],"poll");
        expired.pending_dispatches.clear();
        engine.advance(&mut expired).unwrap();
        assert_eq!(expired.failed_steps[&plan.steps[0].key()]["code"],"quote_expired");
        assert_eq!(expired.actions.len(),1);
        assert_eq!(expired.actions[0]["kind"],"analyze");
    }
}
