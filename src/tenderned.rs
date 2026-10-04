//! Preserve a supplied procurement topic and country, then serialize the
//! topic with TenderNed's phrase-search syntax and reviewed Dutch keywords. No CPV code, source
//! result, deadline or open/awarded status is inferred from the question.
use crate::{
    capability_intake::{Capability, Deliverable, Intake, Meta, ParsedRequest, Subject},
    catalog::Service,
};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const BROWSE: &str = "procurement.nl-public-contracts";
const SEARCH: &str = "company_intel.input.search";

/// The complete plain-question form, with arbitrary literal topics and named
/// countries. Additional formats, sources, dates or result requirements stay
/// on the normal extraction path instead of disappearing into this shortcut.
pub(crate) fn parse(question: &str, model: &str) -> Option<ParsedRequest> {
    let text = crate::capability_intake::literal_sentence(question).trim_end_matches(['.', '?', '!']).trim();
    let lower = text.to_lowercase();
    let (language, prefix) = [
        ("en", "find public tenders for "),
        ("en", "show public tenders for "),
        ("nl", "zoek openbare aanbestedingen voor "),
        ("nl", "toon openbare aanbestedingen voor "),
    ]
    .into_iter()
    .find(|(_, prefix)| lower.starts_with(prefix))?;
    let body = text.get(prefix.len()..)?;
    let at = body.to_lowercase().rfind(" in ")?;
    let topic = body.get(..at)?.trim();
    let country = body.get(at + 4..)?.trim();
    if !plain_topic(topic) {
        return None;
    }
    let country_name = if country
        .get(..4)
        .is_some_and(|part| part.eq_ignore_ascii_case("the "))
    {
        &country[4..]
    } else {
        country
    };
    if !country_name
        .chars()
        .all(|ch| ch.is_alphabetic() || ch.is_whitespace() || ch == '-')
    {
        return None;
    }
    let compact: String = country_name
        .chars()
        .filter(|ch| ch.is_alphabetic())
        .collect();
    if crate::ask_v2::search::named_countries(&compact).len() != 1 {
        return None;
    }
    Some(ParsedRequest {
        language: language.into(),
        subjects: vec![Subject { id: "s1".into(), label: topic.into(), kind: "topic".into(),
            identifiers: BTreeMap::from([("country".into(), country.into())]) }],
        capabilities: vec![Capability {
            slug: "procurement.public.notices".into(), name: "Public procurement notices".into(),
            description: "Find public procurement notices matching the supplied topic in the supplied country. Preserve publication types, dates, source links and bounded search coverage.".into(),
            domain: "procurement".into(), inputs: vec!["topic".into(), "country".into()],
            outputs: vec!["publication_page".into()], subject_id: Some("s1".into()), source_hint: None,
            confidence: 1.0, missing: vec![],
        }],
        deliverable: Deliverable { format: "chat".into(), operations: vec![] },
        meta: Meta { model: model.into(), parsed_at: chrono::Utc::now().to_rfc3339(),
            text_sha256: hex::encode(Sha256::digest(question.as_bytes())) },
    })
}

fn plain_topic(topic: &str) -> bool {
    let words: Vec<_> = topic.split_whitespace().collect();
    !words.is_empty()
        && words.len() <= 8
        && topic.len() <= 160
        && topic
            .chars()
            .all(|ch| ch.is_alphanumeric() || ch.is_whitespace() || ch == '-')
        && !words
            .iter()
            .any(|word| word.chars().all(|ch| ch.is_ascii_digit() || ch == '-'))
        && !words.iter().any(|word| {
            matches!(
                word.to_lowercase().as_str(),
                "and"
                    | "or"
                    | "not"
                    | "with"
                    | "without"
                    | "including"
                    | "excluding"
                    | "except"
                    | "before"
                    | "after"
                    | "between"
                    | "from"
                    | "since"
                    | "until"
                    | "via"
                    | "as"
                    | "under"
                    | "over"
                    | "closing"
                    | "closed"
                    | "deadline"
                    | "deadlines"
                    | "open"
                    | "active"
                    | "awarded"
                    | "expired"
                    | "latest"
                    | "recent"
                    | "current"
                    | "currently"
                    | "new"
                    | "newest"
                    | "only"
                    | "top"
                    | "first"
                    | "today"
                    | "tomorrow"
                    | "yesterday"
                    | "week"
                    | "month"
                    | "year"
                    | "weekly"
                    | "monthly"
                    | "yearly"
                    | "january"
                    | "february"
                    | "march"
                    | "april"
                    | "may"
                    | "june"
                    | "july"
                    | "august"
                    | "september"
                    | "october"
                    | "november"
                    | "december"
                    | "en"
                    | "of"
                    | "met"
                    | "zonder"
                    | "inclusief"
                    | "exclusief"
                    | "behalve"
                    | "vanaf"
                    | "tot"
                    | "als"
                    | "sluitend"
                    | "sluitingsdatum"
                    | "openstaande"
                    | "actieve"
                    | "actuele"
                    | "gegunde"
                    | "gesloten"
                    | "verlopen"
                    | "laatste"
                    | "nieuwste"
                    | "recente"
                    | "alleen"
                    | "eerste"
                    | "vandaag"
                    | "morgen"
                    | "gisteren"
                    | "maand"
                    | "jaar"
            )
        })
}

/// Restrict the model-free recovery to the exact parsed request and an
/// already-discovered Dutch national-notices contract. Other countries retain
/// their country requirement and use the regular planner/source contracts.
pub(crate) fn executable_intake(question: &str, intake: &Intake) -> Option<Intake> {
    let expected = parse(question, "literal")?;
    let actual = &intake.parsed_request;
    if serde_json::to_value(&expected.subjects).ok()?
        != serde_json::to_value(&actual.subjects).ok()?
        || serde_json::to_value(&expected.capabilities).ok()?
            != serde_json::to_value(&actual.capabilities).ok()?
        || serde_json::to_value(&expected.deliverable).ok()?
            != serde_json::to_value(&actual.deliverable).ok()?
        || expected.language != actual.language
    {
        return None;
    }
    let country = &actual.subjects[0].identifiers["country"];
    if crate::ask_v2::search::named_countries(country) != BTreeSet::from(["NL"]) {
        return None;
    }
    let mut restricted = intake.clone();
    for found in &mut restricted.matches {
        found.apiosk.retain(|candidate| {
            candidate.source_slug.as_deref() == Some("tenderned")
                && candidate.capability.as_deref() == Some(BROWSE)
                && candidate.availability == "supported"
                && candidate.endpoint_id.is_some()
        });
        found
            .operations
            .retain(|operation| operation == BROWSE && !found.apiosk.is_empty());
    }
    restricted.selected_capabilities = restricted
        .matches
        .iter()
        .flat_map(|found| found.operations.iter().cloned())
        .collect();
    (!restricted.selected_capabilities.is_empty()).then_some(restricted)
}

/// Two reviewed English procurement topics use the same Dutch source keywords
/// as the verified examples. The user's subject and question stay unchanged.
/// Explicit provider syntax and every other topic retain their literal form.
fn source_search(topic: &str, translate_browse_topic: bool) -> String {
    let native = if translate_browse_topic && topic.eq_ignore_ascii_case("solar panels") {
        "zonnepanelen"
    } else if translate_browse_topic && topic.eq_ignore_ascii_case("cleaning services") {
        "schoonmaak"
    } else {
        topic
    };
    if plain_topic(native) && native.split_whitespace().count() > 1 {
        format!("\"{native}\"")
    } else {
        native.into()
    }
}

/// TenderNed combines unquoted words broadly. A multiword topic is a phrase,
/// not implicit OR. Retain explicit provider query syntax unchanged.
/// Both the quote and execution use Service::inputs, so they agree exactly.
pub(crate) fn refine(service: &Service, inputs: &mut Map<String, Value>) {
    if service.api_slug != "tenderned"
        || service.path != "/v2/publicaties"
        || service.method != "GET"
        || !matches!(
            service.contract.capability_slug.as_str(),
            BROWSE | "company_intel.search"
        )
        || service
            .contract
            .input_map
            .get(SEARCH)
            .is_none_or(|binding| binding.name != "search")
    {
        return;
    }
    let Some(search) = inputs.get("search").and_then(Value::as_str) else {
        return;
    };
    let query = source_search(search, service.contract.capability_slug == BROWSE);
    if query != search {
        inputs.insert("search".into(), json!(query));
    }
}

/// Save the same server-bound parameters used for this provider call. Older
/// documents without this provenance cannot be presented as filtered results.
pub(crate) fn record_request(service: &Service, inputs: &Value, document: &mut Value) {
    if service.api_slug == "tenderned"
        && service.path == "/v2/publicaties"
        && service.method == "GET"
        && matches!(
            service.contract.capability_slug.as_str(),
            BROWSE | "company_intel.search"
        )
        && inputs.is_object()
    {
        document["source"]["request_parameters"] = inputs.clone();
    }
}

/// Exact saved publication rows for the plain tender-search form. This does
/// not evaluate eligibility, status, award values or unreturned later pages.
pub(crate) fn compose(
    question: &str,
    intent: &crate::parser::Intent,
    sources: &[Value],
) -> Option<Value> {
    let parsed = parse(question, "literal")?;
    if crate::ask_v2::search::named_countries(&parsed.subjects[0].identifiers["country"])
        != BTreeSet::from(["NL"])
        || intent.subjects.len() != 1
        || intent.goals.len() != 1
        || sources.len() != 1
        || !intent.unresolved_references.is_empty()
    {
        return None;
    }
    let subject = &intent.subjects[0];
    let goal = &intent.goals[0];
    if subject.label != parsed.subjects[0].label
        || subject.values.len() != 1
        || subject.values.get(SEARCH)?.as_str()? != subject.label
        || goal.subject != 0
        || goal.capability != BROWSE
        || goal.constraints.get("source")?.as_str()? != "tenderned"
        || goal
            .constraints
            .keys()
            .any(|key| !matches!(key.as_str(), "source" | "endpoint_id"))
    {
        return None;
    }
    let source = &sources[0];
    let document = &source["document"];
    if !source["result_ref"].is_string()
        || document["coverage"] != "source_response"
        || document["subject"]["entity_ref"] != json!(subject.entity_ref?)
        || document["source"]["provider"] != "tenderned"
        || document["source"]["path"] != "/v2/publicaties"
        || document["source"]["endpoint_id"] != *goal.constraints.get("endpoint_id")?
        || document["missing_fields"]
            .as_array()
            .is_none_or(|fields| !fields.is_empty())
    {
        return None;
    }
    let params = document["source"]["request_parameters"].as_object()?;
    let expected_search = source_search(&subject.label, true);
    if params.get("search")?.as_str()? != expected_search
        || params.get("page")? != &json!(0)
        || params
            .get("size")?
            .as_u64()
            .is_none_or(|size| size == 0 || size > 100)
        || params
            .keys()
            .any(|key| !matches!(key.as_str(), "search" | "page" | "size" | "sort"))
    {
        return None;
    }
    let page = &document["data"];
    let rows = page["content"].as_array()?;
    let total = page["totalElements"].as_u64()?;
    let size = params.get("size")?.as_u64()?;
    // A completed bounded page must contain the whole page that was quoted.
    // Agreement between content and numberOfElements alone also accepts a
    // truncated or empty first page while the source reports existing matches.
    if rows.len() as u64 != total.min(size)
        || page["size"].as_u64()? != size
        || page["number"] != 0
        || page["numberOfElements"].as_u64()? != rows.len() as u64
    {
        return None;
    }
    let mut publication_ids = BTreeSet::new();
    for row in rows {
        for pointer in [
            "/publicatieId",
            "/aanbestedingNaam",
            "/opdrachtgeverNaam",
            "/publicatieDatum",
            "/typePublicatie/omschrijving",
        ] {
            let value = row.pointer(pointer)?.as_str()?;
            if value.trim().is_empty() {
                return None;
            }
        }
        if !publication_ids.insert(row["publicatieId"].as_str()?) {
            return None;
        }
        let link = url::Url::parse(row["link"]["href"].as_str()?).ok()?;
        if link.scheme() != "https"
            || link.host_str() != Some("www.tenderned.nl")
            || !link.username().is_empty()
            || link.password().is_some()
            || link.path()
                != format!(
                    "/aankondigingen/overzicht/{}",
                    row["publicatieId"].as_str()?
                )
        {
            return None;
        }
    }
    let dutch = parsed.language == "nl";
    let reference = |pointer: &str| json!({"result_ref":source["result_ref"],"pointer":pointer});
    let raw = json!({"observations":[
        {"text":if dutch {"De bron meldt {{a}} overeenkomende publicaties; deze opgeslagen pagina bevat {{b}} publicaties."} else {"The source reports {{a}} matching publications; this saved page contains {{b}} publications."},
         "evidence":[reference("/data/totalElements"),reference("/data/numberOfElements")]}
    ],"limitations":[
        if dutch {"Dit is één begrensde pagina uit de Nederlandse TenderNed-bron. Latere pagina's zijn niet opgehaald; de resultaten zijn geen volledige lijst van alle Nederlandse aanbestedingen."} else {"This is one bounded page from the Dutch TenderNed source. Later pages were not retrieved; these results are not a complete list of all Dutch procurement notices."},
        if dutch {"De publicatietypen en datums zijn letterlijk overgenomen. Aankondigingen, gunningen, rectificaties en andere typen kunnen door elkaar staan. De lijst betekent niet dat elke opdracht nog openstaat; controleer de oorspronkelijke publicatie."} else {"Publication types and dates are copied from the source. Announcements, awards, corrections and other types can occur together. The list does not mean every tender is still open; check the original notice."},
        if dutch {"Een ontbrekende sluitingsdatum blijft onbekend. Zoekfrase-overeenkomst bewijst niet dat een opdracht aan alle geschiktheids- of inschrijfcriteria voldoet."} else {"A missing closing date remains unknown. A search-phrase match does not establish that a tender meets every eligibility or bidding criterion."}
    ],"presentation":if rows.is_empty() {json!({"kind":"text"})} else {json!({"kind":"table",
        "title":if dutch {"TenderNed-publicaties"}else{"TenderNed publications"},"result_ref":source["result_ref"],"rows_pointer":"/data/content","row_indices":(0..rows.len()).collect::<Vec<_>>(),
        "columns":[
            {"label":if dutch {"Titel"}else{"Title"},"pointer":"/aanbestedingNaam"},
            {"label":if dutch {"Opdrachtgever"}else{"Buyer"},"pointer":"/opdrachtgeverNaam"},
            {"label":if dutch {"Type publicatie"}else{"Publication type"},"pointer":"/typePublicatie/omschrijving"},
            {"label":if dutch {"Gepubliceerd"}else{"Published"},"pointer":"/publicatieDatum"},
            {"label":if dutch {"Sluitingsdatum"}else{"Closing date"},"pointer":"/sluitingsDatum"},
            {"label":if dutch {"Bron"}else{"Source"},"pointer":"/link/href"}
        ]})}});
    // The quoted bounded page is complete, including a confirmed empty page.
    // Honest pagination/status caveats do not mean an input is still missing.
    crate::parser::validate_analysis(&raw.to_string(), sources, false).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Catalog, Snapshot};

    fn catalog() -> Catalog {
        Catalog::build(
            serde_json::from_str::<Snapshot>(include_str!(
                "../tests/fixtures/tenderned-browse.json"
            ))
            .unwrap(),
        )
    }

    #[test]
    fn parses_topic_and_country_without_invented_missing_inputs_or_required_fields() {
        for (question, topic, country, language) in [
            (
                "Find public tenders for IT services in the Netherlands.",
                "IT services",
                "the Netherlands",
                "en",
            ),
            (
                "Show public tenders for solar panels in Belgium.",
                "solar panels",
                "Belgium",
                "en",
            ),
            (
                "Find public tenders for medical imaging equipment in United Kingdom.",
                "medical imaging equipment",
                "United Kingdom",
                "en",
            ),
            (
                "Zoek openbare aanbestedingen voor laadpalen in Nederland.",
                "laadpalen",
                "Nederland",
                "nl",
            ),
        ] {
            let parsed = parse(question, "fixture").unwrap();
            assert_eq!(parsed.subjects[0].label, topic);
            assert_eq!(parsed.subjects[0].identifiers["country"], country);
            assert_eq!(parsed.language, language);
            assert_eq!(parsed.capabilities[0].outputs, ["publication_page"]);
            assert!(parsed.capabilities[0].missing.is_empty());
        }
        for question in [
            "Find public tenders for IT services in the Netherlands as JSON.",
            "Find public tenders for IT services in the Netherlands before 2025.",
            "Find public tenders for IT services in the Netherlands via TED.",
            "Find public tenders for IT services in the Netherlands. Include award values.",
            "Find public tenders for IT services in the Netherlands and Belgium.",
            "Find public tenders for IT services with deadlines in the Netherlands.",
            "Find public tenders for IT services closing tomorrow in the Netherlands.",
            "Find public tenders for currently open IT services in the Netherlands.",
            "Find public tenders for awarded IT services in the Netherlands.",
            "Find public tenders for latest IT services in the Netherlands.",
            "Find public tenders for top 5 IT services in the Netherlands.",
            "Find public tenders for IT services 2025 in the Netherlands.",
            "Find public tenders for IT services 2025-10-01 in the Netherlands.",
            "Zoek openbare aanbestedingen voor actuele IT-diensten in Nederland.",
            "Find public tenders for IT services in somewhere.",
        ] {
            assert!(parse(question, "fixture").is_none(), "{question}");
        }
    }

    #[test]
    fn exact_phrase_serialization_preserves_explicit_queries_and_other_sources() {
        let service = &catalog().services[0];
        for (input, expected) in [
            ("IT services", "\"IT services\""),
            ("solar panels", "zonnepanelen"),
            ("Solar Panels", "zonnepanelen"),
            ("cleaning services", "schoonmaak"),
            ("zonnepanelen", "zonnepanelen"),
            ("schoonmaak", "schoonmaak"),
            ("cybersecurity", "cybersecurity"),
            ("solar panel installation", "\"solar panel installation\""),
            ("laadpalen", "laadpalen"),
            ("\"IT services\"", "\"IT services\""),
            ("\"solar panels\"", "\"solar panels\""),
            ("\"cleaning services\"", "\"cleaning services\""),
            ("IT OR software", "IT OR software"),
            ("IT AND services", "IT AND services"),
        ] {
            let mut inputs =
                json!({"search":input,"publicatieDatumVanaf":"2025-01-01","cpvCodes":"72000000-5"})
                    .as_object()
                    .unwrap()
                    .clone();
            refine(service, &mut inputs);
            assert_eq!(inputs["search"], expected);
            assert_eq!(inputs["publicatieDatumVanaf"], "2025-01-01");
            assert_eq!(inputs["cpvCodes"], "72000000-5");
        }
        let mut other = service.clone();
        other.api_slug = "other".into();
        let mut inputs = json!({"search":"IT services"}).as_object().unwrap().clone();
        refine(&other, &mut inputs);
        assert_eq!(inputs["search"], "IT services");
        let mut company = service.clone();
        company.contract.capability_slug = "company_intel.search".into();
        let mut inputs = json!({"search":"solar panels"}).as_object().unwrap().clone();
        refine(&company, &mut inputs);
        assert_eq!(inputs["search"], "\"solar panels\"", "a company name is not a translated procurement topic");
    }

    #[test]
    fn no_model_recovery_requires_country_and_unchanged_scope() {
        let catalog = catalog();
        for (question, executable) in [
            (
                "Find public tenders for IT services in the Netherlands.",
                true,
            ),
            ("Find public tenders for solar panels in Belgium.", false),
        ] {
            let parsed_request = parse(question, "fixture").unwrap();
            let matches = crate::ask_v2::search::search(&parsed_request, &catalog);
            let mut intake = Intake {
                parsed_request,
                selected_capabilities: matches
                    .iter()
                    .flat_map(|found| found.operations.iter().cloned())
                    .collect(),
                matches,
                catalog_version: catalog.version.clone(),
                apiosk_sources_searched: 1,
            };
            assert_eq!(executable_intake(question, &intake).is_some(), executable);
            intake.parsed_request.capabilities[0]
                .outputs
                .push("award_value".into());
            assert!(executable_intake(question, &intake).is_none());
        }
    }

    #[tokio::test]
    async fn full_question_keeps_country_and_topic_through_one_priced_source_call_without_a_model()
    {
        let catalog = catalog();
        let parser = crate::parser::Parser::new(
            reqwest::Client::new(),
            "http://127.0.0.1:1".into(),
            String::new(),
            "fixture".into(),
        );
        for question in [
            "Find public tenders for IT services in the Netherlands.",
            "Find public tenders for solar panels in the Netherlands.",
            "Find public tenders for cleaning services in the Netherlands.",
            "Zoek openbare aanbestedingen voor laadpalen in Nederland.",
        ] {
            let intake = parser
                .ask_intake(question, &Default::default(), &catalog, None)
                .await
                .unwrap();
            let topic = intake.parsed_request.subjects[0].label.clone();
            let reply = parser
                .chat_from_capabilities(question, None, &Default::default(), &catalog, &intake)
                .await
                .unwrap();
            assert!(reply.discover, "{}", reply.message);
            let mut intent = reply.intent.unwrap();
            assert_eq!(intent.goals.len(), 1);
            assert_eq!(intent.goals[0].capability, BROWSE);
            assert_eq!(intent.goals[0].constraints["source"], "tenderned");
            assert_eq!(intent.subjects[0].values[SEARCH], topic);
            let mut task = crate::state::Task::default();
            task.capability_intake = Some(intake);
            crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
            let crate::resolver::Resolution::Plan(plan) =
                crate::resolver::resolve(&catalog, &task, &intent)
            else {
                panic!("supplied country and topic must yield a plan")
            };
            assert_eq!(plan.steps.len(), 1);
            assert!(
                plan.cap_atomic > 0,
                "ordinary price and approval remain required"
            );
            let step = &plan.steps[0];
            let inputs = step
                .service
                .inputs(&task, step.entity_ref, &catalog.vocabulary)
                .unwrap();
            let expected = if topic == "solar panels" {
                "zonnepanelen".into()
            } else if topic == "cleaning services" {
                "schoonmaak".into()
            } else if topic.split_whitespace().count() > 1 {
                format!("\"{topic}\"")
            } else {
                topic
            };
            assert_eq!(
                inputs,
                json!({"page":0,"size":20,"sort":"publicatieDatum,desc","search":expected})
            );
        }
    }

    #[tokio::test]
    async fn english_examples_keep_user_topics_and_bind_verified_dutch_source_queries() {
        let catalog = catalog();
        let parser = crate::parser::Parser::new(reqwest::Client::new(), "http://127.0.0.1:1".into(), String::new(), "fixture".into());
        for (topic, native, fixture) in [
            ("solar panels", "zonnepanelen", include_str!("../tests/fixtures/additional-examples/tender-zonnepanelen.json")),
            ("cleaning services", "schoonmaak", include_str!("../tests/fixtures/additional-examples/tender-schoonmaak.json")),
        ] {
            let question = format!("Find public tenders for {topic} in the Netherlands.");
            let intake = parser.ask_intake(&question, &Default::default(), &catalog, None).await.unwrap();
            assert_eq!(intake.parsed_request.language, "en");
            assert_eq!(intake.parsed_request.subjects[0].label, topic);
            let reply = parser.chat_from_capabilities(&question, None, &Default::default(), &catalog, &intake).await.unwrap();
            let mut intent = reply.intent.unwrap();
            assert_eq!(intent.subjects[0].label, topic);
            assert_eq!(intent.subjects[0].values[SEARCH], topic);
            let mut task = crate::state::Task { capability_intake: Some(intake), ..Default::default() };
            crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
            let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(&catalog, &task, &intent) else { panic!("complete tender plan required") };
            assert_eq!(plan.steps.len(), 1);
            let step = &plan.steps[0];
            let inputs = step.service.inputs(&task, step.entity_ref, &catalog.vocabulary).unwrap();
            assert_eq!(inputs, json!({"search":native,"page":0,"size":20,"sort":"publicatieDatum,desc"}));
            let fixture: Value = serde_json::from_str(fixture).unwrap();
            let url = url::Url::parse(fixture["url"].as_str().unwrap()).unwrap();
            assert_eq!(url.query_pairs().find(|(key, _)| key == "search").unwrap().1, native);
            let extracted = step.service.contract.extract(&fixture["body"], &catalog.vocabulary).unwrap();
            assert!(extracted.rejected.is_empty());
            let mut document = json!({"subject":{"entity_ref":step.entity_ref,"label":topic},"source":{"provider":"tenderned","path":"/v2/publicaties","endpoint_id":step.service.endpoint_id},"data":fixture["body"],"missing_fields":[],"coverage":"source_response"});
            record_request(&step.service, &inputs, &mut document);
            let source = json!({"result_ref":uuid::Uuid::new_v4(),"document":document});
            let answer = crate::direct_answer::reviewed_compose(&question, &intent, &[source.clone()]).unwrap().unwrap();
            assert_eq!(answer["status"], "completed");
            assert_eq!(answer["presentation"]["title"], "TenderNed publications");
            assert_eq!(answer["presentation"]["columns"][0]["value"], "Title");
            assert_eq!(answer["presentation"]["rows"].as_array().unwrap().len(), 20);
            assert!(answer["observations"][0]["text"].as_str().unwrap().starts_with("The source reports "));
            assert!(answer["limitations"][0].as_str().unwrap().starts_with("This is one bounded page "));
            for row in answer["presentation"]["rows"].as_array().unwrap() {
                for cell in row.as_array().unwrap() {
                    if let Some(pointer) = cell["pointer"].as_str() {
                        assert_eq!(&cell["value"], source["document"].pointer(pointer).unwrap());
                    }
                }
            }
            let mut wrong = source.clone();
            wrong["document"]["source"]["request_parameters"]["search"] = json!(format!("\"{topic}\""));
            assert!(compose(&question, &intent, &[wrong]).is_none(), "an old English phrase result must not be relabelled as the reviewed Dutch search");
        }
        assert_eq!(parser.calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn recorded_it_phrase_results_keep_notice_types_dates_links_and_pagination() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/tenderned-it-public-page.json"
        ))
        .unwrap();
        let url = url::Url::parse(fixture["url"].as_str().unwrap()).unwrap();
        assert_eq!(
            url.query_pairs()
                .find(|(key, _)| key == "search")
                .unwrap()
                .1,
            "\"IT services\""
        );
        let catalog = catalog();
        let extracted = catalog.services[0]
            .contract
            .extract(&fixture["response"], &catalog.vocabulary)
            .unwrap();
        assert!(extracted.rejected.is_empty());
        let page = &extracted.subjects[0]
            .fact("tenderned.publication_page")
            .unwrap()
            .value;
        assert_eq!(page, &fixture["response"]);
        let rows = page["content"].as_array().unwrap();
        assert_eq!(rows.len(), 20);
        assert!(page["totalElements"].as_u64().unwrap() > rows.len() as u64);
        assert_eq!(page["last"], false);
        assert!(rows
            .iter()
            .any(|row| row["typePublicatie"]["code"] == "AGO"));
        for row in rows {
            assert!(row["aanbestedingNaam"].is_string());
            assert!(row["publicatieDatum"].is_string());
            assert!(row["typePublicatie"]["omschrijving"].is_string());
            let link = url::Url::parse(row["link"]["href"].as_str().unwrap()).unwrap();
            assert_eq!(link.host_str(), Some("www.tenderned.nl"));
        }
    }

    #[test]
    fn saved_tender_table_requires_exact_query_provenance_and_keeps_all_twenty_rows() {
        let question = "Find public tenders for IT services in the Netherlands.";
        let catalog = catalog();
        let service = &catalog.services[0];
        let entity = uuid::Uuid::new_v4();
        let intent = crate::parser::Intent {
            subjects: vec![crate::parser::Subject {
                entity_ref: Some(entity),
                label: "IT services".into(),
                values: BTreeMap::from([(SEARCH.into(), json!("IT services"))]),
            }],
            goals: vec![crate::parser::Goal {
                capability: BROWSE.into(),
                subject: 0,
                constraints: BTreeMap::from([
                    ("source".into(), json!("tenderned")),
                    ("endpoint_id".into(), json!(service.endpoint_id)),
                ]),
            }],
            answer_operations: vec![],
            unresolved_references: vec![],
        };
        let fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/tenderned-it-public-page.json"
        ))
        .unwrap();
        let mut document = json!({"subject":{"entity_ref":entity},"source":{"provider":"tenderned","path":"/v2/publicaties","endpoint_id":service.endpoint_id},
            "data":fixture["response"],"missing_fields":[],"coverage":"source_response"});
        record_request(
            service,
            &json!({"page":0,"size":20,"sort":"publicatieDatum,desc","search":"\"IT services\""}),
            &mut document,
        );
        let source = json!({"result_ref":uuid::Uuid::new_v4(),"document":document});
        let answer = crate::direct_answer::compose(question, &intent, &[source.clone()]).unwrap();
        assert_eq!(answer["status"], "completed");
        assert!(answer["follow_up_question"].is_null());
        assert_eq!(answer["presentation"]["rows"].as_array().unwrap().len(), 20);
        assert_eq!(answer["presentation"]["returned_count"], 20);
        assert_eq!(
            answer["presentation"]["rows"][0][0]["pointer"],
            "/data/content/0/aanbestedingNaam"
        );
        assert_eq!(
            answer["presentation"]["columns"].as_array().unwrap().len(),
            6
        );
        for mutation in [
            "missing_query",
            "unquoted_query",
            "wrong_query",
            "wrong_subject",
            "unsafe_link",
            "extra_filter",
            "empty_positive_page",
            "short_first_page",
            "wrong_page_size",
            "duplicate_notice",
        ] {
            let mut changed = source.clone();
            match mutation {
                "missing_query" => {
                    changed["document"]["source"]
                        .as_object_mut()
                        .unwrap()
                        .remove("request_parameters");
                }
                "unquoted_query" => {
                    changed["document"]["source"]["request_parameters"]["search"] =
                        json!("IT services")
                }
                "wrong_query" => {
                    changed["document"]["source"]["request_parameters"]["search"] =
                        json!("construction")
                }
                "wrong_subject" => {
                    changed["document"]["subject"]["entity_ref"] = json!(uuid::Uuid::new_v4())
                }
                "unsafe_link" => {
                    changed["document"]["data"]["content"][0]["link"]["href"] =
                        json!("https://example.com/wrong-notice")
                }
                "extra_filter" => {
                    changed["document"]["source"]["request_parameters"]["cpvCodes"] =
                        json!("72000000-5")
                }
                "empty_positive_page" => {
                    changed["document"]["data"]["content"] = json!([]);
                    changed["document"]["data"]["numberOfElements"] = json!(0);
                }
                "short_first_page" => {
                    changed["document"]["data"]["content"].as_array_mut().unwrap().truncate(19);
                    changed["document"]["data"]["numberOfElements"] = json!(19);
                }
                "wrong_page_size" => changed["document"]["data"]["size"] = json!(10),
                "duplicate_notice" => changed["document"]["data"]["content"][1] = changed["document"]["data"]["content"][0].clone(),
                _ => unreachable!(),
            }
            assert!(
                compose(question, &intent, &[changed]).is_none(),
                "{mutation}"
            );
        }
        let mut empty = source.clone();
        empty["document"]["data"]["content"] = json!([]);
        empty["document"]["data"]["totalElements"] = json!(0);
        empty["document"]["data"]["numberOfElements"] = json!(0);
        let answer = crate::direct_answer::reviewed_compose(question, &intent, &[empty]).unwrap().unwrap();
        assert_eq!(answer["status"], "completed", "a confirmed empty page is a valid source answer");
        assert!(answer["follow_up_question"].is_null());
        assert_eq!(answer["presentation"]["kind"], "text");
    }
}
