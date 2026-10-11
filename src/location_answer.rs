//! A source-only answer for the complete, plain building-and-neighbourhood
//! request. The saved address and identifier dependencies prove which records
//! belong together; this module never fetches a source or creates a new plan.
use crate::{capability_intake::ParsedRequest, parser::Intent, resolver::Step};
use anyhow::{ensure, Context};
use serde_json::{json, Value};
use uuid::Uuid;

const ROUTES: [(&str, &str); 5] = [
    (
        "location.property-search",
        "/bzk/locatieserver/search/v3_1/free",
    ),
    ("building.bag-unit", "/apiosk/bag-unit"),
    ("building.bag-building", "/apiosk/building"),
    ("location.bag-address-context", "/apiosk/bag-address"),
    ("area.cbs-neighbourhood-2025", "/apiosk/neighbourhood"),
];

fn canonical(question: &str) -> Option<ParsedRequest> {
    let parsed = crate::capability_intake::broad_information::parse(question, "saved-location")?;
    let outputs: std::collections::BTreeSet<_> = parsed
        .capabilities
        .iter()
        .flat_map(|capability| capability.outputs.iter().map(String::as_str))
        .collect();
    (outputs == std::collections::BTreeSet::from(["building_info", "neighbourhood_info"]))
        .then_some(parsed)
}

fn folded(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn same_address(left: &str, right: &str) -> bool {
    let (Some(a), Some(b)) = (
        crate::pdok_address::parse_literal(left),
        crate::pdok_address::parse_literal(right),
    ) else {
        return false;
    };
    folded(&a.street) == folded(&b.street)
        && a.number == b.number
        && a.city.as_deref().map(folded) == b.city.as_deref().map(folded)
        && a.postcode == b.postcode
}

fn scope(parsed: &ParsedRequest, intent: &Intent) -> anyhow::Result<Uuid> {
    ensure!(
        intent.subjects.len() == 1
            && intent.goals.len() == 2
            && intent.unresolved_references.is_empty(),
        "location answer requires the exact combined subject and goals"
    );
    let subject = &intent.subjects[0];
    let address = &parsed.subjects[0].label;
    ensure!(
        same_address(address, &subject.label)
            && subject
                .values
                .get("geo.input.q")
                .and_then(Value::as_str)
                .is_some_and(|q| same_address(address, q)),
        "saved location intent does not preserve the supplied address"
    );
    for capability in ["building.bag-building", "area.cbs-neighbourhood-2025"] {
        let goals: Vec<_> = intent
            .goals
            .iter()
            .filter(|goal| goal.capability == capability)
            .collect();
        ensure!(
            goals.len() == 1,
            "location answer requires each requested record exactly once"
        );
        let goal = goals[0];
        ensure!(
            goal.subject == 0
                && goal.constraints.get("source").and_then(Value::as_str) == Some("pdok")
                && goal
                    .constraints
                    .get("endpoint_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| Uuid::parse_str(id).is_ok())
                && goal
                    .constraints
                    .keys()
                    .all(|key| matches!(key.as_str(), "source" | "endpoint_id")),
            "location answer has changed source or additional constraints"
        );
    }
    subject
        .entity_ref
        .context("location subject has no saved entity")
}

/// Expose only already-quoted dependencies for the same canonical address.
/// `answer_sources` separately requires the document's endpoint and entity to
/// match this plan step. Other questions keep their ordinary source selection.
pub(crate) fn includes_dependency(question: &str, intent: &Intent, step: &Step) -> bool {
    let Some(parsed) = canonical(question) else {
        return false;
    };
    scope(&parsed, intent).is_ok_and(|entity| entity == step.entity_ref)
        && step.service.api_slug == "pdok"
        && step.service.method == "GET"
        && ROUTES.iter().any(|(capability, path)| {
            *capability == step.service.contract.capability_slug && *path == step.service.path
        })
}

struct Source<'a> {
    saved: &'a Value,
}

impl Source<'_> {
    fn data(&self) -> &Value {
        &self.saved["document"]["data"]
    }
    fn reference(&self, pointer: &str) -> Value {
        json!({"result_ref":self.saved["result_ref"], "pointer":pointer})
    }
    fn single(&self, route: &str) -> anyhow::Result<&Value> {
        let data = self.data();
        ensure!(
            data["route"] == route
                && data["coverage"]["complete"] == true
                && data["count"] == 1
                && data["items"]
                    .as_array()
                    .is_some_and(|items| items.len() == 1)
                && data["matched"].is_object()
                && data["matched"] == data["items"][0],
            "saved PDOK record is incomplete or ambiguous: {route}"
        );
        Ok(&data["matched"])
    }
}

fn source<'a>(
    sources: &'a [Value],
    intent: &Intent,
    entity: Uuid,
    index: usize,
) -> anyhow::Result<Source<'a>> {
    let (capability, path) = ROUTES[index];
    let selected: Vec<_> = sources
        .iter()
        .filter(|source| {
            let doc = &source["document"];
            doc["source"]["provider"] == "pdok"
                && doc["source"]["path"] == path
                && doc["subject"]["entity_ref"] == json!(entity)
                && doc["coverage"] == "source_response"
                && source["result_ref"]
                    .as_str()
                    .is_some_and(|value| Uuid::parse_str(value).is_ok())
                && doc["source"]["endpoint_id"]
                    .as_str()
                    .is_some_and(|value| Uuid::parse_str(value).is_ok())
                && intent
                    .goals
                    .iter()
                    .filter(|goal| goal.capability == capability)
                    .all(|goal| doc["source"]["endpoint_id"] == goal.constraints["endpoint_id"])
        })
        .collect();
    ensure!(
        selected.len() == 1,
        "location answer needs one matching saved {capability} source"
    );
    Ok(Source { saved: selected[0] })
}

fn digits(value: &Value, length: usize) -> anyhow::Result<&str> {
    value
        .as_str()
        .filter(|value| value.len() == length && value.bytes().all(|ch| ch.is_ascii_digit()))
        .context("saved BAG identifier is missing or malformed")
}

fn official_url(value: &Value, path: &str) -> anyhow::Result<url::Url> {
    let url = url::Url::parse(value.as_str().context("missing PDOK source URL")?)?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("api.pdok.nl")
            && url.path() == path
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.fragment().is_none(),
        "saved source has an unexpected PDOK URL"
    );
    Ok(url)
}

fn literal_matches(row: &Value, address: &crate::pdok_address::Address) -> bool {
    row["type"] == "adres"
        && row["straatnaam"]
            .as_str()
            .is_some_and(|street| folded(street) == folded(&address.street))
        && row["huisnummer"].as_u64() == Some(address.number.into())
        && address.city.as_ref().is_none_or(|city| {
            row["woonplaatsnaam"]
                .as_str()
                .is_some_and(|value| folded(value) == folded(city))
        })
        && address.postcode.as_ref().is_none_or(|code| {
            row["postcode"]
                .as_str()
                .is_some_and(|value| value.replace(' ', "").eq_ignore_ascii_case(code))
        })
        && ["huisletter", "huisnummertoevoeging"].iter().all(|key| {
            row.get(key).is_none_or(|value| {
                value.is_null() || value.as_str().is_some_and(|value| value.trim().is_empty())
            })
        })
}

fn text(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        !text.trim().is_empty() && text.len() <= 1024 && !text.chars().any(char::is_control)
    })
}

fn number(value: &Value) -> bool {
    value
        .as_f64()
        .is_some_and(|number| number.is_finite() && number >= 0.0)
}

/// `None` leaves other questions alone. A recognized question with incomplete
/// or mismatched saved evidence returns an error, never an invented answer.
pub(crate) fn compose(
    question: &str,
    intent: &Intent,
    sources: &[Value],
) -> anyhow::Result<Option<Value>> {
    let Some(parsed) = canonical(question) else {
        return Ok(None);
    };
    let entity = scope(&parsed, intent)?;
    let dutch = parsed.language == "nl";
    let address = crate::pdok_address::parse_literal(&parsed.subjects[0].label)
        .context("invalid literal address")?;
    ensure!(
        sources.len() == ROUTES.len(),
        "location answer requires the complete saved address chain"
    );
    let references: std::collections::BTreeSet<_> = sources
        .iter()
        .map(|source| source["result_ref"].as_str())
        .collect();
    ensure!(
        references.len() == sources.len(),
        "duplicate location result reference"
    );
    let seed = source(sources, intent, entity, 0)?;
    let unit = source(sources, intent, entity, 1)?;
    let building = source(sources, intent, entity, 2)?;
    let context = source(sources, intent, entity, 3)?;
    let neighbourhood = source(sources, intent, entity, 4)?;
    let response = &seed.data()["response"];
    ensure!(
        response["numFound"] == 1
            && response["start"] == 0
            && response["numFoundExact"] == true
            && response["docs"]
                .as_array()
                .is_some_and(|rows| rows.len() == 1),
        "saved address search is not one complete exact result"
    );
    let seed_row = &response["docs"][0];
    ensure!(
        literal_matches(seed_row, &address) && text(&seed_row["weergavenaam"]),
        "saved address differs from the supplied literal"
    );
    let vbo = digits(&seed_row["adresseerbaarobject_id"], 16)?;
    let address_id = digits(&seed_row["nummeraanduiding_id"], 16)?;
    let unit_row = unit.single("bag-unit")?;
    ensure!(
        unit_row["identificatie"] == vbo,
        "saved registered unit differs from the exact address"
    );
    let unit_url = official_url(
        &unit.data()["source_url"],
        "/kadaster/bag/ogc/v2/collections/verblijfsobject/items",
    )?;
    ensure!(
        unit_url
            .query_pairs()
            .filter(|(key, _)| key == "identificatie")
            .collect::<Vec<_>>()
            == [("identificatie".into(), vbo.into())],
        "saved unit query has another BAG identifier"
    );
    let href = unit_row["single_building_href"]
        .as_str()
        .context("registered unit has no single building link")?;
    ensure!(
        unit_row["pand.href"]
            .as_array()
            .is_some_and(|links| links.len() == 1 && links[0] == href),
        "registered unit links to multiple buildings"
    );
    let building_prefix = "/kadaster/bag/ogc/v2/collections/pand/items/";
    let href_url = url::Url::parse(href)?;
    let building_key = href_url
        .path()
        .strip_prefix(building_prefix)
        .context("unexpected building link")?;
    ensure!(
        Uuid::parse_str(building_key).is_ok(),
        "invalid building item identifier"
    );
    official_url(&json!(href), href_url.path())?;
    ensure!(
        href_url.query().is_none(),
        "unexpected building link parameters"
    );
    let building_row = building.single("building")?;
    digits(&building_row["identificatie"], 16)?;
    official_url(&building.data()["source_url"], href_url.path())?;
    let area_code = seed_row["buurtcode"]
        .as_str()
        .filter(|code| {
            code.len() == 10
                && code.starts_with("BU")
                && code[2..].bytes().all(|ch| ch.is_ascii_alphanumeric())
        })
        .context("exact address has no neighbourhood code")?;
    let aliases = &context.data()["items"];
    let aliases = aliases
        .as_array()
        .context("saved address context has no rows")?;
    ensure!(
        context.data()["route"] == "bag-address"
            && context.data()["coverage"]["complete"] == true
            && context.data()["count"].as_u64() == Some(aliases.len() as u64)
            && !aliases.is_empty(),
        "saved address context is incomplete"
    );
    official_url(
        &context.data()["source_url"],
        "/bzk/locatieserver/search/v3_1/free",
    )?;
    ensure!(
        aliases
            .iter()
            .all(|row| row["bag_object_id"] == vbo && row["neighbourhood_code"] == area_code),
        "registered unit has inconsistent neighbourhood links"
    );
    let matching: Vec<_> = aliases
        .iter()
        .filter(|row| row["bag_address_id"] == address_id)
        .collect();
    ensure!(
        matching.len() == 1 && literal_matches(&matching[0]["source_record"], &address),
        "saved address context does not contain the supplied address"
    );
    let neighbourhood_row = neighbourhood.single("neighbourhood")?;
    ensure!(
        neighbourhood_row["buurtcode"] == area_code && neighbourhood_row["jaar"] == 2025,
        "saved CBS record has another neighbourhood or reporting period"
    );
    let cbs_url = official_url(
        &neighbourhood.data()["source_url"],
        "/cbs/wijken-en-buurten-2025/ogc/v1/collections/buurten/items",
    )?;
    ensure!(
        cbs_url
            .query_pairs()
            .filter(|(key, _)| key == "buurtcode")
            .collect::<Vec<_>>()
            == [("buurtcode".into(), area_code.into())],
        "saved CBS query has another neighbourhood code"
    );

    let mut observations = vec![
        json!({"text":if dutch {"Het opgegeven adres is gekoppeld aan het BAG-adres {{a}}."} else {"The supplied address resolves to the registered BAG address {{a}}."}, "evidence":[seed.reference("/data/response/docs/0/weergavenaam")]}),
    ];
    observations.push(if number(&building_row["bouwjaar"]) && building_row["bouwjaar"].as_f64().is_some_and(|year| year > 0.0) {
        json!({"text":if dutch {"Het gekoppelde pand heeft BAG-identificatie {{a}} en geregistreerd bouwjaar {{b}}."} else {"The linked building has BAG identifier {{a}} and registered construction year {{b}}."}, "evidence":[building.reference("/data/matched/identificatie"),building.reference("/data/matched/bouwjaar")]})
    } else {
        json!({"text":if dutch {"Het gekoppelde pand heeft BAG-identificatie {{a}}. Het bouwjaar is niet beschikbaar."} else {"The linked building has BAG identifier {{a}}. Its construction year is unavailable."}, "evidence":[building.reference("/data/matched/identificatie")]})
    });
    if number(&unit_row["oppervlakte"]) {
        observations.push(json!({"text":if dutch {"Het gekoppelde verblijfsobject heeft een geregistreerde oppervlakte van {{a}} vierkante meter. Dit is de oppervlakte van het verblijfsobject, niet van het gehele pand."} else {"The linked registered unit has an area of {{a}} square metres. This is the unit's area, not the area of the whole building."}, "evidence":[unit.reference("/data/matched/oppervlakte")]}));
    }
    observations.push(json!({"text":if dutch {"De CBS-buurt is {{a}}; de buurtcijfers horen bij verslagjaar {{b}}."} else {"The CBS neighbourhood code is {{a}}; these neighbourhood statistics refer to reporting year {{b}}."}, "evidence":[neighbourhood.reference("/data/matched/buurtcode"),neighbourhood.reference("/data/matched/jaar")]}));
    if number(&neighbourhood_row["aantal_inwoners"])
        && number(&neighbourhood_row["aantal_huishoudens"])
    {
        observations.push(json!({"text":if dutch {"De buurt telt volgens die CBS-publicatie {{a}} inwoners en {{b}} huishoudens."} else {"That CBS publication reports {{a}} residents and {{b}} households in the neighbourhood."}, "evidence":[neighbourhood.reference("/data/matched/aantal_inwoners"),neighbourhood.reference("/data/matched/aantal_huishoudens")]}));
    }
    let mut rows = Vec::new();
    let mut add =
        |label_en: &str, label_nl: &str, saved: &Source<'_>, pointer: &str, numeric: bool| {
            let value = saved.saved["document"].pointer(pointer);
            let cell = value
                .filter(|value| {
                    if numeric {
                        number(value)
                            && (!pointer.ends_with("/bouwjaar")
                                || value.as_f64().is_some_and(|year| year > 0.0))
                    } else {
                        text(value)
                    }
                })
                .map(|_| saved.reference(pointer))
                .unwrap_or(Value::Null);
            rows.push(json!([{ "label":if dutch {label_nl} else {label_en} }, cell]));
        };
    add(
        "Registered address",
        "Geregistreerd adres",
        &seed,
        "/data/response/docs/0/weergavenaam",
        false,
    );
    for (en, nl, key, numeric) in [
        (
            "BAG unit identifier",
            "BAG-identificatie verblijfsobject",
            "identificatie",
            false,
        ),
        (
            "Unit area (m²)",
            "Oppervlakte verblijfsobject (m²)",
            "oppervlakte",
            true,
        ),
        (
            "Registered unit use",
            "Geregistreerd gebruik verblijfsobject",
            "gebruiksdoel",
            false,
        ),
        ("Unit status", "Status verblijfsobject", "status", false),
    ] {
        add(en, nl, &unit, &format!("/data/matched/{key}"), numeric);
    }
    for (en, nl, key, numeric) in [
        (
            "BAG building identifier",
            "BAG-identificatie pand",
            "identificatie",
            false,
        ),
        (
            "Registered construction year",
            "Geregistreerd bouwjaar",
            "bouwjaar",
            true,
        ),
        ("Building status", "Pandstatus", "status", false),
        (
            "Registered building uses",
            "Geregistreerde gebruiksdoelen pand",
            "gebruiksdoel",
            false,
        ),
        (
            "Registered units in building",
            "Verblijfsobjecten in pand",
            "aantal_verblijfsobjecten",
            true,
        ),
    ] {
        add(en, nl, &building, &format!("/data/matched/{key}"), numeric);
    }
    for (en, nl, key, numeric) in [
        ("Neighbourhood", "Buurt", "buurtnaam", false),
        ("Neighbourhood code", "Buurtcode", "buurtcode", false),
        ("Municipality", "Gemeente", "gemeentenaam", false),
        ("CBS reporting year", "CBS-verslagjaar", "jaar", true),
        (
            "Residents in neighbourhood",
            "Inwoners in buurt",
            "aantal_inwoners",
            true,
        ),
        (
            "Households in neighbourhood",
            "Huishoudens in buurt",
            "aantal_huishoudens",
            true,
        ),
        (
            "Average household size (persons)",
            "Gemiddelde huishoudensgrootte (personen)",
            "gemiddelde_huishoudsgrootte",
            true,
        ),
        (
            "Neighbourhood population density (residents/km²)",
            "Bevolkingsdichtheid buurt (inwoners/km²)",
            "bevolkingsdichtheid_inwoners_per_km2",
            true,
        ),
        (
            "Neighbourhood area (ha)",
            "Buurtoppervlakte (ha)",
            "oppervlakte_totaal_in_ha",
            true,
        ),
        (
            "Neighbourhood land area (ha)",
            "Landoppervlakte buurt (ha)",
            "oppervlakte_land_in_ha",
            true,
        ),
        (
            "Neighbourhood water area (ha)",
            "Wateroppervlakte buurt (ha)",
            "oppervlakte_water_in_ha",
            true,
        ),
    ] {
        add(
            en,
            nl,
            &neighbourhood,
            &format!("/data/matched/{key}"),
            numeric,
        );
    }
    add(
        "BAG unit source",
        "Bron verblijfsobject",
        &unit,
        "/data/source_url",
        false,
    );
    add(
        "BAG building source",
        "Bron pand",
        &building,
        "/data/source_url",
        false,
    );
    add(
        "CBS neighbourhood source",
        "Bron CBS-buurt",
        &neighbourhood,
        "/data/source_url",
        false,
    );
    let limitations = if dutch {
        vec![
        "Dit overzicht neemt bronvelden over uit BAG en CBS via PDOK. Geregistreerde gebruiksdoelen en statussen blijven in de oorspronkelijke brontaal staan.",
        "De oppervlakte van het verblijfsobject is geen totale pandoppervlakte. Hetzelfde verblijfsobject kan meerdere geregistreerde adresnamen hebben.",
        "De CBS-cijfers beschrijven de hele buurt in het vermelde verslagjaar; ze beschrijven geen individueel adres en zijn geen actuele bewonersmeting.",
        "Ontbrekende of afgeschermde bronwaarden blijven onbekend. De bronnen onderbouwen hier geen eigendom, verdiepingsaantal of energielabel.",
    ]
    } else {
        vec![
        "This overview copies source fields from BAG and CBS through PDOK. Registered uses and statuses retain their original source language.",
        "The registered unit area is not the total building area. A registered unit may have several address aliases.",
        "CBS figures describe the whole neighbourhood in the stated reporting year, not an individual address or a current resident count.",
        "Missing or suppressed source values remain unknown. These records do not establish ownership, floor count or an energy label.",
    ]
    };
    let raw = json!({"observations":observations,"limitations":limitations,"presentation":{
        "kind":"comparison", "title":if dutch {"Gebouw- en buurtgegevens"} else {"Building and neighbourhood information"},
        "columns":[{"label":if dutch {"Gegeven"} else {"Field"}},{"label":if dutch {"Bronwaarde"} else {"Source value"}}],"rows":rows}});
    let mut answer = crate::parser::validate_analysis(&raw.to_string(), sources, false)?;
    answer["projection"] = json!("saved_pdok_location_fields");
    Ok(Some(answer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalog::{Catalog, Snapshot},
        parser::{Goal, Subject},
        resolver::Plan,
        state::Task,
    };
    use std::collections::BTreeMap;

    const QUESTION: &str = "Find building and neighbourhood information for Damrak 1, Amsterdam.";

    fn fixture() -> (Intent, Vec<Value>, Plan) {
        let raw: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/dutch-links/damrak-normalized-chain.json"
        ))
        .unwrap();
        let catalog = Catalog::build(
            serde_json::from_str::<Snapshot>(include_str!(
                "../tests/fixtures/workflows/catalog.json"
            ))
            .unwrap(),
        );
        let linked_catalog = Catalog::build(serde_json::from_str::<Snapshot>(include_str!(
            "../tests/fixtures/dutch-links/catalog.json"
        )).unwrap());
        let entity = Uuid::new_v4();
        let steps: Vec<_> = ROUTES
            .iter()
            .map(|(capability, _)| Step {
                service: catalog
                    .services
                    .iter()
                    .chain(linked_catalog.services.iter())
                    .find(|service| {
                        service.api_slug == "pdok"
                            && service.contract.capability_slug == *capability
                    })
                    .unwrap()
                    .clone(),
                entity_ref: entity,
                goal_index: usize::from(*capability == "area.cbs-neighbourhood-2025"),
                browser_fallback: None,
            })
            .collect();
        let intent = Intent {
            subjects: vec![Subject {
                entity_ref: Some(entity),
                label: "Damrak 1, Amsterdam".into(),
                values: BTreeMap::from([("geo.input.q".into(), json!("Damrak 1, Amsterdam"))]),
            }],
            goals: [2, 4]
                .into_iter()
                .map(|index| Goal {
                    capability: steps[index].service.contract.capability_slug.clone(),
                    subject: 0,
                    constraints: BTreeMap::from([
                        ("source".into(), json!("pdok")),
                        (
                            "endpoint_id".into(),
                            json!(steps[index].service.endpoint_id),
                        ),
                    ]),
                })
                .collect(),
            answer_operations: vec![],
            unresolved_references: vec![],
        };
        let sources = steps.iter().zip(["address", "bag-unit", "building", "bag-address", "neighbourhood"]).map(|(step, key)| json!({
            "result_ref":Uuid::new_v4(), "document": {"data":raw[key],"source":{
                "provider":"pdok","path":step.service.path,"endpoint_id":step.service.endpoint_id,"name":step.service.name,"contract_version":1},
                "subject":{"entity_ref":entity,"label":"Damrak 1, Amsterdam"},"coverage":"source_response","missing_fields":[]}
        })).collect();
        let plan = Plan { compute: None,
            quote_ref: Uuid::new_v4(),
            steps,
            cap_atomic: 1,
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(5),
            hash: "fixture".into(),
            fee_bps: 1500,
            workflow_slug: None,
        };
        (intent, sources, plan)
    }

    fn cell<'a>(answer: &'a Value, label: &str) -> &'a Value {
        &answer["presentation"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row[0]["value"] == label)
            .unwrap()[1]
    }

    #[test]
    fn actual_public_chain_returns_meaningful_source_cited_records_without_a_model() {
        let (intent, sources, _) = fixture();
        let answer = compose(QUESTION, &intent, &sources).unwrap().unwrap();
        assert_eq!(answer["status"], "completed");
        assert!(answer["follow_up_question"].is_null());
        assert_eq!(cell(&answer, "Registered construction year")["value"], 1890);
        assert_eq!(cell(&answer, "Unit area (m²)")["value"], 16100);
        assert_eq!(
            cell(&answer, "Registered unit use")["value"],
            "bijeenkomstfunctie,logiesfunctie"
        );
        assert_eq!(
            cell(&answer, "Registered building uses")["value"],
            "bijeenkomstfunctie,logiesfunctie,winkelfunctie"
        );
        assert_eq!(cell(&answer, "CBS reporting year")["value"], 2025);
        assert_eq!(cell(&answer, "Residents in neighbourhood")["value"], 485);
        assert_eq!(cell(&answer, "Households in neighbourhood")["value"], 345);
        assert_eq!(
            sources[3]["document"]["data"]["count"], 4,
            "legitimate aliases of the same registered unit must remain usable"
        );
        assert!(cell(&answer, "BAG building source")["value"]
            .as_str()
            .unwrap()
            .starts_with("https://api.pdok.nl/kadaster/bag/"));
        let shared = crate::direct_answer::reviewed_compose(QUESTION, &intent, &sources)
            .unwrap()
            .unwrap();
        assert_eq!(
            answer, shared,
            "the reviewed path must use the same saved-source projection"
        );
        for observation in answer["observations"].as_array().unwrap() {
            for evidence in observation["evidence"].as_array().unwrap() {
                let saved = sources
                    .iter()
                    .find(|source| source["result_ref"] == evidence["result_ref"])
                    .unwrap();
                assert_eq!(
                    saved["document"]
                        .pointer(evidence["pointer"].as_str().unwrap())
                        .unwrap(),
                    &evidence["value"]
                );
            }
        }
    }

    #[test]
    fn only_whole_combined_requests_use_this_projection() {
        let (intent, sources, _) = fixture();
        for question in [
            "Find building information for Damrak 1, Amsterdam.",
            "Find neighbourhood information for Damrak 1, Amsterdam.",
            "Find building and neighbourhood information for Damrak 1, Amsterdam in 2020.",
            "Find building and neighbourhood information for Damrak 1, Amsterdam, including floor count.",
            "Find building and neighbourhood information for Damrak 1, Amsterdam as JSON.",
        ] { assert!(compose(question, &intent, &sources).unwrap().is_none(), "{question}"); }
        let dutch = compose(
            "Geef gebouw- en buurtinformatie voor Damrak 1, Amsterdam.",
            &intent,
            &sources,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            cell(&dutch, "Oppervlakte verblijfsobject (m²)")["value"],
            16100
        );
        assert!(dutch["observations"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("Het opgegeven adres"));
    }

    #[test]
    fn address_identity_completeness_and_each_link_fail_closed() {
        let (intent, sources, _) = fixture();
        let cases: Vec<(usize, &str, Value)> = vec![
            (0, "/document/data/response/docs/0/huisnummer", json!(5)),
            (0, "/document/data/response/docs/0/huisletter", json!("A")),
            (0, "/document/data/response/numFound", json!(2)),
            (0, "/document/data/response/numFoundExact", json!(false)),
            (1, "/document/data/coverage/complete", json!(false)),
            (1, "/document/data/source_url", json!("https://api.pdok.nl/kadaster/bag/ogc/v2/collections/verblijfsobject/items?identificatie=0000000000000001")),
            (2, "/document/data/source_url", json!("https://api.pdok.nl/kadaster/bag/ogc/v2/collections/pand/items/aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa?f=json")),
            (3, "/document/data/count", json!(5)),
            (3, "/document/data/items/0/source_record/huisnummer", json!(5)),
            (3, "/document/data/items/0/neighbourhood_code", json!("BU0000XX00")),
            (4, "/document/data/source_url", json!("https://api.pdok.nl/cbs/wijken-en-buurten-2025/ogc/v1/collections/buurten/items?buurtcode=BU0000XX00")),
            (4, "/document/subject/entity_ref", json!(Uuid::new_v4())),
            (4, "/document/source/provider", json!("other")),
            (4, "/document/source/endpoint_id", json!(Uuid::new_v4())),
        ];
        for (index, pointer, value) in cases {
            let mut altered = sources.clone();
            // This field is intentionally absent on the unsuffixed seed.
            if pointer.ends_with("/huisletter") {
                altered[index]["document"]["data"]["response"]["docs"][0]["huisletter"] = value;
            } else {
                *altered[index].pointer_mut(pointer).unwrap() = value;
            }
            assert!(
                compose(QUESTION, &intent, &altered).is_err(),
                "{index} {pointer}"
            );
        }
        for (index, key, value) in [
            (1, "identificatie", json!("0000000000000001")),
            (4, "buurtcode", json!("BU0000XX00")),
            (4, "jaar", json!(2024)),
        ] {
            let mut altered = sources.clone();
            altered[index]["document"]["data"]["matched"][key] = value.clone();
            altered[index]["document"]["data"]["items"][0][key] = value;
            assert!(
                compose(QUESTION, &intent, &altered).is_err(),
                "{index} {key}"
            );
        }
        let mut other = intent.clone();
        other.subjects[0]
            .values
            .insert("geo.input.q".into(), json!("Damrak 5, Amsterdam"));
        assert!(compose(QUESTION, &other, &sources).is_err());
        let mut other = intent.clone();
        other.goals[0]
            .constraints
            .insert("year".into(), json!(2020));
        assert!(compose(QUESTION, &other, &sources).is_err());
        assert!(compose(QUESTION, &intent, &sources[..4]).is_err());
    }

    #[test]
    fn missing_or_suppressed_statistics_and_unknown_year_do_not_become_numbers() {
        let (intent, mut sources, _) = fixture();
        for (index, key, value) in [
            (4, "aantal_inwoners", json!(-99997)),
            (4, "aantal_huishoudens", Value::Null),
            (2, "bouwjaar", json!(0)),
        ] {
            sources[index]["document"]["data"]["matched"][key] = value.clone();
            sources[index]["document"]["data"]["items"][0][key] = value;
        }
        let answer = compose(QUESTION, &intent, &sources).unwrap().unwrap();
        assert!(cell(&answer, "Residents in neighbourhood").is_null());
        assert!(cell(&answer, "Households in neighbourhood").is_null());
        assert!(cell(&answer, "Registered construction year").is_null());
        assert!(!answer.to_string().contains("-99997"));
        assert_eq!(answer["status"], "completed", "a broad available-information answer can label unavailable fields without asking for the address again");
    }

    #[test]
    fn saved_dependencies_are_scoped_to_the_question_entity_and_quoted_plan() {
        let (intent, sources, mut plan) = fixture();
        let mut documents = BTreeMap::new();
        let mut task = Task::default();
        task.record_turn(QUESTION, None);
        for source in &sources {
            let id = Uuid::parse_str(source["result_ref"].as_str().unwrap()).unwrap();
            task.active_results.push(id);
            documents.insert(id, source["document"].clone());
        }
        // Even an active saved result cannot join the answer without matching
        // the exact quoted step's endpoint and entity.
        for key in ["entity_ref", "endpoint_id", "provider"] {
            let mut wrong = sources[0]["document"].clone();
            if key == "entity_ref" {
                wrong["subject"][key] = json!(Uuid::new_v4());
            } else {
                wrong["source"][key] = if key == "provider" {
                    json!("other")
                } else {
                    json!(Uuid::new_v4())
                };
            }
            let id = Uuid::new_v4();
            task.active_results.push(id);
            documents.insert(id, wrong);
        }
        let selected = crate::engine::answer_sources(&plan, &intent, &task, &documents);
        assert_eq!(selected.len(), 5);
        assert_eq!(
            compose(QUESTION, &intent, &selected).unwrap().unwrap()["status"],
            "completed"
        );
        plan.steps.remove(1);
        assert_eq!(
            crate::engine::answer_sources(&plan, &intent, &task, &documents).len(),
            4,
            "unquoted dependencies cannot become evidence"
        );
        task.record_turn("Find building information for Damrak 1, Amsterdam.", None);
        assert_eq!(
            crate::engine::answer_sources(&plan, &intent, &task, &documents).len(),
            2,
            "single-scope requests keep ordinary source selection"
        );
    }
}
