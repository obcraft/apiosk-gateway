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
    let a = report(&[s.clone()], None, &BTreeSet::new());
    let b = report(&[], Some(&a), &BTreeSet::new());
    assert_eq!(b["new_signal_count"], 1);
    assert_eq!(b["new_signals"][0]["signal_id"], a["new_signals"][0]["signal_id"]);
    assert_eq!(b["new_signals"][0]["carried_forward"], true);
    assert_eq!(b["new_signals"][0]["evidence"], a["new_signals"][0]["evidence"]);
    assert_eq!(b["sources"][0]["historical"], true);
    assert_eq!(b["source_count"], 0);
    assert_eq!(b["coverage"][2]["status"], "unknown");
    let mail = delivery::message(&b);
    assert!(mail["body"].as_str().unwrap().contains("Eerder waargenomen"));
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
    assert!(!question(&[customer()]).contains("insolventie"));
    assert!(question(&[customer()]).contains("nieuwsberichten"));
    assert!(question(&[customer()]).contains("01234567"));
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
    let a = report(&[s.clone()], None, &BTreeSet::new());
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
    let a = report(&[s.clone()], None, &BTreeSet::new());
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
    let a = report(&[s.clone()], None, &BTreeSet::new());
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
    let a = report(&[s.clone()], None, &BTreeSet::new());
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
    let a = report(&[s.clone()], None, &BTreeSet::new());
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
    let a = report(&[s.clone()], None, &BTreeSet::new());
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
            && body.contains("Onvolledige dekking")
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
    let a = report(&[s.clone()], None, &BTreeSet::new());
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
    let a = report(&[s.clone()], None, &BTreeSet::new());
    assert_eq!(a["new_signal_count"], 0);
    s.document["data"]["materieleRegistratie"]["datumEinde"] = json!("20261020");
    let b = report(&[s], Some(&a), &BTreeSet::new());
    assert_eq!(b["new_signal_count"], 1);
    assert_eq!(b["signals"][0]["identity"], "registration_end");
}
