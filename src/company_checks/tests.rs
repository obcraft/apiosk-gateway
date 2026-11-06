use super::*;

fn customer() -> Company {
    Company {
        name: "Acme B.V.".into(),
        kvk_number: "01234567".into(),
    }
}
fn source(slug: &str, data: Value) -> SavedSource {
    SavedSource {
        result_ref: Uuid::new_v4(),
        retrieved_at: "2026-10-21T10:00:00Z".into(),
        kvk_number: Some("01234567".into()),
        document: json!({"source":{"provider":slug,"name":slug},"coverage":"source_response","data":data}),
    }
}
fn report(sources: &[SavedSource], previous: Option<&Value>, reported: &BTreeSet<String>) -> Value {
    snapshot(
        &[customer()],
        sources,
        previous,
        reported,
        "2026-10-21T10:00:00Z",
        json!({"total_charged":"1234","currency":"USD"}),
        "succeeded",
    )
}
fn screening() -> SavedSource {
    source(
        "opensanctions",
        json!({"responses":{"q1":{"total":{"value":1},"results":[{"id":"candidate-1","match":true,"score":0.95,"properties":{"topics":["sanction"]},"datasets":["ofac"]}]}}}),
    )
}
#[test]
fn unreported_screening_is_retained_with_original_evidence_across_source_failure() {
    let s = screening();
    let a = report(std::slice::from_ref(&s), None, &BTreeSet::new());
    let b = report(&[], Some(&a), &BTreeSet::new());
    assert_eq!(b["new_signal_count"], 1);
    assert_eq!(b["new_signals"][0]["signal_id"], a["new_signals"][0]["signal_id"]);
    assert_eq!(b["new_signals"][0]["carried_forward"], true);
    assert_eq!(b["new_signals"][0]["evidence"], a["new_signals"][0]["evidence"]);
    assert_eq!(b["sources"][0]["historical"], true);
    assert_eq!(b["source_count"], 0);
    assert_eq!(b["coverage"][2]["status"], "unknown");
    let mail = delivery::message(&b);
    assert!(mail["body"].as_str().unwrap().contains("Previously observed"));
    let ids = BTreeSet::from([a["new_signals"][0]["signal_id"].as_str().unwrap().to_owned()]);
    assert_eq!(report(&[], Some(&b), &ids)["new_signal_count"], 0);
    let refreshed = report(&[s], Some(&b), &BTreeSet::new());
    assert_eq!(refreshed["new_signal_count"], 1);
    assert_ne!(refreshed["new_signals"][0]["carried_forward"], true);
}
#[test]
fn validates_company_identity_without_invoice_or_csv() {
    assert!(validate_companies(&[customer()]).is_ok());
    assert!(validate_companies(&[customer(), customer()]).is_err());
    let mut c = customer();
    c.kvk_number = "1234567".into();
    assert!(validate_companies(&[c]).is_err());
    assert!(!question(&[customer()]).contains("insolvency"));
    assert!(question(&[customer()]).contains("news articles"));
    assert!(question(&[customer()]).contains("01234567"));
}
#[test]
fn every_company_gets_the_three_fixed_source_checks() {
    let mut second = customer();
    second.kvk_number = "76543210".into();
    let i = recipe(&[customer(), second]);
    assert_eq!(i.subjects.len(), 2);
    assert_eq!(i.subjects[1].values["company_registry.kvknummer"], "76543210");
    assert_eq!(i.subjects[0].values["web.query"], "Acme B.V.");
    // Only the news topic returns the publication dates news.rs requires.
    assert_eq!(i.subjects[0].values["web_tools.input.topic"], "news");
    let checks: Vec<_> = i
        .goals
        .iter()
        .map(|g| (g.subject, g.capability.as_str(), g.constraints["source"].as_str().unwrap()))
        .collect();
    for subject in 0..2 {
        assert!(checks.contains(&(subject, "company.registry-profile", "kvk-dutch-business-register")));
        assert!(checks.contains(&(subject, "company.sanctions-screen", "opensanctions")));
        assert!(checks.contains(&(subject, "web.search", crate::web_search::SLUG)));
    }
    assert_eq!(i.goals.len(), 6);
}
#[test]
fn an_uploaded_client_csv_becomes_the_company_list() {
    // Dutch Excel: BOM, semicolons, quoted name, extra column, dropped zero.
    let csv = "\u{feff}Klant;KvK-nummer;Accountmanager\n\"Acme; Holding B.V.\";1234567;Sam\n\nBeta B.V.;76543210;\n";
    assert_eq!(
        companies_from_csv(csv).unwrap(),
        vec![
            Company { name: "Acme; Holding B.V.".into(), kvk_number: "01234567".into() },
            Company { name: "Beta B.V.".into(), kvk_number: "76543210".into() },
        ]
    );
    let comma = "name,kvk_number\nHeineken N.V.,33011433\nMollie B.V.,30204462\nCoolblue B.V.,24330087\n";
    assert_eq!(companies_from_csv(comma).unwrap().len(), 3);
}
fn read(question: &str) -> Option<(Vec<(String, String)>, Vec<String>)> {
    let intent = direct_intent(question)?;
    let companies = intent
        .subjects
        .iter()
        .map(|s| (s.label.clone(), s.values["company_registry.kvknummer"].as_str().unwrap().to_owned()))
        .collect();
    let sources = intent
        .goals
        .iter()
        .filter(|g| g.subject == 0)
        .map(|g| g.constraints["source"].as_str().unwrap().to_owned())
        .collect();
    Some((companies, sources))
}
fn demo() -> Vec<(String, String)> {
    [("Heineken N.V.", "33011433"), ("Mollie B.V.", "30204462"), ("Coolblue B.V.", "24330087")]
        .iter()
        .map(|(n, k)| (n.to_string(), k.to_string()))
        .collect()
}
#[test]
fn a_chatbot_company_check_reads_the_client_list_without_a_model() {
    let all = vec!["kvk-dutch-business-register".to_owned(), "opensanctions".into(), crate::web_search::SLUG.into()];
    for question in [
        "Run the company check on this client list:\nname,kvk\nHeineken N.V.,33011433\nMollie B.V.,30204462\nCoolblue B.V.,24330087\nThanks.",
        "Company check for my clients Heineken N.V. (KVK 33011433), Mollie B.V. (KVK 30204462) and Coolblue B.V. (KVK 24330087).",
        "Use Apiosk to run the company check on this client list:\n- Heineken N.V. — KVK: 33011433\n- Mollie B.V. — KVK: 30204462\n- Coolblue B.V. — KVK: 24330087",
        "Check Heineken N.V. (KvK-nummer 33011433), Mollie B.V. (kvk 30204462) en Coolblue B.V. (kvk 24330087) for sanctions and news.",
        "Company check:\n| Company | KVK |\n|---|---|\n| Heineken N.V. | 33011433 |\n| Mollie B.V. | 30204462 |\n| Coolblue B.V. | 24330087 |",
    ] {
        assert_eq!(read(question), Some((demo(), all.clone())), "{question}");
    }
    // Only the requested checks; the KVK profile anchors the identity.
    let (companies, sources) =
        read("The current KVK registry profile and an OpenSanctions sanctions screening for Heineken N.V. (KVK 33011433).").unwrap();
    assert_eq!(companies, demo()[..1]);
    assert_eq!(sources, ["kvk-dutch-business-register", "opensanctions"]);
    let intent = direct_intent("Sanctions screening and KVK profile for bol.com B.V. (KVK 1234567)").unwrap();
    assert_eq!((intent.subjects[0].label.as_str(), &intent.subjects[0].values["company_registry.kvknummer"]), ("bol.com B.V.", &json!("01234567")));
    assert!(!intent.subjects[0].values.contains_key("web.query"));
}
#[test]
fn an_unclear_company_question_is_left_to_the_ordinary_interpretation() {
    for question in [
        // One check: an ordinary lookup.
        "KVK profile for Heineken N.V. (KVK 33011433)",
        // No KVK numbers, a number without a name, or more than three companies.
        "Run the company check on Heineken, Mollie and Coolblue",
        "Company check for KVK 33011433",
        "Company check: A B.V. (KVK 11111111), B B.V. (KVK 22222222), C B.V. (KVK 33333333), D B.V. (KVK 44444444)",
        "Company check: A B.V. (KVK 11111111) and A B.V. (KVK 11111111)",
    ] {
        assert!(direct_intent(question).is_none(), "{question}");
    }
}
#[test]
fn a_client_csv_is_refused_rather_than_guessed() {
    let error = |csv: &str| companies_from_csv(csv).unwrap_err().to_string();
    assert!(error("name,city\nAcme B.V.,Utrecht\n").contains("KVK number column"));
    assert!(error("name,kvk\nAcme B.V.,12AB5678\n").contains("Row 2"));
    assert!(error("name,kvk\nAcme B.V.,1234\n").contains("eight-digit"));
    assert!(error("name,kvk\nA,11111111\nB,22222222\nC,33333333\nD,44444444\n").contains("at most three"));
    assert!(error("name,kvk\nA,11111111\nB,11111111\n").contains("Duplicate"));
    assert!(error("").contains("empty"));
}
#[test]
fn a_missing_named_source_refuses_the_check_instead_of_planning_part_of_it() {
    // The fixture catalog has KVK and OpenSanctions but no Tavily operation.
    let catalog = crate::catalog::Catalog::build(
        serde_json::from_str(include_str!("../../tests/fixtures/workflows/catalog.json")).unwrap(),
    );
    assert!(!catalog.services.iter().any(|s| s.api_slug == "tavily"));
    assert!(!matches!(
        crate::workflow_catalog::plan_preview(&catalog, &recipe(&[customer()])),
        crate::resolver::Resolution::Plan(_)
    ));
}
#[tokio::test]
async fn a_recipe_request_fails_closed_when_the_store_is_unreachable() {
    let http = reqwest::Client::new();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_secs(2))
        .connect_lazy("postgresql://postgres@127.0.0.1:1/unused")
        .unwrap();
    let store = crate::store::Store { pool: pool.clone() };
    let engine = crate::engine::Engine {
        store: store.clone(),
        catalog: crate::catalog::CatalogCache::new(pool),
        parser: crate::parser::Parser::new(http.clone(), "http://127.0.0.1:1/".into(), "unused".into(), "unused".into()),
        signer: crate::state::Signer::new(vec![8; 32]).unwrap(),
        app_url: "http://localhost".into(),
        executor: crate::execution::Executor { store, http, supabase_url: "http://127.0.0.1:1".into(), enabled: false },
    };
    let p = crate::auth::Principal { owner: Uuid::new_v4(), connection: Some(Uuid::new_v4()), workspace: None };
    let req = crate::protocol::DiscoverRequest {
        request_id: Uuid::new_v4(),
        question: question(&[customer()]),
        state: None,
        context_delta: Default::default(),
    };
    // Without the request store nothing is claimed, planned or bought: the
    // error reaches the route, which answers it instead of a price.
    assert!(engine.discover_recipe(&p, req, recipe(&[customer()])).await.is_err());
}
#[test]
fn no_sources_is_unknown_not_clear_or_complete() {
    let r = report(&[], None, &BTreeSet::new());
    assert_eq!(r["status"], "partial");
    assert_eq!(r["coverage"].as_array().unwrap().len(), 3);
    assert!(r["coverage"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["status"] == "unknown"));
    assert_eq!(r["notification_required"], false);
    assert_eq!(r["coverage_attention_required"], true);
    assert_eq!(r["schedule"]["created"], false);
}
#[test]
fn exact_registry_identity_required_and_registry_does_not_cover_news() {
    let mut s = source(
        "kvk-dutch-business-register",
        json!({"kvkNummer":"99999999","naam":"Acme B.V."}),
    );
    assert_eq!(
        report(&[s.clone()], None, &BTreeSet::new())["coverage"][0]["status"],
        "unknown"
    );
    s.document["data"]["kvkNummer"] = json!("01234567");
    let r = report(&[s], None, &BTreeSet::new());
    assert_eq!(r["coverage"][0]["status"], "checked");
    assert_eq!(r["coverage"][1]["status"], "unknown");
}
#[test]
fn baseline_distinguishes_first_observation_from_change_with_exact_evidence() {
    let mut s = source(
        "kvk-dutch-business-register",
        json!({"kvkNummer":"01234567","naam":"Acme B.V."}),
    );
    let a = report(std::slice::from_ref(&s), None, &BTreeSet::new());
    assert_eq!(a["new_signal_count"], 0);
    s.document["data"]["naam"] = json!("Acme Nieuw B.V.");
    let b = report(&[s.clone()], Some(&a), &BTreeSet::new());
    assert_eq!(b["new_signal_count"], 1);
    assert_eq!(b["new_signals"][0]["previous"], "Acme B.V.");
    assert_eq!(
        b["new_signals"][0]["evidence"][0]["value"],
        s.document.pointer("/data/naam").unwrap().clone()
    );
}
#[test]
fn reported_signal_survives_receipt_timestamp_score_changes_and_missing_intermediate_result() {
    let mut s = screening();
    let a = report(std::slice::from_ref(&s), None, &BTreeSet::new());
    assert_eq!(a["new_signals"][0]["change"], "first_observation");
    let reported = BTreeSet::from([a["new_signals"][0]["signal_id"]
        .as_str()
        .unwrap()
        .to_owned()]);
    s.result_ref = Uuid::new_v4();
    s.retrieved_at = "2026-10-28T10:00:00Z".into();
    s.document["data"]["responses"]["q1"]["results"][0]["score"] = json!(0.99);
    let gap = report(&[], Some(&a), &reported);
    let b = report(&[s], Some(&gap), &reported);
    assert_eq!(b["new_signal_count"], 0);
    assert_eq!(b["signals"][0]["previously_reported"], true);
}
#[test]
fn unreported_signals_remain_available_and_matches_are_never_confirmed_identity() {
    let s = screening();
    let a = report(std::slice::from_ref(&s), None, &BTreeSet::new());
    let b = report(&[s], Some(&a), &BTreeSet::new());
    assert_eq!(b["new_signal_count"], 1);
    assert_eq!(b["signals"][0]["kind"], "potential_screening_match");
    assert_eq!(b["signals"][0]["review_required"], true);
}
#[test]
fn unknown_subject_and_partial_or_malformed_screening_cannot_clear() {
    let mut s = screening();
    s.kvk_number = None;
    assert_eq!(
        report(&[s.clone()], None, &BTreeSet::new())["new_signal_count"],
        0
    );
    s.kvk_number = Some("01234567".into());
    s.document["coverage"] = json!("partial");
    assert_eq!(
        report(&[s], None, &BTreeSet::new())["coverage"][2]["status"],
        "unknown"
    );
    let empty = source("opensanctions", json!({"responses":{"q1":{"results":[]}}}));
    assert_eq!(
        report(&[empty], None, &BTreeSet::new())["coverage"][2]["status"],
        "unknown"
    );
}
#[test]
fn zero_matches_are_explicit_and_source_bounded() {
    let s = source(
        "opensanctions",
        json!({"responses":{"q1":{"total":{"value":0},"results":[]}}}),
    );
    let r = report(&[s], None, &BTreeSet::new());
    assert_eq!(r["coverage"][2]["status"], "checked");
    assert_eq!(r["status"], "partial");
}
#[test]
fn raw_evidence_is_saved_but_public_result_is_bounded() {
    let a = report(&[screening()], None, &BTreeSet::new());
    assert!(a["sources"][0]["document"].is_object());
    let b = store::public_snapshot(a);
    assert!(b.get("observations").is_none());
    assert!(b["sources"][0].get("document").is_none());
    assert!(b["new_signals"][0]["evidence"][0]["value"].is_object());
    assert_eq!(b["billing"]["total_charged"], "1234");
}

#[test]
fn partial_results_keep_positive_evidence_without_claiming_complete_coverage() {
    let mut s = screening();
    s.document["coverage"] = json!("partial");
    let r = report(&[s], None, &BTreeSet::new());
    assert_eq!(r["new_signal_count"], 1);
    assert_eq!(r["coverage"][2]["status"], "unknown");
}

#[test]
fn provider_array_order_does_not_reannounce_a_screening_match() {
    let mut s = screening();
    s.document["data"]["responses"]["q1"]["results"][0]["datasets"] = json!(["eu", "ofac"]);
    let a = report(std::slice::from_ref(&s), None, &BTreeSet::new());
    s.document["data"]["responses"]["q1"]["results"][0]["datasets"] = json!(["ofac", "eu"]);
    let b = report(&[s], Some(&a), &BTreeSet::new());
    assert_eq!(a["signals"][0]["signal_id"], b["signals"][0]["signal_id"]);
}

#[test]
fn an_undelivered_registry_change_is_retained_on_the_next_equal_check() {
    let mut s = source(
        "kvk-dutch-business-register",
        json!({"kvkNummer":"01234567","naam":"Acme B.V."}),
    );
    let a = report(std::slice::from_ref(&s), None, &BTreeSet::new());
    s.document["data"]["naam"] = json!("Acme Nieuw B.V.");
    let b = report(&[s.clone()], Some(&a), &BTreeSet::new());
    let c = report(&[s], Some(&b), &BTreeSet::new());
    assert_eq!(c["new_signal_count"], 1);
    assert_eq!(b["signals"][0]["signal_id"], c["signals"][0]["signal_id"]);
}

#[test]
fn source_binding_refuses_names_and_conflicting_registration_facts() {
    let entity = Uuid::new_v4();
    let document = json!({"subject":{"entity_ref":entity,"label":"Acme B.V."}});
    let mut task = crate::state::Task::default();
    assert_eq!(source_company(&task, &document, &[customer()]), None);
    let fact = crate::state::Fact {
        entity_ref: entity,
        semantic_type: "company.registration.nl.kvk".into(),
        value: json!("01234567"),
        provenance: json!({"kind":"user_explicit"}),
        observed_at: chrono::Utc::now(),
        expires_at: chrono::Utc::now(),
        verification: "explicit".into(),
    };
    task.facts.push(fact.clone());
    assert_eq!(
        source_company(&task, &document, &[customer()]),
        Some("01234567".into())
    );
    task.facts.push(crate::state::Fact {
        value: json!("99999999"),
        ..fact
    });
    assert_eq!(source_company(&task, &document, &[customer()]), None);
}

#[test]
fn uncertain_payment_and_running_work_cannot_be_frozen_as_a_completed_check() {
    use crate::protocol::{Response, Status};
    let mut response = Response::new(Uuid::new_v4(), Status::Succeeded);
    response.billing = Some(json!({"status":"pending_reconciliation"}));
    assert!(!can_snapshot(&response));
    response.billing = Some(json!({"status":"reserved"}));
    assert!(!can_snapshot(&response));
    response.billing = Some(json!({"status":"captured"}));
    assert!(can_snapshot(&response));
    response.context_view["worker_active"] = json!(true);
    assert!(!can_snapshot(&response));
    response.context_view["worker_active"] = json!(false);
    response.status = Status::RequiresApproval;
    assert!(!can_snapshot(&response));
}

fn news(url: &str, date: &str, title: &str) -> SavedSource {
    source(
        "exa-search",
        json!({"results":[{"url":url,"publishedDate":date,"title":title}]}),
    )
}
#[test]
fn news_keeps_source_date_and_url_and_deduplicates_tracking_urls() {
    let a = report(
        &[news(
            "https://news.example/article?utm_source=foo#top",
            "2026-10-20T12:00:00Z",
            "Acme bouwt nieuwe fabriek",
        )],
        None,
        &BTreeSet::new(),
    );
    assert_eq!(a["new_signal_count"], 1);
    assert_eq!(a["signals"][0]["url"], "https://news.example/article");
    assert_eq!(
        a["signals"][0]["identity_match"],
        "name_mention_not_verified"
    );
    let reported = BTreeSet::from([a["signals"][0]["signal_id"].as_str().unwrap().to_owned()]);
    let mut s = news(
        "https://news.example/article?fbclid=other",
        "2026-10-20T12:00:00Z",
        "Acme bouwt nieuwe fabriek (bijgewerkt)",
    );
    s.document["source"]["provider"] = json!("tavily");
    s.document["data"]["results"][0]["published_date"] = json!("2026-10-20T12:00:00Z");
    let b = report(&[s], Some(&a), &reported);
    assert_eq!(
        b["new_signal_count"], 0,
        "same article via another provider is not a new signal"
    );
}
#[test]
fn news_rejects_unsafe_undated_old_future_unrelated_and_unbound_results() {
    for (url, date, title) in [
        ("javascript:alert(1)", "2026-10-20T12:00:00Z", "Acme nieuws"),
        ("https://news.example/a", "", "Acme nieuws"),
        (
            "https://news.example/a",
            "2025-10-20T12:00:00Z",
            "Acme nieuws",
        ),
        (
            "https://news.example/a",
            "2027-10-20T12:00:00Z",
            "Acme nieuws",
        ),
        (
            "https://news.example/a",
            "2026-10-20T12:00:00Z",
            "Acmeology nieuws",
        ),
    ] {
        assert_eq!(
            report(&[news(url, date, title)], None, &BTreeSet::new())["new_signal_count"],
            0
        );
    }
    let mut s = news(
        "https://news.example/a",
        "2026-10-20T12:00:00Z",
        "Acme nieuws",
    );
    s.kvk_number = Some("99999999".into());
    assert_eq!(report(&[s], None, &BTreeSet::new())["new_signal_count"], 0);
}
#[test]
fn registry_arrays_are_canonical_and_missing_checks_keep_the_baseline() {
    let mut s = source(
        "kvk-dutch-business-register",
        json!({"kvkNummer":"01234567","naam":"Acme B.V.","handelsnamen":[{"naam":"Acme"},{"naam":"Acme Bikes"}],"totaalWerkzamePersonen":0}),
    );
    let a = report(std::slice::from_ref(&s), None, &BTreeSet::new());
    s.document["data"]["handelsnamen"]
        .as_array_mut()
        .unwrap()
        .reverse();
    let b = report(&[s.clone()], Some(&a), &BTreeSet::new());
    assert_eq!(b["new_signal_count"], 0);
    let gap = report(&[], Some(&b), &BTreeSet::new());
    s.document["data"]["totaalWerkzamePersonen"] = json!(3);
    let changed = report(&[s.clone()], Some(&gap), &BTreeSet::new());
    assert_eq!(changed["new_signal_count"], 1);
    assert_eq!(changed["signals"][0]["previous"], 0);
    let retry = report(&[s], Some(&changed), &BTreeSet::new());
    assert_eq!(
        retry["signals"][0]["signal_id"],
        changed["signals"][0]["signal_id"]
    );
}
#[test]
fn email_contains_only_new_signals_with_evidence_and_coverage_gaps() {
    let first = report(&[screening()], None, &BTreeSet::new());
    let payload = delivery::message(&first);
    assert_eq!(payload["signal_ids"].as_array().unwrap().len(), 1);
    let body = payload["body"].as_str().unwrap();
    assert!(
        body.contains("01234567")
            && body.contains("opensanctions")
            && body.contains("Incomplete coverage")
    );
    assert!(!body.contains("Openstaand"));
    let reported = BTreeSet::from([first["signals"][0]["signal_id"]
        .as_str()
        .unwrap()
        .to_owned()]);
    let second = report(&[screening()], Some(&first), &reported);
    assert!(delivery::message(&second)["signal_ids"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn sanctions_do_not_turn_pep_only_matches_or_inexact_totals_into_sanction_findings() {
    let mut s = screening();
    s.document["data"]["responses"]["q1"]["results"][0]["properties"]["topics"] =
        json!(["role.pep"]);
    assert_eq!(
        report(&[s.clone()], None, &BTreeSet::new())["new_signal_count"],
        0
    );
    s.document["data"]["responses"]["q1"]["total"]["relation"] = json!("gte");
    assert_eq!(
        report(&[s], None, &BTreeSet::new())["coverage"][2]["status"],
        "unknown"
    );
}
#[test]
fn shielded_address_data_is_not_promoted_to_a_signal_or_email() {
    let mut s = source(
        "kvk-dutch-business-register",
        json!({"kvkNummer":"01234567","naam":"Acme B.V.","_embedded":{"hoofdvestiging":{"adressen":[{"indAfgeschermd":"Nee","volledigAdres":"Public 1"}]}}}),
    );
    let a = report(std::slice::from_ref(&s), None, &BTreeSet::new());
    s.document["data"]["_embedded"]["hoofdvestiging"]["adressen"][0] =
        json!({"indAfgeschermd":"Ja","volledigAdres":"Private 2"});
    let b = report(&[s], Some(&a), &BTreeSet::new());
    assert_eq!(b["new_signal_count"], 0);
    assert!(!delivery::message(&b)["body"]
        .as_str()
        .unwrap()
        .contains("Private 2"));
}

#[test]
fn undelivered_news_keeps_original_evidence_after_the_search_window() {
    let a = report(
        &[news(
            "https://news.example/article",
            "2026-10-20T12:00:00Z",
            "Acme bouwt fabriek",
        )],
        None,
        &BTreeSet::new(),
    );
    let b = report(&[], Some(&a), &BTreeSet::new());
    assert_eq!(b["new_signal_count"], 1);
    assert_eq!(b["coverage"][1]["status"], "unknown");
    assert_eq!(b["new_signals"][0]["carried_forward"], true);
    assert_eq!(b["sources"][0]["historical"], true);
    assert_eq!(
        b["sources"][0]["result_ref"],
        b["new_signals"][0]["evidence"][0]["result_ref"]
    );
    assert_eq!(
        b["source_count"], 0,
        "old evidence never counts as a fresh source call"
    );
    assert_eq!(b["sources"][0]["document"], a["sources"][0]["document"]);
}
#[test]
fn newly_added_registration_end_compares_against_complete_previous_registration() {
    let mut s = source(
        "kvk-dutch-business-register",
        json!({"kvkNummer":"01234567","naam":"Acme B.V.","materieleRegistratie":{"datumAanvang":"20200101"}}),
    );
    let a = report(std::slice::from_ref(&s), None, &BTreeSet::new());
    assert_eq!(a["new_signal_count"], 0);
    s.document["data"]["materieleRegistratie"]["datumEinde"] = json!("20261020");
    let b = report(&[s], Some(&a), &BTreeSet::new());
    assert_eq!(b["new_signal_count"], 1);
    assert_eq!(b["signals"][0]["identity"], "registration_end");
}
