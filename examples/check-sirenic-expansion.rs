//! Validate expanded contracts and country-safe dependencies without purchasing.
use apiosk_gateway_v2::{
    catalog::{Catalog, Snapshot},
    parser::{Goal, Intent, Parser, Subject},
    resolver::{resolve, Resolution},
    state::{Fact, Task},
};
use serde_json::json;
use std::collections::BTreeMap;
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let p = std::env::args()
        .nth(1)
        .unwrap_or("tests/fixtures/sirenic/expanded-catalog.json".into());
    let mut raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&p)?)?;
    if std::env::args().any(|a| a == "--reviewed") {
        let adjustments: serde_json::Value = serde_json::from_slice(&std::fs::read(
            "tests/fixtures/sirenic/verified-contract-adjustments.json",
        )?)?;
        for adjustment in adjustments["services"].as_array().unwrap() {
            let service = raw["services"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|s| s["id"] == adjustment["id"])
                .unwrap();
            service["contract"] = adjustment["contract"].clone();
        }
    }
    let snap: Snapshot = serde_json::from_value(raw.clone())?;
    let expected = snap.services.len();
    let c = Catalog::build(snap);
    anyhow::ensure!(
        c.services.len() == expected,
        "Rejected contracts: {:?}",
        c.rejected
    );
    let search = c
        .services
        .iter()
        .find(|s| s.path == "/v1/eu/recherche")
        .unwrap();
    let ex=search.contract.extract(&json!({"resultats":[{"pays":"GB","id_national":"00445790","denomination":"TESCO PLC"},{"pays":"NO","id_national":"923609016","denomination":"EQUINOR ASA"}]}),&c.vocabulary)?;
    anyhow::ensure!(ex.rejected.is_empty());
    anyhow::ensure!(ex.subjects[0]
        .fact("company.registration.gb.companies_house")
        .is_some());
    anyhow::ensure!(ex.subjects[0]
        .fact("company.registration.no.national_id")
        .is_none());
    anyhow::ensure!(ex.subjects[1]
        .fact("company.registration.no.national_id")
        .is_some());
    let entity = uuid::Uuid::new_v4();
    let mut task = Task::default();
    task.facts.push(Fact {
        entity_ref: entity,
        semantic_type: "company.name".into(),
        value: json!("DANONE"),
        provenance: json!({"kind":"user_input"}),
        observed_at: chrono::Utc::now(),
        expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        verification: "user_asserted".into(),
    });
    for suffix in ["alertes", "contentieux", "etablissements"] {
        let service = c
            .services
            .iter()
            .find(|s| s.path == format!("/v1/entreprise/{{siren}}/{suffix}"))
            .unwrap();
        let intent = Intent {
            subjects: vec![Subject {
                entity_ref: Some(entity),
                label: "DANONE".into(),
                values: BTreeMap::new(),
            }],
            goals: vec![Goal {
                subject: 0,
                capability: service.contract.capability_slug.clone(),
                constraints: BTreeMap::new(),
            }],
            answer_operations: vec![],
            unresolved_references: vec![],
        };
        let Resolution::Plan(plan) = resolve(&c, &task, &intent) else {
            anyhow::bail!("No name-to-{suffix} plan")
        };
        anyhow::ensure!(plan.steps.len() == 2 && plan.steps[0].service.path == "/v1/recherche");
    }
    if std::env::args().any(|a| a == "--model") {
        let parser = Parser::new(
            reqwest::Client::new(),
            std::env::var("INTENT_API_URL")?,
            std::env::var("INTENT_API_KEY")?,
            std::env::var("INTENT_MODEL")?,
        );
        let mut examples: serde_json::Value = serde_json::from_slice(&std::fs::read(
            "../app/src/lib/domain/sirenic-examples.json",
        )?)?;
        examples.as_array_mut().unwrap().push(json!({"question":"Find DANONE in the French company register and retrieve its legal alerts and court cases.","expected_paths":["/v1/recherche","/v1/entreprise/{siren}/alertes","/v1/entreprise/{siren}/contentieux"],"automatic_source_selection":true}));
        let mut evidence = vec![];
        for example in examples.as_array().unwrap() {
            let question = example["question"].as_str().unwrap();
            let mut state = Task::default();
            let mut intent = parser
                .parse(uuid::Uuid::new_v4(), question, &state, &c)
                .await?;
            for subject in &mut intent.subjects {
                let id = uuid::Uuid::new_v4();
                subject.entity_ref = Some(id);
                for (kind, value) in &subject.values {
                    state.facts.push(Fact {
                        entity_ref: id,
                        semantic_type: kind.clone(),
                        value: value.clone(),
                        provenance: json!({"kind":"user_input"}),
                        observed_at: chrono::Utc::now(),
                        expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
                        verification: "user_asserted".into(),
                    });
                }
            }
            let resolution = resolve(&c, &state, &intent);
            let Resolution::Plan(plan) = resolution else {
                anyhow::bail!(
                    "Question has no executable plan: {question}: {intent:?}, {resolution:?}"
                )
            };
            let paths: Vec<_> = plan.steps.iter().map(|s| s.service.path.as_str()).collect();
            for expected in example["expected_paths"].as_array().unwrap() {
                anyhow::ensure!(
                    paths.contains(&expected.as_str().unwrap()),
                    "Missing {} for {}: {:?}",
                    expected,
                    question,
                    paths
                );
            }
            anyhow::ensure!(plan.steps.iter().all(|s| s.service.api_slug == "sirenic"));
            evidence.push(json!({"question":question,"intent":intent,"paths":paths,"cap_atomic":plan.cap_atomic}));
            println!(
                "Planner check verified: {} steps, {} micro-USD approval ceiling",
                plan.steps.len(),
                plan.cap_atomic
            );
        }
        std::fs::write(
            "output/sirenic-20260913/ask-plan-evidence.json",
            serde_json::to_vec_pretty(&evidence)?,
        )?;
    }
    if std::env::args().any(|a| a == "--paid") {
        let mut checked = 0;
        let mut reports = vec![];
        let mut files = std::fs::read_dir("output/sirenic-20260913/payments")?
            .map(|e| e.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()?;
        files.extend(
            std::fs::read_dir("output/sirenic-20260912")?
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with("canonical-paid-")
                })
                .map(|e| e.path()),
        );
        for file in files {
            if file.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let d: serde_json::Value = serde_json::from_slice(&std::fs::read(file)?)?;
            if d["body"]["execution"]["paid"] != true {
                continue;
            }
            let s = c
                .services
                .iter()
                .find(|s| {
                    if let Some(id) = d["service_id"].as_str() {
                        s.id.to_string() == id
                    } else {
                        Some(s.path.as_str()) == d["path"].as_str()
                    }
                })
                .ok_or_else(|| anyhow::anyhow!("Paid service absent"))?;
            let ex = s
                .contract
                .extract(&d["body"]["execution"]["body"], &c.vocabulary)?;
            let missing: Vec<_> = s
                .contract
                .output_map
                .produces
                .keys()
                .filter(|kind| {
                    !s.optional_outputs.contains(kind)
                        && !ex
                            .subjects
                            .iter()
                            .any(|subject| subject.fact(kind).is_some())
                })
                .collect();
            let valid = ex.rejected.is_empty() && !ex.subjects.is_empty() && missing.is_empty();
            reports.push(json!({"service_id":s.id,"path":s.path,"valid":valid,"missing":missing,"rejected":format!("{:?}",ex.rejected),"tx_hash":d["body"]["execution"]["tx_hash"]}));
            if valid {
                checked += 1;
            }
        }
        std::fs::write(
            "output/sirenic-20260913/paid-contract-evidence.json",
            serde_json::to_vec_pretty(&reports)?,
        )?;
        println!(
            "{checked} of {} paid response contracts verified",
            reports.len()
        );
        anyhow::ensure!(
            checked == reports.len(),
            "Some paid responses failed extraction; inspect paid-contract-evidence.json"
        );
    }
    println!("{expected} contracts accepted; country IDs and name-to-record plans verified");
    Ok(())
}
