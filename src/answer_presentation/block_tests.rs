//! Version-2 block rules (design 3.2 and 3.3), the golden analysis and the
//! version-1 mapping.
use super::{blocks_from_presentation, legacy_kind, set_conclusion_scope, validate, validate_blocks};
use crate::answer_blocks::*;
use serde_json::{json, Value};

const CONTEXT: &str = include_str!("../../tests/fixtures/answer-blocks/context.json");
const MODEL: &str = include_str!("../../tests/fixtures/answer-blocks/model-output.json");
const VALIDATED: &str = include_str!("../../tests/fixtures/answer-blocks/validated.json");

fn fixture(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn sources() -> Vec<Value> {
    vec![
        json!({"result_ref":"r1","document":{"source":{"name":"Registry","retrieved_at":"2026-09-24T19:02:11Z"},"subject":{"label":"Mollie B.V.","entity_ref":"e1"},"coverage":"source_response",
            "report_assets":{"assets":[{"kind":"document"},{"kind":"map"}]},
            "data":{"name":"Mollie B.V.","kvk":"30204462","status":"Actief","as_of":"2026-09-01","total":5,"next":null,
                "years":[{"year":2023,"revenue":1200000,"currency":"EUR","employees":700},{"year":2024,"revenue":1500000,"currency":"EUR","employees":800,"estimated":"geschat"},{"year":"2025","revenue":null,"currency":"EUR"}],
                "items":[
                    {"name":"Alpha","amount":-50,"currency":"EUR","url":"https://example.org/a","date":"2024-05-02","point":"POINT(4.89 52.37)"},
                    {"name":"Beta","amount":0,"currency":"EUR","url":"http://example.org/b","date":"2024","point":null},
                    {"name":"Gamma","amount":120,"currency":"USD","url":"https://user:pw@example.org/c","date":"binnenkort"},
                    {"name":"Delta","amount":null,"currency":"EUR","date":"2023-11-30T10:00:00+01:00"},
                    {"name":"Epsilon","amount":120,"currency":"EUR","date":null}],
                "officers":[{"name":"J. Jansen","role":"Bestuurder","kvk":"30204462","since":"2019-04-01"},{"name":"P. Pieters","role":"Commissaris","since":"onbekend"}]}}}),
        json!({"result_ref":"r2","document":{"source":{"name":"Registry","retrieved_at":"2026-09-24T19:02:14Z"},"subject":{"label":"Adyen N.V.","entity_ref":"e2"},"coverage":"source_response",
            "data":{"name":"Adyen N.V.","kvk":"34259528","parent_kvk":"30204462","years":[{"year":2024,"revenue":1900000,"currency":"EUR"},{"year":2024,"revenue":1800000,"currency":"USD"}]}}}),
        json!({"result_ref":"r3","document":{"source":{"name":"Person register"},"subject":{"label":"J. Jansen","entity_ref":"e3"},"coverage":"partial","data":{"name":"J. Jansen","birth":"1970"}}}),
    ]
}

fn requirements() -> Vec<Requirement> {
    serde_json::from_value(json!([
        {"id":"r1","criterion":"company.profile","criterion_kind":"capability","status":"answered"},
        {"id":"r2","criterion":"company.accounts","criterion_kind":"capability","status":"unknown","reason":"not_in_source"}
    ]))
    .unwrap()
}

fn status() -> Vec<SourceStatus> {
    serde_json::from_value(json!([
        {"step_key":"svc:e1","result_ref":"r1","source_name":"Registry","state":"completed"},
        {"step_key":"svc:e2","result_ref":"r2","source_name":"Registry","state":"completed"},
        {"step_key":"accounts:e1","source_name":"Accounts","state":"pending"},
        {"step_key":"people:e3","result_ref":"r3","source_name":"Person register","state":"completed"}
    ]))
    .unwrap()
}

fn derivations() -> Vec<Derivation> {
    serde_json::from_value(json!([
        {"id":"d1","op":"difference","inputs":[{"result_ref":"r1","pointer":"/data/years/1/revenue"},{"result_ref":"r1","pointer":"/data/years/0/revenue"}],"periods":"compare",
         "result":{"value":300000,"state":"derived","currency":{"value":"EUR","basis":"derivation"},"inputs":[],"complete":true,"formula":"f"}},
        {"id":"d2","op":"percentage","inputs":[{"derivation_id":"d1"},{"result_ref":"r1","pointer":"/data/years/0/revenue"}],"periods":"compare","decimals":1,
         "result":{"value":25,"state":"derived","unit":{"value":"percent","basis":"operation"},"inputs":[],"complete":true,"formula":"f"}},
        {"id":"d3","op":"difference","inputs":[{"result_ref":"r2","pointer":"/data/years/0/revenue"},{"result_ref":"r1","pointer":"/data/years/1/revenue"}],"periods":"same",
         "result":{"value":400000,"state":"derived","inputs":[],"complete":true,"formula":"f"}},
        {"id":"d4","op":"sum","inputs":[{"result_ref":"r1","pointer":"/data/years/2/revenue"}]},
        {"id":"d5","op":"difference","inputs":[{"result_ref":"r2","pointer":"/data/years/0/revenue"},{"result_ref":"r1","pointer":"/data/years/1/revenue"}],"periods":"same",
         "result":{"value":400000,"state":"derived","inputs":[],"complete":true,"formula":"f"}}
    ]))
    .unwrap()
}

struct Fixture {
    sources: Vec<Value>,
    derivations: Vec<Derivation>,
    observations: Vec<String>,
    missing: Vec<String>,
    requirements: Vec<Requirement>,
    status: Vec<SourceStatus>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            sources: sources(),
            derivations: derivations(),
            observations: vec!["o1".into(), "o2".into()],
            missing: vec!["m1".into()],
            requirements: requirements(),
            status: status(),
        }
    }
    fn context(&self, provisional: bool) -> BlockContext<'_> {
        BlockContext {
            question: "Welke bedrijven staan in de Top 10?",
            sources: &self.sources,
            derivations: &self.derivations,
            observation_ids: &self.observations,
            limitation_count: 1,
            missing_ids: &self.missing,
            requirements: &self.requirements,
            source_status: &self.status,
            provisional,
        }
    }
    /// One block through `validate_blocks`: the validated block or its rejection.
    fn one(&self, block: Value) -> Result<AnswerBlock, BlockRejection> {
        let mut outcome = validate_blocks(&json!([block]), &self.context(false));
        match outcome.rejected.pop() {
            Some(rejection) => Err(rejection),
            None => Ok(outcome.blocks.into_iter().find(|block| block.kind() != BlockKind::Narrative || block.produced_by == BlockProducer::Model).unwrap()),
        }
    }
    fn issue(&self, block: Value) -> (BlockIssue, String) {
        let rejection = self.one(block).unwrap_err();
        (rejection.issue, rejection.path)
    }
    fn body(&self, block: Value) -> Value {
        serde_json::to_value(self.one(block).unwrap()).unwrap()
    }
}

#[test]
fn golden_model_output_validates_to_the_golden_blocks() {
    let (context, model, validated) = (fixture(CONTEXT), fixture(MODEL), fixture(VALIDATED));
    let extension = AnswerExtension::read(&validated).unwrap();
    let requirements = extension.completeness.unwrap().requirements;
    let status: Vec<SourceStatus> = serde_json::from_value(context["source_status"].clone()).unwrap();
    let ctx = BlockContext {
        question: context["question"].as_str().unwrap(),
        sources: context["sources"].as_array().unwrap(),
        derivations: &extension.derivations,
        observation_ids: &["o1".into(), "o2".into()],
        limitation_count: 1,
        missing_ids: &["m1".into()],
        requirements: &requirements,
        source_status: &status,
        provisional: false,
    };
    let outcome = validate_blocks(&model["blocks"], &ctx);
    assert_eq!(outcome.rejected, []);
    assert_eq!(serde_json::to_value(&outcome.blocks).unwrap(), validated["blocks"]);
    assert_eq!(legacy_kind(&outcome.blocks), validated["presentation"]["kind"]);
    assert_eq!(legacy_presentation_stub(legacy_kind(&outcome.blocks)), validated["presentation"]);
}

#[test]
fn golden_validated_analysis_round_trips_through_the_contract_types() {
    let validated = fixture(VALIDATED);
    let extension = AnswerExtension::read(&validated).unwrap();
    let mut expected = validated.clone();
    for key in ["status", "observations", "limitations", "follow_up_question", "presentation"] {
        expected.as_object_mut().unwrap().remove(key);
    }
    assert_eq!(serde_json::to_value(&extension).unwrap(), expected);
    let mut rebuilt = json!({"status":"partial","observations":validated["observations"],"limitations":validated["limitations"],"follow_up_question":null,"presentation":validated["presentation"]});
    extension.attach(&mut rebuilt).unwrap();
    assert_eq!(rebuilt, validated);
}

#[test]
fn one_invalid_block_never_rejects_the_others() {
    let fixture = Fixture::new();
    let blocks = json!([
        {"id":"b1","kind":"narrative","observation_ids":["o1"]},
        {"id":"b2","kind":"gauge","value":1},
        {"id":"b3","kind":"kpi","cards":"not a list"},
        {"id":"b4","kind":"data_table","collection":{"result_ref":"r1","rows_pointer":"/data/items"},"columns":[{"id":"name","label":"Naam","pointer":"/name"}]},
        {"id":"b5","kind":"kpi","cards":[{"id":"k1","label":"Omzet","fact":{"result_ref":"r1","pointer":"/data/years/1/revenue"}}]},
        {"id":"b5","kind":"narrative","observation_ids":[]},
        {"id":"x6","kind":"narrative","observation_ids":[]}
    ]);
    let outcome = validate_blocks(&blocks, &fixture.context(false));
    let rejected: Vec<_> = outcome.rejected.iter().map(|r| (r.block_id.as_deref(), r.issue, r.path.as_str())).collect();
    assert_eq!(rejected, [
        (Some("b2"), BlockIssue::UnsupportedKind, "kind"),
        (Some("b3"), BlockIssue::Schema, ""),
        (Some("b4"), BlockIssue::MissingRowSelection, "collection/row_indices"),
        (Some("b5"), BlockIssue::DuplicateId, "id"),
        (Some("x6"), BlockIssue::Schema, "id"),
    ]);
    let kept: Vec<_> = outcome.blocks.iter().map(|b| (b.id.as_str(), b.kind(), b.produced_by)).collect();
    assert_eq!(kept, [("b1", BlockKind::Narrative, BlockProducer::Model), ("b5", BlockKind::Kpi, BlockProducer::Model), ("b6", BlockKind::Narrative, BlockProducer::Deterministic)]);
    assert_eq!(outcome.blocks[2].body, BlockBody::Narrative(NarrativeBlock { observation_ids: vec!["o2".into()] }), "no validated finding is lost");
    let nothing = validate_blocks(&json!("blocks"), &fixture.context(false));
    assert_eq!((nothing.rejected[0].issue, nothing.blocks[0].id.as_str()), (BlockIssue::Schema, "b1"));
}

#[test]
fn unplaced_findings_lead_when_no_narrative_exists_and_summaries_place_them() {
    let fixture = Fixture::new();
    let kpi = json!({"id":"b1","kind":"kpi","cards":[{"id":"k1","label":"Omzet","fact":{"result_ref":"r1","pointer":"/data/years/1/revenue"}}]});
    let outcome = validate_blocks(&json!([kpi]), &fixture.context(false));
    assert_eq!(outcome.blocks[0].kind(), BlockKind::Narrative);
    assert_eq!(outcome.blocks[0].id, "b2");
    let summary = json!({"id":"b1","kind":"summary","finding_ids":["o1","o2"],"open_point_ids":["m1","l1","r2"]});
    let outcome = validate_blocks(&json!([summary]), &fixture.context(false));
    assert_eq!(outcome.blocks.len(), 1, "a summary that lists every finding needs no extra narrative");
    for (open, id) in [("l2", "l2"), ("r1", "r1"), ("m2", "m2")] {
        let bad = json!({"id":"b1","kind":"summary","finding_ids":[],"open_point_ids":[open]});
        assert_eq!(fixture.issue(bad), (BlockIssue::UnknownReference, "open_point_ids/0".into()), "{id}: only limitations, missing information or unknown requirements");
    }
    let repeated = json!([{"id":"b1","kind":"narrative","observation_ids":["o1"]},{"id":"b2","kind":"narrative","observation_ids":["o1","o2"]}]);
    let outcome = validate_blocks(&repeated, &fixture.context(false));
    assert_eq!(outcome.rejected[0].issue, BlockIssue::DuplicateId);
    assert_eq!(fixture.issue(json!({"id":"b1","kind":"narrative","observation_ids":["o9"]})).0, BlockIssue::UnknownReference);
}

#[test]
fn common_header_rules_apply_to_every_kind() {
    let fixture = Fixture::new();
    let kpi = |extra: Value| {
        let mut block = json!({"id":"b1","kind":"kpi","cards":[{"id":"k1","label":"Omzet","fact":{"result_ref":"r1","pointer":"/data/years/1/revenue"}}]});
        block.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        block
    };
    assert_eq!(fixture.issue(kpi(json!({"title":"Omzet 2024"}))), (BlockIssue::InvalidLabel, "title".into()));
    assert!(fixture.one(kpi(json!({"title":"De Top 10"}))).is_ok(), "digits quoted from the question are allowed");
    assert_eq!(fixture.issue(kpi(json!({"requirement_ids":["r9"]}))), (BlockIssue::UnknownReference, "requirement_ids/0".into()));
    assert_eq!(fixture.issue(kpi(json!({"notes":["Zie https://example.org"]}))), (BlockIssue::InvalidLabel, "notes/0".into()));
    assert_eq!(fixture.issue(kpi(json!({"notes":["a","b","c","d","e"]}))).0, BlockIssue::TooManyItems);
    let block = fixture.one(kpi(json!({"provisional":true,"produced_by":"deterministic","coverage":{"selected":1,"fetched":1,"paginated":false,"scope":"complete"}}))).unwrap();
    assert_eq!((block.provisional, block.produced_by, block.coverage), (false, BlockProducer::Model, None), "server fields are never taken from the model");
    let statuses: Vec<_> = block.source_status.iter().map(|s| s.step_key.as_str()).collect();
    assert_eq!(statuses, ["svc:e1", "accounts:e1"], "cited results plus pending steps for the same subject");
    let duplicate = json!({"id":"b1","kind":"kpi","cards":[{"id":"k1","label":"A","fact":{"result_ref":"r1","pointer":"/data/name"}},{"id":"k1","label":"B","fact":{"result_ref":"r1","pointer":"/data/kvk"}}]});
    assert_eq!(fixture.issue(duplicate), (BlockIssue::DuplicateId, "cards/1/id".into()));
}

#[test]
fn provisional_answers_state_no_conclusion_and_scope_follows_completeness() {
    let fixture = Fixture::new();
    let summary = json!({"id":"b1","kind":"summary","conclusion":{"text":"Mollie heeft kvk-nummer {{a}}.","facts":[{"result_ref":"r1","pointer":"/data/kvk"}]},"finding_ids":["o1","o2"],"open_point_ids":[]});
    let provisional = validate_blocks(&json!([summary.clone()]), &fixture.context(true));
    assert_eq!((provisional.rejected[0].issue, provisional.rejected[0].path.as_str()), (BlockIssue::ConclusionOnIncomplete, "conclusion"));
    assert!(provisional.blocks.iter().all(|block| block.provisional));
    let mut blocks = validate_blocks(&json!([summary]), &fixture.context(false)).blocks;
    let conclusion = |blocks: &[AnswerBlock]| match &blocks[0].body {
        BlockBody::Summary(summary) => summary.conclusion.clone().unwrap(),
        other => panic!("{other:?}"),
    };
    assert_eq!((conclusion(&blocks).text.as_str(), conclusion(&blocks).scope), ("Mollie heeft kvk-nummer 30204462.", ConclusionScope::Partial));
    set_conclusion_scope(&mut blocks, true);
    assert_eq!(conclusion(&blocks).scope, ConclusionScope::Complete);
    set_conclusion_scope(&mut blocks, false);
    assert_eq!(conclusion(&blocks).scope, ConclusionScope::Partial);
    for (text, facts, issue) in [
        ("Omzet van twaalf miljoen in 2024.", json!([]), BlockIssue::InvalidLabel),
        ("Omzet {{a}} en {{b}}.", json!([{"result_ref":"r1","pointer":"/data/kvk"}]), BlockIssue::UnknownReference),
        ("Omzet {{a}}.", json!([{"result_ref":"r1","pointer":"/data/years/2/revenue"}]), BlockIssue::MissingField),
        ("Omzet {{e}}.", json!([]), BlockIssue::InvalidLabel),
    ] {
        let block = json!({"id":"b1","kind":"summary","conclusion":{"text":text,"facts":facts},"finding_ids":[],"open_point_ids":[]});
        assert_eq!(fixture.issue(block).0, issue, "{text}");
    }
}

#[test]
fn blocks_over_the_task_budget_are_dropped_from_the_end() {
    let mut fixture = Fixture::new();
    let rows: Vec<Value> = (0..200).map(|i| json!({"name":format!("{i:0>900}")})).collect();
    fixture.sources.push(json!({"result_ref":"big","document":{"data":{"rows":rows}}}));
    let table = |id: &str| json!({"id":id,"kind":"data_table","collection":{"result_ref":"big","rows_pointer":"/data/rows","row_indices":(0..100).collect::<Vec<_>>()},"columns":[{"id":"name","label":"Naam","pointer":"/name"}]});
    let outcome = validate_blocks(&json!([table("b1"), table("b2")]), &fixture.context(false));
    assert_eq!(outcome.rejected, [BlockRejection::new(Some("b2"), Some(BlockKind::DataTable), BlockIssue::TooLarge, "")]);
    assert!(serde_json::to_vec(&outcome.blocks).unwrap().len() <= MAX_BLOCKS_BYTES);
}

#[test]
fn data_table_copies_selected_records_with_counts_and_keeps_unknown_cells() {
    let fixture = Fixture::new();
    let table = fixture.body(json!({"id":"b1","kind":"data_table","title":"Overzicht",
        "collection":{"result_ref":"r1","rows_pointer":"/data/items","row_indices":[4,0,3],"total_pointer":"/data/total"},
        "columns":[{"id":"name","label":"Naam","pointer":"/name","role":"label"},{"id":"amount","label":"Bedrag","pointer":"/amount","role":"amount","currency":{"pointer":"/currency"}},
                   {"id":"link","label":"Bron","pointer":"/url","role":"link","hidden":true}],
        "rows":[{"cells":[{"value":"invented"}]}],"sort":{"column_id":"amount","direction":"desc"}}));
    assert_eq!(table["coverage"], json!({"selected":3,"fetched":5,"total_available":5,"paginated":false,"scope":"selection"}));
    assert_eq!(table["rows"].as_array().unwrap().iter().map(|r| r["row_index"].clone()).collect::<Vec<_>>(), [json!(4), json!(0), json!(3)]);
    assert_eq!(table["rows"][0]["cells"][0], json!({"value":"Epsilon"}), "the model's rows are discarded");
    assert_eq!(table["rows"][2]["cells"][1], json!({"value":null,"state":"missing"}));
    assert_eq!(table["rows"][1]["cells"][1], json!({"value":-50}));
    assert!(table["rows"][2]["cells"][2].is_null(), "an absent field is an unknown cell");
    assert_eq!(table["columns"][1]["currency"], json!({"pointer":"/currency","value":"EUR","basis":"evidence"}));
    let mixed = fixture.body(json!({"id":"b1","kind":"data_table","collection":{"result_ref":"r1","rows_pointer":"/data/items","row_indices":[0,2]},
        "columns":[{"id":"amount","label":"Bedrag","pointer":"/amount","currency":{"pointer":"/currency"}}]}));
    assert!(mixed["columns"][0].get("currency").is_none());
    assert_eq!(mixed["rows"][1]["cells"][0]["currency"], json!({"value":"USD","basis":"evidence"}), "records in another currency say so per cell");
    let explicit = json!({"id":"b1","kind":"data_table","columns":[{"id":"name","label":"Naam"},{"id":"kvk","label":"KVK"}],
        "rows":[{"cells":[{"result_ref":"r1","pointer":"/data/name"},null]},{"cells":[{"result_ref":"r2","pointer":"/data/name"},{"result_ref":"r2","pointer":"/data/kvk"}]}]});
    let body = fixture.body(explicit.clone());
    assert!(body["rows"][0]["cells"][1].is_null() && body.get("coverage").is_none());
    let mut wide = explicit.clone();
    wide["rows"][0]["cells"] = json!([null]);
    assert_eq!(fixture.issue(wide), (BlockIssue::WidthMismatch, "rows/0/cells".into()));
    let mut long = explicit.clone();
    long["rows"] = json!((0..51).map(|_| json!({"cells":[null,null]})).collect::<Vec<_>>());
    assert_eq!(fixture.issue(long).0, BlockIssue::TooManyItems);
    let mut sorted = explicit;
    sorted["sort"] = json!({"column_id":"missing"});
    assert_eq!(fixture.issue(sorted), (BlockIssue::UnknownReference, "sort/column_id".into()));
    let no_pointer = json!({"id":"b1","kind":"data_table","collection":{"result_ref":"r1","rows_pointer":"/data/items","row_indices":[0]},"columns":[{"id":"name","label":"Naam"}]});
    assert_eq!(fixture.issue(no_pointer), (BlockIssue::InvalidPointer, "columns/0/pointer".into()));
    let too_wide = json!({"id":"b1","kind":"data_table","columns":(0..9).map(|i| json!({"id":format!("c{i}"),"label":"Kolom"})).collect::<Vec<_>>(),"rows":[]});
    assert_eq!(fixture.issue(too_wide).0, BlockIssue::TooManyItems);
}

#[test]
fn comparison_names_differences_and_never_picks_a_winner() {
    let fixture = Fixture::new();
    let revenue = |r: &str, i: usize, currency: bool| {
        let mut fact = json!({"result_ref":r,"pointer":format!("/data/years/{i}/revenue"),"period":{"pointer":format!("/data/years/{i}/year")}});
        if currency {
            fact["currency"] = json!({"pointer":format!("/data/years/{i}/currency")});
        }
        fact
    };
    let block = |cells: Value| json!({"id":"b1","kind":"comparison","entities":[{"id":"e1","label":"Mollie B.V.","name":{"result_ref":"r1","pointer":"/data/name"}},{"id":"e2","label":"Adyen N.V."}],
        "rows":[{"id":"c1","label":"Omzet","cells":cells}]});
    let same = fixture.body(block(json!([revenue("r1", 1, true), revenue("r2", 0, true)])));
    assert_eq!(same["rows"][0]["basis"], json!({"comparable":true,"values_differ":true}));
    assert!(same.to_string().find("winner").is_none());
    let periods = fixture.body(block(json!([revenue("r1", 0, true), revenue("r2", 1, true)])));
    assert_eq!(periods["rows"][0]["basis"], json!({"comparable":false,"issues":["currency","period"],"values_differ":true}));
    let unknown = fixture.body(block(json!([{"result_ref":"r1","pointer":"/data/years/1/revenue"},{"result_ref":"r2","pointer":"/data/years/0/revenue"}])));
    assert_eq!(unknown["rows"][0]["basis"]["issues"], json!(["qualifiers_unknown"]));
    let definition = fixture.body(block(json!([{"result_ref":"r1","pointer":"/data/years/1/revenue","currency":{"pointer":"/data/years/1/currency"}},{"result_ref":"r1","pointer":"/data/years/1/employees","currency":{"pointer":"/data/years/1/currency"}}])));
    assert_eq!(definition["rows"][0]["basis"]["issues"], json!(["definition"]));
    let text = fixture.body(block(json!([{"result_ref":"r1","pointer":"/data/name"},null])));
    assert_eq!(text["rows"][0]["basis"], json!({"comparable":true,"values_differ":false}), "an unknown cell is not a difference");
    assert_eq!(fixture.issue(block(json!([null]))), (BlockIssue::WidthMismatch, "rows/0/cells".into()));
    let single = json!({"id":"b1","kind":"comparison","entities":[{"id":"e1","label":"Mollie B.V."}],"rows":[{"id":"c1","label":"Omzet","cells":[null]}]});
    assert_eq!(fixture.issue(single).0, BlockIssue::Schema);
}

#[test]
fn kpi_changes_need_a_valid_comparison_basis() {
    let fixture = Fixture::new();
    let fact = |r: &str, i: usize| json!({"result_ref":r,"pointer":format!("/data/years/{i}/revenue"),"currency":{"pointer":format!("/data/years/{i}/currency")},"period":{"pointer":format!("/data/years/{i}/year")}});
    let card = |fact: Value, change: Value| json!({"id":"b1","kind":"kpi","cards":[{"id":"k1","label":"Omzet","fact":fact,"change":change}]});
    let change = fixture.body(card(fact("r1", 1), json!({"derivation_id":"d2","baseline":fact("r1", 0)})));
    assert_eq!(change["cards"][0]["change"]["basis"], "prior_period");
    assert_eq!(change["cards"][0]["change"]["result"], json!({"derivation_id":"d2","value":25,"state":"derived","rounded_to":1,"unit":{"value":"percent","basis":"operation"}}));
    assert_eq!(fixture.body(card(fact("r1", 1), json!({"derivation_id":"d1","baseline":fact("r1", 0)})))["cards"][0]["change"]["result"]["value"], 300000);
    let entity = fixture.body(card(fact("r2", 0), json!({"derivation_id":"d3","baseline":fact("r1", 1)})));
    assert_eq!(entity["cards"][0]["change"]["basis"], "other_entity");
    let path = "cards/0/change".to_string();
    assert_eq!(fixture.issue(card(fact("r1", 0), json!({"derivation_id":"d2","baseline":fact("r1", 1)}))), (BlockIssue::ChangeWithoutBasis, path.clone()), "inputs must be the card and its baseline");
    assert_eq!(fixture.issue(card(fact("r1", 2), json!({"derivation_id":"d1","baseline":fact("r1", 0)}))), (BlockIssue::ChangeWithoutBasis, path.clone()), "a missing value has no change");
    let undated = json!({"result_ref":"r1","pointer":"/data/years/0/revenue","currency":{"pointer":"/data/years/0/currency"}});
    assert_eq!(fixture.issue(card(fact("r1", 1), json!({"derivation_id":"d1","baseline":undated}))), (BlockIssue::ChangeWithoutBasis, path.clone()));
    assert_eq!(fixture.issue(card(fact("r1", 1), json!({"derivation_id":"d4","baseline":fact("r1", 0)}))), (BlockIssue::DerivationFailed, "cards/0/change/derivation_id".into()));
    assert_eq!(fixture.issue(card(fact("r1", 1), json!({"derivation_id":"d9","baseline":fact("r1", 0)}))).0, BlockIssue::UnknownDerivation);
    let missing = fixture.body(json!({"id":"b1","kind":"kpi","cards":[{"id":"k1","label":"Omzet","fact":{"result_ref":"r1","pointer":"/data/years/2/revenue"}},{"id":"k2","label":"Nul","fact":{"result_ref":"r1","pointer":"/data/items/1/amount"}}]}));
    assert_eq!((missing["cards"][0]["fact"]["state"].clone(), missing["cards"][1]["fact"]["state"].clone()), (json!("missing"), json!("zero")));
    let estimated = fixture.body(json!({"id":"b1","kind":"kpi","cards":[{"id":"k1","label":"Omzet","fact":{"result_ref":"r1","pointer":"/data/years/1/revenue","flags":[{"kind":"estimated","result_ref":"r1","pointer":"/data/years/1/estimated"}]}}]}));
    assert_eq!(estimated["cards"][0]["fact"]["flags"][0]["value"], "geschat");
}

#[test]
fn line_chart_orders_periods_marks_gaps_and_never_interpolates() {
    let fixture = Fixture::new();
    let point = |i: usize| json!({"period":{"result_ref":"r1","pointer":format!("/data/years/{i}/year")},"fact":{"result_ref":"r1","pointer":format!("/data/years/{i}/revenue"),"currency":{"pointer":format!("/data/years/{i}/currency")}}});
    let chart = |series: Value| json!({"id":"b1","kind":"line_chart","value_label":"Omzet","series":series});
    let body = fixture.body(chart(json!([{"id":"s1","label":"Mollie B.V.","points":[point(2), point(1), point(0)]}])));
    let series = &body["series"][0]["points"];
    let keys: Vec<_> = series.as_array().unwrap().iter().map(|p| p["period"]["key"].clone()).collect();
    assert_eq!(keys, [json!("2023"), json!("2024"), json!("2025")]);
    assert_eq!((series[2]["gap"].clone(), series[2]["fact"]["value"].clone()), (json!("null_in_source"), Value::Null));
    assert_eq!((body["granularity"].clone(), body["currency"].clone()), (json!("year"), json!({"value":"EUR","basis":"evidence"})));
    let collection = json!({"id":"s2","label":"Adyen N.V.","collection":{"result_ref":"r2","rows_pointer":"/data/years","row_indices":[0]},"period_pointer":"/year","value_pointer":"/revenue"});
    assert_eq!(fixture.issue(chart(json!([{"id":"s1","label":"Mollie B.V.","points":[point(0), point(2)]}]))), (BlockIssue::Schema, "series/0/points".into()), "at least two values per series");
    assert_eq!(fixture.issue(chart(json!([{"id":"s1","label":"Mollie B.V.","points":[point(0), point(0)]}]))).0, BlockIssue::DuplicatePeriod);
    assert_eq!(fixture.issue(chart(json!([{"id":"s1","label":"Mollie B.V.","points":[point(0), point(1)]}, collection]))).0, BlockIssue::Schema);
    let text_period = json!({"period":{"result_ref":"r1","pointer":"/data/items/2/date"},"fact":{"result_ref":"r1","pointer":"/data/items/2/amount"}});
    assert_eq!(fixture.issue(chart(json!([{"id":"s1","label":"Mollie B.V.","points":[point(0), text_period]}]))).0, BlockIssue::UnparsedPeriod);
    let day = json!({"period":{"result_ref":"r1","pointer":"/data/items/0/date"},"fact":{"result_ref":"r1","pointer":"/data/items/0/amount"}});
    assert_eq!(fixture.issue(chart(json!([{"id":"s1","label":"Mollie B.V.","points":[point(0), day]}]))).0, BlockIssue::PeriodMismatch);
    let names = json!({"period":{"result_ref":"r1","pointer":"/data/years/1/year"},"fact":{"result_ref":"r1","pointer":"/data/name"}});
    assert_eq!(fixture.issue(chart(json!([{"id":"s1","label":"Mollie B.V.","points":[point(0), names]}]))).0, BlockIssue::IncompatibleSeries);
    let usd = json!({"id":"s2","label":"Adyen N.V.","collection":{"result_ref":"r2","rows_pointer":"/data/years","row_indices":[1, 0]},"period_pointer":"/year","value_pointer":"/revenue","currency":{"pointer":"/currency"}});
    assert_eq!(fixture.issue(chart(json!([{"id":"s1","label":"Mollie B.V.","points":[point(0), point(1)]}, usd]))).0, BlockIssue::IncompatibleSeries);
    let derived = json!({"period":{"derivation_id":"d3"},"fact":{"derivation_id":"d3"}});
    assert_eq!(fixture.issue(chart(json!([{"id":"s1","label":"Mollie B.V.","points":[point(0), derived]}]))), (BlockIssue::UnparsedPeriod, "series/0/points/1/period".into()));
}

#[test]
fn bar_chart_scale_covers_every_bar_and_ranking_stays_within_coverage() {
    let fixture = Fixture::new();
    let bars = |rows: Value, ranking: Value| json!({"id":"b1","kind":"bar_chart","value_label":"Bedrag","ranking":ranking,
        "collection":{"result_ref":"r1","rows_pointer":"/data/items","row_indices":rows,"total_pointer":"/data/total"},"label_pointer":"/name","value_pointer":"/amount"});
    let body = fixture.body(bars(json!([0, 1, 2, 3, 4]), json!({"direction":"desc","scope":"total_available"})));
    assert_eq!(body["scale"], json!({"min":-50.0,"max":120.0}), "zero and negative values are on one fixed scale");
    let ranked: Vec<_> = body["bars"].as_array().unwrap().iter().map(|b| (b["id"].as_str().unwrap().to_owned(), b["rank"].as_u64())).collect();
    assert_eq!(ranked, [("row-2".into(), Some(1)), ("row-4".into(), Some(1)), ("row-1".into(), Some(3)), ("row-0".into(), Some(4)), ("row-3".into(), None)], "ties share a rank; unknown is not ranked");
    assert_eq!(body["coverage"]["scope"], "complete");
    let (issue, path) = fixture.issue(bars(json!([0, 2]), json!({"direction":"desc","scope":"fetched"})));
    assert_eq!((issue, path.as_str()), (BlockIssue::RankingScopeExceedsCoverage, "ranking/scope"));
    let selection = fixture.body(bars(json!([0, 2]), json!({"direction":"asc","scope":"selection"})));
    assert_eq!(selection["scale"], json!({"min":-50.0,"max":120.0}));
    let explicit = |bars: Value, ranking: Value| json!({"id":"b1","kind":"bar_chart","value_label":"Omzet","bars":bars,"ranking":ranking});
    let revenue = |id: &str, r: &str, i: usize| json!({"id":id,"label_fact":{"result_ref":r,"pointer":"/data/name"},"fact":{"result_ref":r,"pointer":format!("/data/years/{i}/revenue"),"currency":{"pointer":format!("/data/years/{i}/currency")}}});
    let body = fixture.body(explicit(json!([revenue("a", "r1", 1), revenue("b", "r2", 0)]), json!({"scope":"selection"})));
    assert_eq!((body["scale"].clone(), body["currency"]["value"].clone()), (json!({"min":0.0,"max":1900000.0}), json!("EUR")));
    assert_eq!(fixture.issue(explicit(json!([revenue("a", "r1", 1), revenue("b", "r2", 1)]), json!({"scope":"selection"}))).0, BlockIssue::CurrencyMismatch);
    assert_eq!(fixture.issue(explicit(json!([revenue("a", "r1", 1)]), json!({"scope":"total_available"}))).0, BlockIssue::RankingScopeExceedsCoverage);
    assert_eq!(fixture.issue(explicit(json!([{"id":"a","fact":{"result_ref":"r1","pointer":"/data/kvk"}}]), Value::Null)), (BlockIssue::Schema, "bars/0/label".into()));
}

#[test]
fn map_copies_source_coordinates_or_keeps_the_feature_in_the_list() {
    let fixture = Fixture::new();
    let map = |extra: Value| {
        let mut block = json!({"id":"b1","kind":"map","collection":{"result_ref":"r1","rows_pointer":"/data/items","row_indices":[0, 1, 3]},"label_pointer":"/name","geometry":{"pointer":"/point","format":"wkt_point"}});
        block.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        block
    };
    let body = fixture.body(map(json!({"asset":{"result_ref":"r1","asset_index":1}})));
    assert_eq!(body["crs"], "EPSG:4326");
    assert_eq!(body["features"][0]["geometry"]["geometry"], json!({"type":"Point","coordinates":[4.89,52.37]}));
    assert!(body["features"][1].get("geometry").is_none() && body["features"][2].get("geometry").is_none(), "no location is never geocoded");
    assert_eq!(fixture.issue(map(json!({"asset":{"result_ref":"r1","asset_index":0}}))), (BlockIssue::UnknownReference, "asset/asset_index".into()));
    let explicit = json!({"id":"b1","kind":"map","features":[{"id":"hq","label":"Hoofdkantoor","geometry":{"result_ref":"r1","pointer":"/data/items/0/point","format":"wkt_point"},"facts":[{"label":"Naam","fact":{"result_ref":"r1","pointer":"/data/items/0/name"}}]}]});
    assert_eq!(fixture.body(explicit)["features"][0]["geometry"]["crs"], "EPSG:4326");
}

#[test]
fn timeline_places_only_stated_dates_and_links_only_safe_urls() {
    let fixture = Fixture::new();
    let timeline = |rows: Value| json!({"id":"b1","kind":"timeline","collection":{"result_ref":"r1","rows_pointer":"/data/items","row_indices":rows},"label_pointer":"/name","date_pointer":"/date","date_kind":"published"});
    let body = fixture.body(timeline(json!([4, 2, 0, 3])));
    let order: Vec<_> = body["events"].as_array().unwrap().iter().map(|e| (e["id"].as_str().unwrap().to_owned(), e["when"]["precision"].as_str().map(str::to_owned))).collect();
    assert_eq!(order, [("row-3".into(), Some("date_time".into())), ("row-0".into(), Some("day".into())), ("row-4".into(), None), ("row-2".into(), Some("unknown".into()))]);
    assert_eq!(body["events"][0]["when"]["utc_offset"], "+01:00");
    assert!(body["events"][2].get("when").is_none(), "a null date is unknown, never estimated");
    assert!(body["events"][3]["when"].get("sort_key").is_none(), "an unparsed date is never placed on the axis");
    let with_link = |row: usize| json!({"id":"b1","kind":"timeline","collection":{"result_ref":"r1","rows_pointer":"/data/items","row_indices":[row]},"date_pointer":"/date","link_pointer":"/url","date_kind":"published"});
    assert!(fixture.one(with_link(0)).is_ok());
    assert_eq!(fixture.issue(with_link(1)).0, BlockIssue::Schema, "plain http is not a safe link");
    assert_eq!(fixture.issue(with_link(2)).0, BlockIssue::Schema, "credentials in a link are refused");
    let undated = json!({"id":"b1","kind":"timeline","events":[{"id":"e1","label":"Oprichting","date_kind":"registered"}]});
    assert!(fixture.body(undated)["events"][0].get("when").is_none());
    let kindless = json!({"id":"b1","kind":"timeline","collection":{"result_ref":"r1","rows_pointer":"/data/items","row_indices":[0]},"date_pointer":"/date"});
    assert_eq!(fixture.issue(kindless), (BlockIssue::Schema, "date_kind".into()));
}

#[test]
fn profile_checks_need_evidence_and_a_complete_source_for_no_match() {
    let fixture = Fixture::new();
    let profile = |checks: Value| json!({"id":"b1","kind":"profile","subject_label":"Mollie B.V.","name":{"result_ref":"r1","pointer":"/data/name"},
        "identifiers":[{"label":"KVK","fact":{"result_ref":"r1","pointer":"/data/kvk"}}],"status":{"label":"Status","fact":{"result_ref":"r1","pointer":"/data/status"}},
        "freshness":[{"result_ref":"r1","as_of":{"result_ref":"r1","pointer":"/data/as_of"}}],"checks":checks});
    let checks = json!([
        {"id":"active","label":"Ingeschreven","outcome":"confirmed","evidence":[{"result_ref":"r1","pointer":"/data/status"}]},
        {"id":"claimed","label":"Vergunning","outcome":"confirmed","evidence":[]},
        {"id":"sanctions","label":"Sancties","outcome":"no_match_in_scope","step_key":"svc:e2"},
        {"id":"people","label":"Personen","outcome":"no_match_in_scope","step_key":"people:e3"},
        {"id":"pending","label":"Jaarrekening","outcome":"no_match_in_scope","step_key":"accounts:e1"}
    ]);
    let body = fixture.body(profile(checks));
    let outcomes: Vec<_> = body["checks"].as_array().unwrap().iter().map(|c| c["outcome"].as_str().unwrap().to_owned()).collect();
    assert_eq!(outcomes, ["confirmed", "unknown", "no_match_in_scope", "unknown", "unknown"], "a finished call alone is never a pass");
    assert_eq!(body["freshness"], json!([
        {"result_ref":"r1","retrieved_at":"2026-09-24T19:02:11Z","as_of":{"result_ref":"r1","pointer":"/data/as_of","value":"2026-09-01","field":"as_of"}},
        {"result_ref":"r2","retrieved_at":"2026-09-24T19:02:14Z"},{"result_ref":"r3"}
    ]));
    assert_eq!(fixture.issue(profile(json!([{"id":"x","label":"X","outcome":"confirmed","step_key":"nope"}]))).0, BlockIssue::UnknownReference);
}

#[test]
fn relationships_need_one_stated_record_and_never_rest_on_names() {
    let fixture = Fixture::new();
    let graph = |nodes: Value, edge: Value| json!({"id":"b1","kind":"relationship","nodes":nodes,"edges":[edge]});
    let company = json!({"id":"mollie","label":"Mollie B.V.","node_kind":"organisation","name":{"result_ref":"r1","pointer":"/data/name"},"identifiers":[{"label":"KVK","fact":{"result_ref":"r1","pointer":"/data/kvk"}}]});
    let officer = json!({"id":"jansen","label":"J. Jansen","node_kind":"person","name":{"result_ref":"r1","pointer":"/data/officers/0/name"}});
    let edge = |evidence: Value| json!({"id":"board","from":"mollie","to":"jansen","relation":"officer","label":"Bestuurder","directed":true,"evidence":evidence,"valid_from":{"result_ref":"r1","pointer":"/data/officers/0/since"}});
    let stated = json!([{"result_ref":"r1","pointer":"/data/officers/0/name"},{"result_ref":"r1","pointer":"/data/officers/0/role"}]);
    let body = fixture.body(graph(json!([company, officer]), edge(stated.clone())));
    assert_eq!(body["edges"][0]["identity_basis"], "same_record");
    let split = json!([{"result_ref":"r1","pointer":"/data/officers/0/name"},{"result_ref":"r1","pointer":"/data/officers/1/role"}]);
    assert_eq!(fixture.issue(graph(json!([company, officer]), edge(split))).0, BlockIssue::MissingRelationEvidence);
    assert_eq!(fixture.issue(graph(json!([company, officer]), edge(json!([])))).0, BlockIssue::MissingRelationEvidence);
    let namesake = json!({"id":"jansen","label":"J. Jansen","node_kind":"person","name":{"result_ref":"r3","pointer":"/data/name"}});
    assert_eq!(fixture.issue(graph(json!([company, namesake]), edge(stated.clone()))).0, BlockIssue::NameOnlyRelation, "an equal name is not a proven relation");
    let parent = json!({"id":"adyen","label":"Adyen N.V.","node_kind":"organisation","name":{"result_ref":"r2","pointer":"/data/name"}});
    let owned = json!({"id":"link","from":"adyen","to":"mollie","relation":"parent","label":"Moeder","directed":true,"evidence":[{"result_ref":"r2","pointer":"/data/parent_kvk"}]});
    assert_eq!(fixture.body(graph(json!([company, parent]), owned))["edges"][0]["identity_basis"], "identifier_match");
    let mut unparsed = edge(stated);
    unparsed["valid_from"] = json!({"result_ref":"r1","pointer":"/data/officers/1/since"});
    assert_eq!(fixture.issue(graph(json!([company, officer]), unparsed)), (BlockIssue::UnparsedPeriod, "edges/0/valid_from".into()));
    let self_loop = json!({"id":"x","from":"mollie","to":"mollie","relation":"other","label":"Zelf","directed":false,"evidence":[{"result_ref":"r1","pointer":"/data/name"}]});
    assert_eq!(fixture.issue(graph(json!([company]), self_loop)), (BlockIssue::UnknownReference, "edges/0/to".into()));
}

fn legacy(presentation: Value) -> Vec<AnswerBlock> {
    let sources = vec![json!({"result_ref":"saved","document":{"source":{"name":"Registry"},"data":{"items":[{"name":"First","amount":12},{"name":"Second","amount":0},{"name":"Third","amount":null}]}}})];
    let validated = validate(&presentation, &sources).unwrap();
    blocks_from_presentation(&validated, &["o1".into(), "o2".into()], 2)
}

#[test]
fn every_version_one_kind_maps_to_blocks_deterministically() {
    let text = legacy(json!({"kind":"text"}));
    assert_eq!(text, [AnswerBlock { produced_by: BlockProducer::LegacyPresentation, ..AnswerBlock::narrative("b1", vec!["o1".into(), "o2".into()]) }]);
    assert_eq!(legacy_kind(&text), "text");
    assert_eq!(blocks_from_presentation(&Value::Null, &["o1".into()], 0).len(), 1);
    assert_eq!(blocks_from_presentation(&Value::Null, &[], 0), []);

    let document = legacy(json!({"kind":"document"}));
    assert_eq!(document[1].body, BlockBody::Summary(SummaryBlock { conclusion: None, finding_ids: vec!["o1".into(), "o2".into()], open_point_ids: vec!["l1".into(), "l2".into()], offer_report: true }));
    assert_eq!(legacy_kind(&document), "document");

    let table = legacy(json!({"kind":"table","title":"Overview 2024","result_ref":"saved","rows_pointer":"/data/items","row_indices":[1,2],"columns":[{"label":"Name","pointer":"/name"},{"label":"Amount","pointer":"/amount"}]}));
    let body = serde_json::to_value(&table[1]).unwrap();
    assert_eq!((body["kind"].clone(), body["title"].clone(), body["produced_by"].clone()), (json!("data_table"), json!("Overview 2024"), json!("legacy_presentation")));
    assert_eq!(body["coverage"], json!({"selected":2,"fetched":3,"paginated":false,"scope":"selection"}));
    assert_eq!(body["columns"][1], json!({"id":"c2","label":"Amount","role":"number"}));
    assert_eq!(body["rows"][0]["cells"][1], json!({"result_ref":"saved","pointer":"/data/items/1/amount","value":0,"state":"zero","field":"amount","source":"Registry"}));
    assert!(body["rows"][1]["cells"][1].is_null(), "unknown stays unknown");
    assert_eq!(legacy_kind(&table), "table");

    let comparison = legacy(json!({"kind":"comparison","title":"Comparison","columns":[{"label":"Feature"},{"result_ref":"saved","pointer":"/data/items/0/name"},{"result_ref":"saved","pointer":"/data/items/1/name"}],
        "rows":[[{"label":"Amount"},{"result_ref":"saved","pointer":"/data/items/0/amount"},{"result_ref":"saved","pointer":"/data/items/1/amount"}]]}));
    let body = serde_json::to_value(&comparison[1]).unwrap();
    assert_eq!(body["entities"][0]["label"], "First");
    assert_eq!(body["entities"][1]["name"]["pointer"], "/data/items/1/name");
    assert_eq!((body["rows"][0]["label"].clone(), body["rows"][0]["basis"].clone()), (json!("Amount"), json!({"comparable":false,"issues":["qualifiers_unknown"],"values_differ":true})));
    assert_eq!(legacy_kind(&comparison), "comparison");

    let chart = legacy(json!({"kind":"chart","title":"Amounts","columns":[{"label":"Name"},{"label":"Amount"}],
        "rows":[[{"result_ref":"saved","pointer":"/data/items/0/name"},{"result_ref":"saved","pointer":"/data/items/0/amount"}],[{"result_ref":"saved","pointer":"/data/items/1/name"},{"result_ref":"saved","pointer":"/data/items/1/amount"}]]}));
    let body = serde_json::to_value(&chart[1]).unwrap();
    assert_eq!((body["kind"].clone(), body["value_label"].clone(), body["scale"].clone()), (json!("bar_chart"), json!("Amount"), json!({"min":0.0,"max":12.0})));
    assert_eq!(body["bars"][0]["label_fact"]["value"], "First");
    assert_eq!(legacy_kind(&chart), "chart");
}
