//! Offline validation of source-bundle mappings with the actual runtime dialect.
//! Samples prove mechanics only; this tool makes no HTTP/model/database calls.
use catalog_core::facts::contract::{ExecutionContract, FactType, Vocabulary};
use serde_json::Value;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let file = std::env::args().nth(1).ok_or("Expected source bundle JSON path")?;
    let bundle: Value = serde_json::from_slice(&std::fs::read(file)?)?;
    let mut vocab = Vocabulary::new();
    for value in bundle["fact_types"].as_array().ok_or("fact_types must be an array")? {
        let fact: FactType = serde_json::from_value(value.clone())?;
        vocab.insert(fact.slug.clone(), fact);
    }
    // Optional read-only export of the current production vocabulary, supplied
    // when a bundle reuses definitions without repeating them in fact_types.
    if let Some(file) = std::env::args().nth(2) {
        let facts: Vec<FactType> = serde_json::from_slice(&std::fs::read(file)?)?;
        for fact in facts {
            if let Some(old) = vocab.get(&fact.slug) {
                if old.value_schema != fact.value_schema { return Err("Conflicting vocabulary schema".into()); }
            }
            vocab.insert(fact.slug.clone(), fact);
        }
    }
    let contracts = bundle["contracts"].as_array().ok_or("contracts must be an array")?;
    let mut samples = 0;
    for value in contracts {
        let contract: ExecutionContract = serde_json::from_value(value.clone())?;
        contract.check(&vocab)?;
        if let Some(sample) = value.get("sample_payload").filter(|v| !v.is_null()) {
            let extraction = contract.extract(sample, &vocab)?;
            if !extraction.rejected.is_empty() { return Err(format!("Rejected sample facts: {:?}", extraction.rejected).into()); }
            if extraction.subjects.is_empty() { return Err("Sample extracted zero subjects; provide a representative nonempty sample".into()); }
            samples += 1;
        }
    }
    println!("PASS: {} runtime contracts, {} extraction samples. No live API or production execution tested.", contracts.len(), samples);
    Ok(())
}
