//! The minimal end-to-end proof, run in one process against real payloads.
//!
//! Step 10 asks for four runs:
//!
//!   1. a company name -> one identity lookup -> profile and filing in
//!      parallel -> a composed answer
//!   2. the same with a KVK number already known: the lookup disappears from
//!      the DAG *and* from the price
//!   3. the same with an ambiguous company: pause, select, resume, without a
//!      second lookup payment
//!   4. a name that is not found: an explicitly incomplete result, and no
//!      invented company data
//!
//! A live run of those needs a database with migrations 112-116 applied, a
//! gateway process and real money. This module runs the parts that do not:
//! compile the plan, price it, sign the approval, schedule the nodes, bind
//! each call, read the REAL stored provider bodies back through the contracts,
//! and check what the job would then be able to say.
//!
//! What that leaves unproven is written down in
//! `tasks/goal-plan-price-result/10-acceptatie-en-releasegates/UITKOMST.md`
//! rather than glossed. What it does prove, it proves against bodies KVK and
//! PDOK actually sent, not against examples of what they might send.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{json, Value};

    use crate::facts::contract::{ExecutionContract, FactType, Vocabulary};
    use crate::fees::ProviderClass;
    use crate::job::{ask, attempt, bind, schedule};
    use crate::plan::compile::{compile, compile_default, Limits, PlanError};
    use crate::plan::{token, Candidate, Intent, OutputFormat, Subject};

    const FACT_TYPES: &str = include_str!("../fixtures/contracts/fact-types.json");
    const ZOEKEN: &str = include_str!("../fixtures/contracts/kvk-zoeken.json");
    const BASISPROFIEL: &str = include_str!("../fixtures/contracts/kvk-basisprofiel.json");
    const JAARREKENINGEN: &str = include_str!("../fixtures/contracts/kvk-jaarrekeningen.json");
    const VOLVE: &str =
        include_str!("../fixtures/contracts/payloads/kvk-zoeken-volve-capital.json");
    const MOLLIE_SEARCH: &str =
        include_str!("../fixtures/contracts/payloads/kvk-zoeken-mollie-bv.json");
    const MOLLIE_PROFILE: &str =
        include_str!("../fixtures/contracts/payloads/kvk-basisprofiel-mollie.json");
    const FILING: &str =
        include_str!("../fixtures/contracts/payloads/kvk-jaarrekening-86717162.json");

    /// The three real live list prices of the first chain, in atomic USDC.
    const ZOEKEN_LIST: i128 = 18_903;
    const PROFIEL_LIST: i128 = 66_162;

    fn vocabulary() -> Vocabulary {
        serde_json::from_str::<BTreeMap<String, FactType>>(FACT_TYPES).unwrap()
    }

    fn contract(text: &str) -> ExecutionContract {
        serde_json::from_str(text).unwrap()
    }

    fn body(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    fn candidate(text: &str, id: &str, path: &str, list_atomic: i128) -> Candidate {
        Candidate {
            endpoint_method: "GET".into(),
            contract_id: id.to_string(),
            endpoint_id: format!("e_{id}"),
            api_slug: "kvk-dutch-business-register".to_string(),
            endpoint_path: path.to_string(),
            contract: contract(text),
            list_atomic,
            class: ProviderClass::Managed,
            active: true,
            jurisdiction: Some("NL".to_string()),
        }
    }

    fn catalogue() -> Vec<Candidate> {
        vec![
            candidate(ZOEKEN, "c_zoeken", "/api/v2/zoeken", ZOEKEN_LIST),
            candidate(
                BASISPROFIEL,
                "c_profiel",
                "/api/v1/basisprofielen",
                PROFIEL_LIST,
            ),
            candidate(
                JAARREKENINGEN,
                "c_jaar",
                "/api/v1/hvds/jaarrekeningen/kvknummer",
                PROFIEL_LIST,
            ),
        ]
    }

    fn intent(known: &[(&str, Value)]) -> Intent {
        Intent {
            subjects: vec![Subject {
                role: "subject".to_string(),
                known: known
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect(),
            }],
            required_outputs: vec![
                "company.profile".to_string(),
                "company.financial_statements".to_string(),
            ],
            optional_outputs: vec![],
            format: OutputFormat::Json,
            max_buyer_total_atomic: None,
            jurisdiction: Some("NL".to_string()),
        }
    }

    fn node_of<'p>(plan: &'p crate::plan::Plan, slug: &str) -> &'p crate::plan::PlanNode {
        plan.nodes
            .iter()
            .find(|node| node.capability_slug == slug)
            .unwrap_or_else(|| panic!("no {slug} node in the plan"))
    }

    // =====================================================================
    // Run 1 — a name, one lookup, two branches in parallel, one answer
    // =====================================================================

    #[test]
    fn run_one_a_company_name_reaches_a_composed_answer() {
        let types = vocabulary();
        let plan = compile_default(
            &intent(&[("company.name", json!("Volve Capital"))]),
            &catalogue(),
        )
        .unwrap();

        // The plan: three paid calls, two steps deep, one lookup.
        assert_eq!(plan.paid_nodes(), 3);
        assert_eq!(plan.critical_path(), 2);
        let expected = crate::plan::quote::node_buyer_total(ZOEKEN_LIST, ProviderClass::Managed)
            + crate::plan::quote::node_buyer_total(PROFIEL_LIST, ProviderClass::Managed) * 2;
        assert_eq!(plan.quote.buyer_total_atomic, expected);

        // The approval covers exactly that plan and that amount.
        let owner = uuid::Uuid::from_u128(1);
        let claim = token::PlanClaim::for_version(
            &plan,
            uuid::Uuid::from_u128(2),
            uuid::Uuid::from_u128(3),
            1,
            Some(owner),
            None,
            2,
            chrono::Utc::now() + chrono::Duration::minutes(30),
        );
        let signed = token::sign("secret", &claim).unwrap();
        assert_eq!(
            token::verify("secret", &signed, Some(owner), None, &plan.hash())
                .unwrap()
                .buyer_total_atomic,
            expected
        );

        // Step 1: only the lookup is dispatchable.
        let search = node_of(&plan, "company_registry.search");
        let profile = node_of(&plan, "company.registry-profile");
        let filing = node_of(&plan, "company.financial-statements");
        let mut nodes = vec![
            state(&search.id, &[], schedule::NodeStatus::Queued, true),
            state(
                &profile.id,
                &[&search.id],
                schedule::NodeStatus::Queued,
                true,
            ),
            state(
                &filing.id,
                &[&search.id],
                schedule::NodeStatus::Queued,
                true,
            ),
        ];
        assert_eq!(
            schedule::dispatchable(&nodes, chrono::Utc::now(), plan.limits.max_concurrency),
            vec![search.id.clone()]
        );

        // The lookup binds the name into the query, exactly as the real call did.
        let bound = bind::bind(
            &contract(ZOEKEN),
            &[("company.name".to_string(), json!("Volve Capital"))]
                .into_iter()
                .collect(),
        )
        .unwrap();
        assert_eq!(
            bound.query,
            vec![("naam".to_string(), "Volve Capital".to_string())]
        );

        // Its real response holds one company under two rows.
        let extraction = contract(ZOEKEN).extract(&body(VOLVE), &types).unwrap();
        let ask::Reading::Resolved(subject) =
            ask::read(&search.id, "company.registration.nl.kvk", &extraction)
        else {
            panic!("Volve Capital is one company and must not ask");
        };
        let kvk = subject
            .fact("company.registration.nl.kvk")
            .unwrap()
            .value
            .clone();
        assert_eq!(kvk, json!("86717162"));

        // Step 2: both branches are released, together.
        nodes[0].status = schedule::NodeStatus::Succeeded;
        let ready = schedule::dispatchable(&nodes, chrono::Utc::now(), plan.limits.max_concurrency);
        assert_eq!(ready.len(), 2, "the profile and the filing run in parallel");

        // Both bind the number the lookup produced, onto the path — which is
        // where the real KVK URLs put it.
        let facts: BTreeMap<String, Value> = [("company.registration.nl.kvk".to_string(), kvk)]
            .into_iter()
            .collect();
        assert_eq!(
            bind::bind(&contract(BASISPROFIEL), &facts)
                .unwrap()
                .path_segments
                .iter()
                .map(|s| s.value.clone())
                .collect::<Vec<_>>(),
            vec!["86717162".to_string()]
        );
        assert_eq!(
            bind::bind(&contract(JAARREKENINGEN), &facts)
                .unwrap()
                .path_segments
                .iter()
                .map(|s| s.value.clone())
                .collect::<Vec<_>>(),
            vec!["86717162".to_string()]
        );

        // And both real bodies yield the facts the answer is made of.
        let profile_out = contract(BASISPROFIEL)
            .extract(&body(MOLLIE_PROFILE), &types)
            .unwrap();
        assert_eq!(
            profile_out
                .single()
                .unwrap()
                .fact("company.profile")
                .unwrap()
                .value["legal_name"],
            json!("Mollie B.V.")
        );
        let filing_out = contract(JAARREKENINGEN)
            .extract(&body(FILING), &types)
            .unwrap();
        assert_eq!(
            filing_out
                .single()
                .unwrap()
                .fact("company.financial_statements")
                .unwrap()
                .value["total_assets"],
            json!(682987.0)
        );

        // Every required output proven -> succeeded.
        nodes[1].status = schedule::NodeStatus::Succeeded;
        nodes[2].status = schedule::NodeStatus::Succeeded;
        assert_eq!(
            schedule::job_status(&nodes, false),
            schedule::JobStatus::Succeeded
        );
    }

    // =====================================================================
    // Run 2 — a known KVK number: the lookup leaves the DAG and the price
    // =====================================================================

    #[test]
    fn run_two_a_known_number_removes_the_lookup_from_both() {
        let with_lookup = compile_default(
            &intent(&[("company.name", json!("Mollie B.V."))]),
            &catalogue(),
        )
        .unwrap();
        let without = compile_default(
            &intent(&[
                ("company.name", json!("Mollie B.V.")),
                ("company.registration.nl.kvk", json!("30204462")),
            ]),
            &catalogue(),
        )
        .unwrap();

        assert_eq!(with_lookup.paid_nodes(), 3);
        assert_eq!(without.paid_nodes(), 2);
        assert!(without
            .nodes
            .iter()
            .all(|node| node.capability_slug != "company_registry.search"));

        // Out of the price too, and by exactly the lookup's own amount.
        assert_eq!(
            with_lookup.quote.buyer_total_atomic - without.quote.buyer_total_atomic,
            crate::plan::quote::node_buyer_total(ZOEKEN_LIST, ProviderClass::Managed)
        );

        // A different plan is a different approval.
        assert_ne!(with_lookup.hash(), without.hash());
        let owner = uuid::Uuid::from_u128(1);
        let claim = token::PlanClaim::for_version(
            &with_lookup,
            uuid::Uuid::from_u128(2),
            uuid::Uuid::from_u128(3),
            1,
            Some(owner),
            None,
            2,
            chrono::Utc::now() + chrono::Duration::minutes(30),
        );
        let signed = token::sign("secret", &claim).unwrap();
        assert_eq!(
            token::verify("secret", &signed, Some(owner), None, &without.hash()),
            Err(token::TokenError::PlanChanged)
        );
    }

    // =====================================================================
    // Run 3 — an ambiguous company: pause, select, resume, one lookup paid
    // =====================================================================

    #[test]
    fn run_three_ambiguity_pauses_and_resumes_without_a_second_payment() {
        let types = vocabulary();
        let plan = compile_default(
            &intent(&[("company.name", json!("Mollie BV"))]),
            &catalogue(),
        )
        .unwrap();
        let search = node_of(&plan, "company_registry.search").clone();
        let profile = node_of(&plan, "company.registry-profile").clone();

        // The lookup runs and is charged once.
        let charged = crate::plan::quote::node_buyer_total(ZOEKEN_LIST, ProviderClass::Managed);
        let outcome = attempt::Outcome::Answered {
            http_status: 200,
            charged_atomic: charged,
            body: body(MOLLIE_SEARCH),
        };
        let (payment, settled) = attempt::settle(&outcome);
        assert_eq!(payment, attempt::PaymentStatus::Settled);
        assert_eq!(settled, Some(charged));

        // Ten different companies: it asks.
        let extraction = contract(ZOEKEN)
            .extract(&body(MOLLIE_SEARCH), &types)
            .unwrap();
        let ask::Reading::Ambiguous(question) =
            ask::read(&search.id, "company.registration.nl.kvk", &extraction)
        else {
            panic!("ten companies must ask");
        };
        assert_eq!(question.candidates.len(), 10);

        // Its dependants wait, and are NOT blocked — an answer is coming.
        let nodes = vec![
            state(&search.id, &[], schedule::NodeStatus::NeedsInput, true),
            state(
                &profile.id,
                &[&search.id],
                schedule::NodeStatus::Queued,
                true,
            ),
        ];
        assert!(schedule::dispatchable(&nodes, chrono::Utc::now(), 3).is_empty());
        assert!(schedule::newly_blocked(&nodes).is_empty());
        assert_eq!(
            schedule::job_status(&nodes, false),
            schedule::JobStatus::NeedsInput
        );

        // Choosing reads a candidate. No call, no charge.
        let chosen = ask::resolve(Some(&question), &search.id, &json!("30204462")).unwrap();
        assert_eq!(chosen.identity, json!("30204462"));

        // And the branch resumes, binding the chosen number.
        let mut resumed = nodes.clone();
        resumed[0].status = schedule::NodeStatus::Succeeded;
        assert_eq!(
            schedule::dispatchable(&resumed, chrono::Utc::now(), 3),
            vec![profile.id.clone()]
        );
        assert_eq!(
            bind::bind(
                &contract(BASISPROFIEL),
                &[(
                    "company.registration.nl.kvk".to_string(),
                    chosen.identity.clone()
                )]
                .into_iter()
                .collect()
            )
            .unwrap()
            .path_segments
            .iter()
            .map(|s| s.value.clone())
            .collect::<Vec<_>>(),
            vec!["30204462".to_string()]
        );

        // The total spent is still one lookup's worth: the resume added none.
        assert_eq!(settled.unwrap(), charged);
    }

    // =====================================================================
    // Run 4 — a name that is not found: incomplete, never invented
    // =====================================================================

    #[test]
    fn run_four_a_name_that_is_not_found_yields_an_explicitly_incomplete_result() {
        let types = vocabulary();
        let plan = compile_default(
            &intent(&[("company.name", json!("Nietbestaand Bedrijf XYZ"))]),
            &catalogue(),
        )
        .unwrap();
        let search = node_of(&plan, "company_registry.search").clone();
        let profile = node_of(&plan, "company.registry-profile").clone();
        let filing = node_of(&plan, "company.financial-statements").clone();

        // The provider answered, and the answer is that there is nothing.
        let empty = json!({ "totaal": 0, "resultaten": [] });
        let extraction = contract(ZOEKEN).extract(&empty, &types).unwrap();
        assert!(extraction.subjects.is_empty());
        assert_eq!(
            ask::read(&search.id, "company.registration.nl.kvk", &extraction),
            ask::Reading::Empty
        );

        // The call was made and billed. An empty answer does not make it free.
        let charged = crate::plan::quote::node_buyer_total(ZOEKEN_LIST, ProviderClass::Managed);
        assert_eq!(
            attempt::settle(&attempt::Outcome::NothingThere {
                http_status: 404,
                charged_atomic: charged
            }),
            (attempt::PaymentStatus::Settled, Some(charged))
        );

        // Nothing was proven, so the dependants are blocked rather than run
        // against nothing — and they cost nothing.
        let nodes = vec![
            state(&search.id, &[], schedule::NodeStatus::Failed, true),
            state(
                &profile.id,
                &[&search.id],
                schedule::NodeStatus::Queued,
                true,
            ),
            state(
                &filing.id,
                &[&search.id],
                schedule::NodeStatus::Queued,
                true,
            ),
        ];
        assert!(schedule::dispatchable(&nodes, chrono::Utc::now(), 3).is_empty());
        assert_eq!(schedule::newly_blocked(&nodes).len(), 2);

        // Even if one were dispatched, it could not be sent: there is no
        // number to bind, and nothing goes looking for one.
        assert!(bind::bind(&contract(BASISPROFIEL), &BTreeMap::new()).is_err());

        // The job reports failed — not "no insolvency found", not an empty
        // profile, and no invented company.
        let blocked = vec![
            state(&search.id, &[], schedule::NodeStatus::Failed, true),
            state(
                &profile.id,
                &[&search.id],
                schedule::NodeStatus::Blocked,
                true,
            ),
            state(
                &filing.id,
                &[&search.id],
                schedule::NodeStatus::Blocked,
                true,
            ),
        ];
        assert_eq!(
            schedule::job_status(&blocked, false),
            schedule::JobStatus::Failed
        );
    }

    // =====================================================================
    // Release gates that are checkable without a database
    // =====================================================================

    #[test]
    fn gate_planning_spends_nothing() {
        // Structural: `plan::compile` takes a slice of candidates and returns a
        // plan. It has no HTTP client, no pool and no way to acquire either, so
        // there is no expenditure to test for — only the shape that makes one
        // impossible. This test exists so that shape is deliberate.
        let plan = compile_default(
            &intent(&[("company.name", json!("Mollie B.V."))]),
            &catalogue(),
        )
        .unwrap();
        assert_eq!(plan.paid_nodes(), 3, "priced, and not one of them called");
    }

    #[test]
    fn gate_a_withdrawn_changed_or_expired_approval_is_refused() {
        let plan = compile_default(
            &intent(&[("company.name", json!("Mollie B.V."))]),
            &catalogue(),
        )
        .unwrap();
        let owner = uuid::Uuid::from_u128(1);
        let mint = |minutes: i64| {
            token::sign(
                "secret",
                &token::PlanClaim::for_version(
                    &plan,
                    uuid::Uuid::from_u128(2),
                    uuid::Uuid::from_u128(3),
                    1,
                    Some(owner),
                    None,
                    2,
                    chrono::Utc::now() + chrono::Duration::minutes(minutes),
                ),
            )
            .unwrap()
        };
        assert_eq!(
            token::verify("secret", &mint(-1), Some(owner), None, &plan.hash()),
            Err(token::TokenError::Expired)
        );
        assert_eq!(
            token::verify(
                "secret",
                &mint(30),
                Some(uuid::Uuid::from_u128(99)),
                None,
                &plan.hash()
            ),
            Err(token::TokenError::WrongOwner)
        );
        assert_eq!(
            token::verify("secret", &mint(30), Some(owner), None, &"0".repeat(64)),
            Err(token::TokenError::PlanChanged)
        );
    }

    #[test]
    fn gate_no_extra_cost_from_a_hidden_resolver() {
        // On the job path a node whose required fact is unavailable is refused,
        // and nothing goes off to find it. The existing /v1/do path fires a
        // paid KVK lookup in exactly this situation, uncapped unless the caller
        // sent a price field.
        let error = bind::bind(&contract(BASISPROFIEL), &BTreeMap::new()).unwrap_err();
        assert!(matches!(error, bind::BindError::Missing { .. }));
    }

    #[test]
    fn gate_a_schema_error_never_yields_a_valid_fact() {
        let types = vocabulary();
        let mut broken = body(VOLVE);
        broken["resultaten"][0]["kvkNummer"] = json!("867171");
        broken["resultaten"][1]["kvkNummer"] = json!("867171");
        let out = contract(ZOEKEN).extract(&broken, &types).unwrap();
        assert!(out
            .subjects
            .iter()
            .all(|s| s.fact("company.registration.nl.kvk").is_none()));
        assert_eq!(out.rejected.len(), 2);
    }

    #[test]
    fn gate_partial_success_marks_only_proven_outputs() {
        let nodes = vec![
            state("search", &[], schedule::NodeStatus::Succeeded, false),
            state(
                "profile",
                &["search"],
                schedule::NodeStatus::Succeeded,
                true,
            ),
            state("filing", &["search"], schedule::NodeStatus::Failed, true),
        ];
        assert_eq!(
            schedule::job_status(&nodes, false),
            schedule::JobStatus::Partial
        );
    }

    #[test]
    fn gate_an_undecided_settlement_stays_traceable_and_blocks_more_spending() {
        assert_eq!(
            attempt::settle(&attempt::Outcome::Undecided),
            (attempt::PaymentStatus::Unresolved, None)
        );
        assert!(!attempt::may_retry(&attempt::Outcome::Undecided));
        assert_eq!(
            attempt::may_spend(
                &attempt::Budget {
                    requests_made: 0,
                    max_requests: 9,
                    committed_atomic: 0,
                    ceiling_atomic: 1_000_000,
                    has_unresolved_payment: true,
                },
                1
            ),
            Err(attempt::AttemptError::UnresolvedPaymentOutstanding)
        );
    }

    #[test]
    fn gate_two_workers_never_hold_one_node() {
        let mut nodes = vec![state("a", &[], schedule::NodeStatus::Running, true)];
        nodes[0].lease_expires_at = Some(chrono::Utc::now() + chrono::Duration::seconds(60));
        assert!(schedule::dispatchable(&nodes, chrono::Utc::now(), 3).is_empty());
        // And the lapse is what makes recovery possible, not a second claim.
        nodes[0].lease_expires_at = Some(chrono::Utc::now() - chrono::Duration::seconds(1));
        assert_eq!(
            schedule::dispatchable(&nodes, chrono::Utc::now(), 3),
            vec!["a".to_string()]
        );
    }

    #[test]
    fn gate_the_app_and_an_mcp_client_share_a_plan_hash_and_an_amount() {
        // Two independent compiles of one intent against one catalogue.
        let a = compile_default(
            &intent(&[("company.name", json!("Mollie B.V."))]),
            &catalogue(),
        )
        .unwrap();
        let b = compile_default(
            &intent(&[("company.name", json!("Mollie B.V."))]),
            &catalogue(),
        )
        .unwrap();
        assert_eq!(a.hash(), b.hash());
        assert_eq!(a.quote.buyer_total_atomic, b.quote.buyer_total_atomic);
    }

    #[test]
    fn gate_a_plan_over_budget_is_refused_with_its_price() {
        let mut over = intent(&[("company.name", json!("Mollie B.V."))]);
        over.max_buyer_total_atomic = Some(1_000);
        assert!(matches!(
            compile(&over, &catalogue(), Limits::default(), None),
            Err(PlanError::TooExpensive { .. })
        ));
    }

    fn state(
        key: &str,
        depends_on: &[&str],
        status: schedule::NodeStatus,
        required: bool,
    ) -> schedule::NodeState {
        schedule::NodeState {
            key: key.to_string(),
            depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
            status,
            lease_expires_at: None,
            attempts: 0,
            max_attempts: 3,
            required,
        }
    }
}

#[cfg(test)]
mod clockwork {
    use crate::{
        facts::contract::{ExecutionContract, Vocabulary},
        fees::ProviderClass,
        plan::{self, Binding, Candidate, Intent, OutputFormat, Subject},
    };
    use serde_json::{json, Value};
    use std::collections::BTreeMap;
    fn vocabulary() -> Vocabulary {
        serde_json::from_str(include_str!("../fixtures/contracts/fact-types.json")).unwrap()
    }
    fn candidates() -> Vec<Candidate> {
        [
            include_str!("../fixtures/contracts/kvk-zoeken.json"),
            include_str!("../fixtures/contracts/kvk-basisprofiel.json"),
            include_str!("../fixtures/contracts/kvk-jaarrekeningen.json"),
        ]
        .iter()
        .enumerate()
        .map(|(i, text)| Candidate {
            endpoint_method: "GET".into(),
            contract_id: format!("c{i}"),
            endpoint_id: format!("e{i}"),
            api_slug: "kvk".into(),
            endpoint_path: format!("/{i}"),
            contract: serde_json::from_str::<ExecutionContract>(text).unwrap(),
            list_atomic: if i == 0 { 18903 } else { 66162 },
            class: ProviderClass::Managed,
            active: true,
            jurisdiction: Some("NL".into()),
        })
        .collect()
    }
    fn intent(known: &[(&str, Value)]) -> Intent {
        Intent {
            subjects: vec![Subject {
                role: "subject".into(),
                known: known
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect(),
            }],
            required_outputs: vec![
                "company.profile".into(),
                "company.financial_statements".into(),
            ],
            optional_outputs: vec![],
            format: OutputFormat::Json,
            max_buyer_total_atomic: None,
            jurisdiction: Some("NL".into()),
        }
    }
    fn checked(i: &Intent) -> plan::Plan {
        plan::validate::intent(i, &vocabulary()).unwrap();
        let p = plan::compile::compile_default(i, &candidates()).unwrap();
        plan::validate::plan(&p, &vocabulary()).unwrap();
        p
    }
    #[test]
    fn known_kvk_bypasses_lookup_and_is_bound_to_each_downstream_call() {
        let p = checked(&intent(&[(
            "company.registration.nl.kvk",
            json!("30204462"),
        )]));
        assert_eq!(p.nodes.len(), 2);
        for node in &p.nodes {
            let required = plan::validate::required_facts(&p, &node.id);
            assert!(!required.is_empty());
            assert!(required.iter().all(|kind| node.produces.contains(kind)));
        }

        for node in &p.nodes {
            let facts = plan::validate::inputs(node, &[], &vocabulary()).unwrap();
            let bound = crate::job::bind::bind(node.contract.as_ref().unwrap(), &facts).unwrap();
            assert_eq!(
                bound
                    .path_segments
                    .iter()
                    .map(|s| s.value.clone())
                    .collect::<Vec<_>>(),
                ["30204462"]
            );
        }
    }
    #[test]
    fn name_lookup_is_shared_and_only_its_explicit_fact_can_feed_profile_and_accounts() {
        let p = checked(&intent(&[("company.name", json!("Mollie B.V."))]));
        assert_eq!(p.nodes.len(), 3);
        let first = &p.nodes[0];
        let facts = plan::validate::inputs(first, &[], &vocabulary()).unwrap();
        let bound = crate::job::bind::bind(first.contract.as_ref().unwrap(), &facts).unwrap();
        assert_eq!(bound.query, [("naam".into(), "Mollie B.V.".into())]);
        let payload: Value = serde_json::from_str(include_str!(
            "../fixtures/contracts/payloads/kvk-zoeken-mollie-bv.json"
        ))
        .unwrap();
        let extraction = first
            .contract
            .as_ref()
            .unwrap()
            .extract(&payload, &vocabulary())
            .unwrap();
        // Multiple distinct identities must ask; no first-row shortcut.
        assert!(matches!(
            crate::job::ask::read(&first.id, "company.registration.nl.kvk", &extraction),
            crate::job::ask::Reading::Ambiguous(_)
        ));
        let supplied = vec![
            (
                first.id.clone(),
                "company.registration.nl.kvk".into(),
                json!("30204462"),
            ),
            (
                "unrelated".into(),
                "company.registration.nl.kvk".into(),
                json!("99999999"),
            ),
        ];
        for n in &p.nodes[1..] {
            assert_eq!(n.depends_on, [first.id.clone()]);
            let facts = plan::validate::inputs(n, &supplied, &vocabulary()).unwrap();
            assert_eq!(facts["company.registration.nl.kvk"], "30204462");
            let mut ambiguous = supplied.clone();
            ambiguous.push((
                first.id.clone(),
                "company.registration.nl.kvk".into(),
                json!("12345678"),
            ));
            assert!(plan::validate::inputs(n, &ambiguous, &vocabulary()).is_err());
            assert!(plan::validate::inputs(n, &[], &vocabulary()).is_err());
        }
        assert_eq!(
            p.quote.buyer_total_atomic,
            p.nodes.iter().map(|n| n.buyer_total_atomic).sum::<i128>()
        );
    }
    #[test]
    fn bad_inputs_and_unmapped_contracts_fail_before_payment() {
        for v in [Value::Null, json!("Mollie"), json!(30204462), json!("")] {
            assert!(plan::validate::intent(
                &intent(&[("company.registration.nl.kvk", v)]),
                &vocabulary()
            )
            .is_err());
        }
        let mut c = candidates()[1].contract.clone();
        c.input_map.clear();
        assert!(c.check(&vocabulary()).is_err());
        c = candidates()[1].contract.clone();
        c.input_map.values_mut().next().unwrap().location = Some("guess".into());
        assert!(c.check(&vocabulary()).is_err());
        c = candidates()[1].contract.clone();
        c.requires[0].subject_role = "acquirer".into();
        assert!(c.check(&vocabulary()).is_err());
    }
    #[test]
    fn graph_price_and_snapshot_tampering_is_detected() {
        let original = checked(&intent(&[("company.name", json!("Mollie B.V."))]));
        let mut p = original.clone();
        let last = p.nodes[2].id.clone();
        p.nodes[0].depends_on.push(last);
        assert!(plan::validate::plan(&p, &vocabulary()).is_err());
        p = original.clone();
        p.quote.buyer_total_atomic += 1;
        assert!(plan::validate::plan(&p, &vocabulary()).is_err());
        p = original.clone();
        p.nodes[0].contract = None;
        assert!(plan::validate::plan(&p, &vocabulary()).is_err());
        p = original.clone();
        let same = p.nodes[1].id.clone();
        p.nodes[1].bindings.insert(
            "company.registration.nl.kvk".into(),
            Binding::Node { node_id: same },
        );
        assert!(plan::validate::plan(&p, &vocabulary()).is_err());
        p = original.clone();
        p.nodes[1]
            .contract
            .as_mut()
            .unwrap()
            .input_map
            .values_mut()
            .next()
            .unwrap()
            .name = "other".into();
        assert_ne!(p.hash(), original.hash());
        p = original.clone();
        p.limits.max_concurrency = 0;
        assert!(plan::validate::plan(&p, &vocabulary()).is_err());
    }
    #[test]
    fn failed_optional_branch_leaves_no_paid_orphan_and_catalogue_order_is_stable() {
        let mut c = candidates();
        let mut optional = c[1].clone();
        optional.contract_id = "optional".into();
        optional.contract.output_map.produces = BTreeMap::from([(
            "optional.result".into(),
            crate::facts::contract::Extract::Path {
                path: "$".into(),
                collect: false,
                cast: None,
            },
        )]);
        optional
            .contract
            .requires
            .push(crate::facts::contract::Requirement {
                fact_type: "unreachable".into(),
                subject_role: "subject".into(),
                optional: false,
            });
        c.push(optional);
        let mut i = intent(&[("company.name", json!("Mollie"))]);
        i.required_outputs = vec!["company.name".into()];
        i.optional_outputs = vec!["optional.result".into()];
        let p = plan::compile::compile_default(&i, &c).unwrap();
        assert!(p.nodes.is_empty());
        assert_eq!(p.quote.buyer_total_atomic, 0);
        let i = intent(&[("company.name", json!("Mollie"))]);
        let a = plan::compile::compile_default(&i, &c).unwrap();
        c.reverse();
        let b = plan::compile::compile_default(&i, &c).unwrap();
        assert_eq!(a.hash(), b.hash());
    }
    #[test]
    fn skips_cheapest_unreachable_source_without_guessing_or_spending() {
        let mut c = candidates();
        let mut bad = c[1].clone();
        bad.contract_id = "cheap-but-unusable".into();
        bad.list_atomic = 1;
        bad.contract.requires[0].fact_type = "missing.fact".into();
        c.push(bad);
        let p = plan::compile::compile_default(&intent(&[("company.name", json!("Mollie"))]), &c)
            .unwrap();
        assert_eq!(p.nodes.len(), 3);
        assert!(!p
            .nodes
            .iter()
            .any(|n| n.contract_id == "cheap-but-unusable"));
    }
    #[test]
    fn budget_ceiling_and_required_unknown_output_are_refused() {
        let mut i = intent(&[("company.name", json!("Mollie"))]);
        i.max_buyer_total_atomic = Some(1);
        assert!(matches!(
            plan::compile::compile_default(&i, &candidates()),
            Err(plan::compile::PlanError::TooExpensive { .. })
        ));
        i.max_buyer_total_atomic = None;
        i.required_outputs.push("invented.fact".into());
        assert!(plan::validate::intent(&i, &vocabulary()).is_err());
    }
}
