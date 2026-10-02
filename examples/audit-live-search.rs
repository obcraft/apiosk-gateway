//! Replay saved public audit questions through deterministic search only.
//! No model, network, credentials, task writes, provider calls or payments.
use apiosk_gateway_v2::{ask_v2::search::search, capability_intake::ParsedRequest, catalog::{Catalog, Snapshot}};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let snapshot: Snapshot = serde_json::from_slice(&std::fs::read(args.next().expect("catalog snapshot"))?)?;
    let dir = PathBuf::from(args.next().expect("sanitized diagnostic directory"));
    let output = PathBuf::from(args.next().expect("output JSON"));
    let catalog = Catalog::build(snapshot);
    let mut cases = BTreeMap::<u64, Value>::new();
    let mut files: Vec<String> = args.collect();
    if files.is_empty() {
        files = ["cases01-04-diagnostics.json", "cases05-09-diagnostics.json", "cases10-11-diagnostics.json", "case13-diagnostics.json", "cases12-16-diagnostics.json", "cases17-20-diagnostics.json"].into_iter().map(str::to_owned).collect();
    }
    for file in files {
        let batch: Value = serde_json::from_slice(&std::fs::read(dir.join(file))?)?;
        for row in batch["chats"].as_array().into_iter().flatten() {
            let case = row["case"].as_u64().expect("case number");
            let report = cases.entry(case).or_insert_with(|| json!({"case":case}));
            report["question"] = row["question"].clone();
            let mut raw = row["parsed"]["parsed_request"].clone();
            for cap in raw["capabilities"].as_array_mut().into_iter().flatten() { cap.as_object_mut().unwrap().remove("status"); }
            if let Ok(parsed) = serde_json::from_value::<ParsedRequest>(raw) {
                report["app"] = json!({"capabilities":parsed.capabilities,"matches":search(&parsed, &catalog)});
            }
        }
        for row in batch["tasks"].as_array().into_iter().flatten() {
            let case = row["case"].as_u64().expect("case number");
            if let Ok(parsed) = serde_json::from_value::<ParsedRequest>(row["intake"]["parsed_request"].clone()) {
                cases.entry(case).or_insert_with(|| json!({"case":case}))["complete_intake"] = json!({"capabilities":parsed.capabilities,"matches":search(&parsed, &catalog)});
            }
        }
    }
    let count = cases.len();
    std::fs::write(output, serde_json::to_vec_pretty(&json!({"boundary":"Saved live model parses replayed through local deterministic search. No execution or coverage guarantee.","catalog_version":catalog.version,"catalog_services":catalog.services.len(),"cases":cases.into_values().collect::<Vec<_>>()}))?)?;
    println!("Replayed {count} saved public audit cases through search without network");
    Ok(())
}
