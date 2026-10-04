use super::*;

const QUESTION: &str = "Compare bankruptcies per sector in the Netherlands.";

fn saved_sources() -> Vec<Value> {
    let fixture: Value = serde_json::from_str(include_str!("../tests/fixtures/cbs-bankruptcy-comparison.json")).unwrap();
    fixture["sources"].as_array().unwrap().iter().enumerate().map(|(index, response)| json!({
        "result_ref": uuid::Uuid::from_u128(index as u128 + 1).to_string(),
        "document": { "data": response["data"], "coverage":"source_response", "currency":"USD",
            "subject":{"label":"Netherlands", "entity_ref":uuid::Uuid::nil().to_string()},
            "source":{"provider":"cbs-statline", "name":"CBS Statistics", "path":format!("/odata/v1/CBS/82244NED/{}",response["resource"].as_str().unwrap()), "url":response["url"]}
        }
    })).collect()
}

#[tokio::test]
async fn public_starter_resolves_all_five_priced_native_calls_without_a_model() {
    let snapshot: crate::catalog::Snapshot = serde_json::from_str(include_str!("../tests/fixtures/cbs-bankruptcy-comparison-catalog.json")).unwrap();
    let catalog = crate::catalog::Catalog::build(snapshot);
    assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
    assert_eq!(catalog.services.len(), 5);
    let parser = crate::parser::Parser::new(reqwest::Client::new(), "http://127.0.0.1:1".into(), "".into(), "fixture".into());
    for question in [QUESTION, "Vergelijk faillissementen per sector in Nederland."] {
        let mut task = crate::state::Task::default();
        let intake = parser.ask_intake(question, &task, &catalog, None).await.unwrap();
        assert_eq!(intake.parsed_request.capabilities.len(), 5);
        assert!(intake.parsed_request.capabilities[0].missing.is_empty());
        let reply = parser.chat_from_capabilities(question, None, &task, &catalog, &intake).await.unwrap();
        assert!(reply.discover, "{}; matches={:?}", reply.message, intake.matches);
        let mut intent = reply.intent.unwrap();
        assert_eq!(intent.goals.len(), 5);
        assert_eq!(intent.subjects.len(), 1);
        assert_eq!(intent.subjects[0].label, intake.parsed_request.subjects[0].label);
        assert_eq!(parser.calls.load(std::sync::atomic::Ordering::Relaxed), 0);
        task.capability_intake = Some(intake);
        crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(&catalog, &task, &intent) else { panic!("complete comparison must resolve") };
        assert_eq!(plan.steps.len(), 5);
        assert_eq!(plan.cap_atomic, plan.steps.iter().map(|step| step.service.total()).sum::<i64>());
        assert_eq!(plan.cap_atomic, 134348, "quoted USD atomic total includes all five existing source prices and the normal buyer fee");
        let observations = plan.steps.iter().find(|step| step.service.contract.capability_slug == CAPABILITY).unwrap();
        assert_eq!(observations.service.inputs(&task, observations.entity_ref, &catalog.vocabulary).unwrap(), json!({"$filter":"TypeGefailleerde eq 'A047596' and contains(Perioden,'MM')", "$orderby":"Perioden desc", "$top":1000, "$count":true}));
        assert!(plan.steps.iter().all(|step| step.service.api_slug == "cbs-statline" && step.service.total() > 0 && Some(step.entity_ref) == intent.subjects[0].entity_ref));
        let calls = plan.steps.iter().map(|step| step.service.contract.capability_slug.as_str()).collect::<BTreeSet<_>>();
        assert_eq!(calls, BTreeSet::from([CAPABILITY, "statistics.cbs-bankruptcy-sectors", "statistics.cbs-bankruptcy-periods", "statistics.cbs-bankruptcy-measures", "statistics.cbs-bankruptcy-entity-types"]));
    }
}

#[tokio::test]
async fn changed_presets_or_missing_codebooks_cannot_create_an_incomplete_quote() {
    for change in ["missing_periods", "wrong_population", "missing_unit_schema"] {
        let mut snapshot: crate::catalog::Snapshot = serde_json::from_str(include_str!("../tests/fixtures/cbs-bankruptcy-comparison-catalog.json")).unwrap();
        match change {
            "missing_periods" => snapshot.services.retain(|service| service.contract.capability_slug != "statistics.cbs-bankruptcy-periods"),
            "wrong_population" => snapshot.services.iter_mut().find(|service| service.contract.capability_slug == CAPABILITY).unwrap().fixed_params["$filter"] = json!("TypeGefailleerde eq 'A028820' and contains(Perioden,'MM')"),
            "missing_unit_schema" => snapshot.vocabulary.iter_mut().find(|fact| fact.slug == "cbs.measure_codes").unwrap().value_schema["properties"]["value"]["items"] = json!({"type":"object"}),
            _ => unreachable!(),
        }
        let catalog = crate::catalog::Catalog::build(snapshot);
        let parser = crate::parser::Parser::new(reqwest::Client::new(), "http://127.0.0.1:1".into(), "".into(), "fixture".into());
        let task = crate::state::Task::default();
        let intake = parser.ask_intake(QUESTION, &task, &catalog, None).await.unwrap();
        let reply = parser.chat_from_capabilities(QUESTION, None, &task, &catalog, &intake).await.unwrap();
        assert!(!reply.discover && reply.intent.is_none(), "{change}: {}", reply.message);
        assert_eq!(parser.calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    }
}

#[test]
fn canonical_intake_consumes_whole_request_without_inventing_missing_inputs() {
    for question in [QUESTION, " Compare   bankruptcies BY sector in the Netherlands? ", "Vergelijk faillissementen per sector in Nederland."] {
        let parsed = canonical_request(question).unwrap();
        assert_eq!(parsed.subjects.len(), 1);
        assert_eq!(parsed.subjects[0].kind, "country");
        assert_eq!(parsed.capabilities.iter().flat_map(|cap| &cap.outputs).map(String::as_str).collect::<Vec<_>>(), ["bankruptcy_count", "sector", "period", "period_status", "unit", "population"]);
        assert!(parsed.capabilities.iter().all(|cap| cap.inputs.is_empty() && cap.missing.is_empty()));
        assert_eq!(parsed.deliverable.operations, ["compare"]);
    }
    for question in [
        "Compare bankruptcies per sector in Belgium.",
        "Compare bankruptcies per sector in the Netherlands in 2024.",
        "Compare bankruptcies per sector in the Netherlands and Germany.",
        "Compare bankruptcies per sector in the Netherlands as JSON.",
        "Compare bankruptcies per sector in the Netherlands. Include turnover.",
        "Compare bankruptcies per sector in the Netherlands, excluding sole proprietors.",
        "Compare bankruptcy rates per sector in the Netherlands.",
    ] {
        assert!(canonical_request(question).is_none(), "{question}");
        assert!(compose(question, &[]).unwrap().is_none(), "{question}");
    }
}

#[test]
fn real_newest_first_page_yields_disjoint_source_cited_latest_month_comparison() {
    let sources = saved_sources();
    let original = sources.clone();
    assert_eq!(sources[0]["document"]["data"]["value"].as_array().unwrap().len(), 1000);
    assert_eq!(sources[1]["document"]["data"]["value"].as_array().unwrap().len(), 372);
    let answer = compose(QUESTION, &sources).unwrap().unwrap();
    assert_eq!(sources, original, "saved provider evidence must remain unchanged");
    assert_eq!(answer["status"], "completed");
    assert!(answer["observations"][0]["text"].as_str().unwrap().contains("2026 augustus"));
    assert!(answer["observations"][0]["text"].as_str().unwrap().contains("Voorlopig"));
    assert!(answer["observations"][1]["text"].as_str().unwrap().contains("Bedrijven, instellingen en eenmanszaken"));
    assert_eq!(answer["observations"][2]["evidence"][1]["value"], 288.0);
    let rows = answer["presentation"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 21);
    assert_eq!(rows[0][0]["value"], "F Bouwnijverheid");
    assert_eq!(rows[0][1]["value"], 50.0);
    assert_eq!(rows[20][0]["value"], "SBI-code onbekend");
    assert_eq!(rows[20][1]["value"], 1.0);
    assert_eq!(rows.iter().map(|row| row[1]["value"].as_f64().unwrap()).sum::<f64>(), 288.0);
    assert!(rows.iter().any(|row| row[1]["value"] == 0.0), "zero is a valid count");
    for row in rows {
        assert_ne!(row[0]["value"], "A-U Alle economische activiteiten", "national total must stay outside the sector ranking");
        assert_eq!(row[2]["value"], "2026 augustus");
        assert_eq!(row[3]["value"], "Voorlopig");
        for cell in row.as_array().unwrap() {
            let source = sources.iter().find(|source| source["result_ref"] == cell["result_ref"]).unwrap();
            assert_eq!(source["document"].pointer(cell["pointer"].as_str().unwrap()).unwrap(), &cell["value"]);
        }
    }
}

#[test]
fn rejects_the_old_truncated_page_missing_sectors_duplicates_and_stale_months() {
    let mut truncated = saved_sources();
    truncated[0]["document"]["data"]["value"].as_array_mut().unwrap().truncate(100);
    assert!(compose(QUESTION, &truncated).unwrap_err().to_string().contains("incomplete"));

    let mut duplicate = saved_sources();
    let first = duplicate[0]["document"]["data"]["value"][0].clone();
    duplicate[0]["document"]["data"]["value"][1] = first;
    assert!(compose(QUESTION, &duplicate).unwrap_err().to_string().contains("duplicate"));

    let mut stale = saved_sources();
    stale[0]["document"]["data"]["value"].as_array_mut().unwrap().retain(|row| row["Perioden"] != "2026MM08");
    assert!(compose(QUESTION, &stale).unwrap_err().to_string().contains("incomplete"));

    let mut new_month = saved_sources();
    new_month[2]["document"]["data"]["value"].as_array_mut().unwrap().push(json!({"Identifier":"2026MM09", "Title":"2026 september", "Status":"Voorlopig"}));
    assert!(compose(QUESTION, &new_month).unwrap_err().to_string().contains("incomplete"));
}

#[test]
fn rejects_mixed_population_periods_measure_tables_and_incomplete_codebooks() {
    for (field, value) in [("TypeGefailleerde", "A028820"), ("Measure", "M999999"), ("Perioden", "2026JJ00")] {
        let mut sources = saved_sources();
        sources[0]["document"]["data"]["value"][0][field] = json!(value);
        assert!(compose(QUESTION, &sources).is_err(), "{field}");
    }
    let mut table = saved_sources();
    table[2]["document"]["data"]["@odata.context"] = json!("https://datasets.cbs.nl/odata/v1/CBS/85809NED/$metadata#PeriodenCodes");
    assert!(compose(QUESTION, &table).is_err());
    let mut provider = saved_sources();
    provider[0]["document"]["source"]["provider"] = json!("other");
    assert!(compose(QUESTION, &provider).is_err());
    let mut paginated = saved_sources();
    paginated[1]["document"]["data"]["@odata.nextLink"] = json!("https://datasets.cbs.nl/next");
    assert!(compose(QUESTION, &paginated).is_err());
    let mut count = saved_sources();
    count[1]["document"]["data"]["@odata.count"] = json!(373);
    assert!(compose(QUESTION, &count).is_err());
    let mut duplicate = saved_sources();
    duplicate.push(duplicate[0].clone());
    assert!(compose(QUESTION, &duplicate).is_err());
}

#[test]
fn missing_counts_remain_unknown_and_invalid_numbers_never_become_analysis_facts() {
    let mut missing = saved_sources();
    missing[0]["document"]["data"]["value"][1]["Value"] = Value::Null;
    missing[0]["document"]["data"]["value"][1]["ValueAttribute"] = json!("Missing");
    let answer = compose(QUESTION, &missing).unwrap().unwrap();
    assert_eq!(answer["status"], "partial");
    let rows = answer["presentation"]["rows"].as_array().unwrap();
    assert!(rows.iter().any(|row| row[0]["value"] == "A Landbouw, bosbouw en visserij" && row[1].is_null()));
    for value in [json!(-99997), json!(2.5), json!("3")] {
        let mut invalid = saved_sources();
        invalid[0]["document"]["data"]["value"][1]["Value"] = value;
        assert!(compose(QUESTION, &invalid).is_err());
    }
    let mut attribute = saved_sources();
    attribute[0]["document"]["data"]["value"][1]["ValueAttribute"] = json!("Suppressed");
    assert!(compose(QUESTION, &attribute).is_err());
    let mut inconsistent = saved_sources();
    inconsistent[0]["document"]["data"]["value"][0]["Value"] = json!(999);
    assert!(compose(QUESTION, &inconsistent).unwrap_err().to_string().contains("reconcile"));
}

#[test]
fn source_grouping_changes_require_review_instead_of_double_counting() {
    let mut changed = saved_sources();
    changed[1]["document"]["data"]["value"][1]["DimensionGroupId"] = json!("5");
    assert!(compose(QUESTION, &changed).unwrap_err().to_string().contains("incomplete"));
    let mut no_status = saved_sources();
    let rows = no_status[2]["document"]["data"]["value"].as_array_mut().unwrap();
    let latest = rows.iter_mut().find(|row| row["Identifier"] == "2026MM08").unwrap();
    latest.as_object_mut().unwrap().remove("Status");
    assert!(compose(QUESTION, &no_status).is_err());
}

fn projection_requirements() -> Vec<Requirement> {
    [CAPABILITY, "statistics.cbs-bankruptcy-sectors", "statistics.cbs-bankruptcy-periods",
        "statistics.cbs-bankruptcy-measures", "statistics.cbs-bankruptcy-entity-types"]
        .into_iter().enumerate().map(|(index, criterion)| serde_json::from_value(json!({
            "id":format!("r{}", index + 1), "subject":0, "subject_label":"Netherlands",
            "entity_ref":uuid::Uuid::nil().to_string(), "criterion":criterion,
            "criterion_kind":"capability", "status":"pending"
        })).unwrap()).collect()
}

#[test]
fn cbs_evidence_projection_preserves_complete_disjoint_slice_and_original_pointers() {
    let mut sources = saved_sources();
    sources.push(json!({"result_ref":uuid::Uuid::from_u128(100).to_string(),
        "document":{"source":{"provider":"another-source"},"data":{"year":2024,"value":123}}}));
    let original = sources.clone();
    let projected = evidence_projection(&sources, &projection_requirements()).unwrap();
    assert_eq!(sources, original, "saved evidence is never modified");
    assert_eq!(projected[5], sources[5], "other source scope is unchanged");
    for (index, expected) in [(0,22), (1,22), (2,1), (3,1), (4,1)] {
        let rows = projected[index]["document"]["data"]["value"].as_object().unwrap();
        assert_eq!(rows.len(), expected);
        for (key, row) in rows {
            let pointer = format!("/data/value/{key}");
            assert_eq!(sources[index]["document"].pointer(&pointer), Some(row));
            for (field, value) in row.as_object().unwrap() {
                assert_eq!(sources[index]["document"].pointer(&format!("{pointer}/{field}")), Some(value));
            }
        }
        assert_eq!(projected[index]["document"]["source"], sources[index]["document"]["source"]);
        assert_eq!(projected[index]["document"]["coverage"], sources[index]["document"]["coverage"]);
        assert_eq!(projected[index]["document"]["evidence_selection"]["comparison_values_complete"], true);
    }
    let observations = projected[0]["document"]["data"]["value"].as_object().unwrap();
    assert!(observations.values().all(|row| row["Perioden"] == "2026MM08"
        && row["Measure"] == MEASURE && row["TypeGefailleerde"] == POPULATION));
    let sectors = projected[1]["document"]["data"]["value"].as_object().unwrap();
    assert_eq!(sectors.values().filter(|row| row["DimensionGroupId"] == "3").count(), 20);
    assert_eq!(sectors.values().filter(|row| row["Identifier"] == TOTAL).count(), 1);
    assert_eq!(sectors.values().filter(|row| row["Identifier"] == UNKNOWN).count(), 1);
    let national = observations.values().find(|row| row["BedrijfstakkenBranchesSBI2008"] == TOTAL).unwrap();
    assert_eq!(national["Value"], 288.0);
    assert_eq!(observations.values().filter(|row| row["BedrijfstakkenBranchesSBI2008"] != TOTAL)
        .map(|row| row["Value"].as_f64().unwrap()).sum::<f64>(), 288.0);
    let period = projected[2]["document"]["data"]["value"].as_object().unwrap().values().next().unwrap();
    assert_eq!(period["Title"], "2026 augustus");
    assert_eq!(period["Status"], "Voorlopig");
    let measure = projected[3]["document"]["data"]["value"].as_object().unwrap().values().next().unwrap();
    assert_eq!(measure["Unit"], "Aantal");
    let population = projected[4]["document"]["data"]["value"].as_object().unwrap().values().next().unwrap();
    assert_eq!(population["Title"], "Bedrijven, instellingen en eenmanszaken");
}

#[test]
fn cbs_evidence_projection_requires_exact_default_period_scope_and_bound_entity() {
    let sources = saved_sources();
    for change in ["no_comparison", "missing_codebook", "explicit_period", "other_cbs_goal", "other_entity", "unbound_entity", "duplicate_comparison"] {
        let mut requirements = projection_requirements();
        match change {
            "no_comparison" => requirements[0].criterion = "unrelated-capability".into(),
            "missing_codebook" => { requirements.pop(); },
            "explicit_period" => requirements[0].period = Some("2024".into()),
            "other_cbs_goal" => { let mut extra = requirements[0].clone(); extra.criterion = "statistics.cbs-bankruptcy-rate".into(); requirements.push(extra); },
            "other_entity" => requirements[1].entity_ref = Some(uuid::Uuid::from_u128(99).to_string()),
            "unbound_entity" => requirements[0].entity_ref = None,
            "duplicate_comparison" => requirements.push(requirements[0].clone()),
            _ => unreachable!(),
        }
        assert!(evidence_projection(&sources, &requirements).is_none(), "{change}");
    }
    let mut requirements = projection_requirements();
    let mut gdp = requirements[0].clone();
    gdp.criterion = "statistics.eurostat-gdp-netherlands".into();
    gdp.period = Some("2024".into());
    gdp.entity_ref = Some(uuid::Uuid::from_u128(99).to_string());
    requirements.push(gdp);
    assert!(evidence_projection(&sources, &requirements).is_some(), "another source's annual period does not change CBS monthly scope");
}

#[test]
fn cbs_evidence_projection_reuses_join_guards_and_keeps_missing_counts_unknown() {
    for change in ["truncated", "unit", "entity", "missing_fields", "duplicate_ref", "group", "period_status"] {
        let mut sources = saved_sources();
        match change {
            "truncated" => sources[0]["document"]["data"]["value"].as_array_mut().unwrap().truncate(100),
            "unit" => sources[3]["document"]["data"]["value"][0]["Unit"] = json!("Percent"),
            "entity" => sources[2]["document"]["subject"]["entity_ref"] = json!(uuid::Uuid::from_u128(99).to_string()),
            "missing_fields" => sources[0]["document"]["missing_fields"] = json!(["cbs.observations"]),
            "duplicate_ref" => { let mut extra = sources[0].clone(); extra["document"]["source"]["provider"] = json!("another-source"); sources.push(extra); },
            "group" => sources[1]["document"]["data"]["value"][1]["DimensionGroupId"] = json!("5"),
            "period_status" => { let period = sources[2]["document"]["data"]["value"].as_array_mut().unwrap().iter_mut().find(|row| row["Identifier"] == "2026MM08").unwrap(); period.as_object_mut().unwrap().remove("Status"); },
            _ => unreachable!(),
        }
        assert!(evidence_projection(&sources, &projection_requirements()).is_none(), "{change}");
    }
    let mut sources = saved_sources();
    sources[0]["document"]["data"]["value"][1]["Value"] = Value::Null;
    sources[0]["document"]["data"]["value"][1]["ValueAttribute"] = json!("Missing");
    let projected = evidence_projection(&sources, &projection_requirements()).unwrap();
    assert_eq!(projected[0]["document"]["data"]["value"]["1"]["Value"], Value::Null);
    assert_eq!(projected[0]["document"]["data"]["value"]["1"]["ValueAttribute"], "Missing");
    assert_eq!(projected[0]["document"]["evidence_selection"]["comparison_values_complete"], false);
}

#[test]
fn cbs_evidence_projection_uses_saved_new_period_and_status_without_question_matching() {
    let mut sources = saved_sources();
    for row in sources[0]["document"]["data"]["value"].as_array_mut().unwrap() {
        if row["Perioden"] == "2026MM08" { row["Perioden"] = json!("2027MM02"); }
    }
    for row in sources[2]["document"]["data"]["value"].as_array_mut().unwrap() {
        if row["Identifier"] == "2026MM08" {
            row["Identifier"] = json!("2027MM02");
            row["Title"] = json!("2027 februari");
            row["Status"] = json!("Definitief");
        }
    }
    let projected = evidence_projection(&sources, &projection_requirements()).unwrap();
    assert!(projected[0]["document"]["data"]["value"].as_object().unwrap().values().all(|row| row["Perioden"] == "2027MM02"));
    let period = projected[2]["document"]["data"]["value"].as_object().unwrap().values().next().unwrap();
    assert_eq!(period["Title"], "2027 februari");
    assert_eq!(period["Status"], "Definitief");
}
