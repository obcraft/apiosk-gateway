//! Opt-in content audit of saved live upstream responses; never purchases source data.
use apiosk_gateway_v2::{
    catalog::{Catalog, Snapshot},
    parser::Parser,
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf};
use uuid::Uuid;

#[tokio::test]
#[ignore = "reads production contracts and calls the configured analysis model on saved public source data"]
async fn audit_saved_direct_source_answers() {
    dotenvy::dotenv().ok();
    let dir = PathBuf::from(
        std::env::var("DIRECT_AUDIT_DIR").expect("saved direct-source evidence directory"),
    );
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&std::env::var("DATABASE_URL").unwrap())
        .await
        .unwrap();
    let snapshot: Value = sqlx::query_scalar(include_str!("../src/catalog.sql"))
        .fetch_one(&pool)
        .await
        .unwrap();
    pool.close().await;
    let catalog = Catalog::build(serde_json::from_value::<Snapshot>(snapshot).unwrap());
    let parser = Parser::new(
        reqwest::Client::new(),
        std::env::var("INTENT_API_URL").unwrap(),
        std::env::var("INTENT_API_KEY").unwrap(),
        std::env::var("INTENT_MODEL").unwrap(),
    );
    let cases:Vec<(&str,&str,Vec<&str>)>=vec![
        ("01-financial-comparison","Vergelijk de beschikbare jaarrekeningen van HEMA B.V. en AFAS Software B.V. Vermeld de boekjaren en ontbrekende gegevens.",vec!["hema-accounts","afas-accounts"]),
        ("02-supplier-check","Vat het bedrijfsprofiel van Coolblue B.V. en de sanctiescreening samen. Geen matches is geen garantie dat het bedrijf veilig is.",vec!["coolblue-profile","coolblue-sanctions"]),
        ("03-corporate-family","Welke directe dochterbedrijven van ASML Holding N.V. staan in GLEIF? Benoem de afbakening van dit register.",vec!["asml-children"]),
        ("04-uk-company","Toon het bedrijfsprofiel, bestuurders en personen met significante zeggenschap van MONZO BANK LIMITED. Onderscheid actief van afgetreden.",vec!["monzo-profile","monzo-officers","monzo-persons-with-significant-control"]),
        ("05-tenders","Vat de gevonden TenderNed-opdrachten over laadpalen samen. Onderscheid oude, gegunde en nog open opdrachten; noem ontbrekende informatie en paginering.",vec!["tenders"]),
        ("06-bankruptcies","Vat de CBS-faillissementscijfers samen. Benoem de feitelijk teruggegeven periodes en sectoren met de officiële codetabellen. Verzin geen betekenis voor onverklaarde codes.",vec!["cbs-bankruptcies","cbs-measures","cbs-sectors","cbs-entity-types"]),
        ("07-gdp-comparison","Vergelijk het bbp van Nederland en Duitsland voor het geretourneerde jaar, met de teruggegeven eenheid en indicator.",vec!["gdp-NL","gdp-DE"]),
        ("08-public-datasets","Toon datasets over waterkwaliteit, uitgevers en bronlinks. Onderscheid publisher van de catalogusorganisatie; dit zijn metadata en geen meetwaarden.",vec!["waterquality"]),
        ("09-address-check","Geef voor exact Dam 1 Amsterdam het officiële adres, postcode, gemeente en buurt. Negeer andere adressen of alleen de straat.",vec!["dam-address"]),
        ("10-postcode","Toon de BAG-adressen op postcode 1012JS en geef aan hoeveel adressen er zijn gevonden en teruggegeven.",vec!["postcode-addresses"]),
    ];
    let mut report = vec![];
    let final_run=std::env::var("DIRECT_AUDIT_FINAL").as_deref()==Ok("1");
    let planned: Option<Value> = if std::env::var("DIRECT_AUDIT_PLAN_OPERATIONS").as_deref()==Ok("1") {
        Some(serde_json::from_slice(&std::fs::read(dir.join("question-planning.json")).unwrap()).unwrap())
    } else {None};
    let only = std::env::var("DIRECT_AUDIT_ONLY").ok();
    for (id, question, files) in cases {
        if only.as_ref().is_some_and(|wanted| wanted != id) {
            continue;
        }
        let mut sources = vec![];
        let mut failures = BTreeMap::new();
        let mut mappings = vec![];
        for file in files {
            let record: Value =
                serde_json::from_slice(&std::fs::read(dir.join(format!("{file}.json"))).unwrap())
                    .unwrap();
            if record["status"] != 200 {
                failures.insert(
                    file.to_owned(),
                    json!({"status":record["status"],"error":record["data"]}),
                );
                continue;
            }
            if file == "waterquality" {
                let preview = apiosk_gateway_v2::source_preview::project("data-overheid-nl", &record["data"])
                    .expect("The complete dataset listing must fit the readable preview");
                let original = record["data"]["result"]["results"].as_array().unwrap();
                let projected = preview["result"]["results"].as_array().unwrap();
                assert_eq!(projected.len(), original.len());
                for (source, row) in original.iter().zip(projected) {
                    assert_eq!(source["title"], row["title"]);
                    assert_eq!(source["publisher"], row["publisher"]);
                    assert_eq!(source["resources"].as_array().unwrap().len(), row["resources"].as_array().unwrap().len());
                }
            }
            for s in catalog
                .services
                .iter()
                .filter(|s| s.endpoint_id.to_string() == record["endpoint_id"].as_str().unwrap())
            {
                match s.contract.extract(&record["data"],&catalog.vocabulary) {
                    Ok(x)=>{let missing:Vec<_>=s.contract.output_map.produces.keys().filter(|k|!x.subjects.iter().any(|v|v.fact(k).is_some())).cloned().collect();mappings.push(json!({"response":file,"capability":s.contract.capability_slug,"subjects":x.subjects.len(),"rejected":format!("{:?}",x.rejected),"missing_outputs":missing}));},
                    Err(e)=>mappings.push(json!({"response":file,"capability":s.contract.capability_slug,"error":e.to_string()})),
                }
            }
            sources.push(json!({"result_ref":Uuid::new_v4(),"document":{"subject":{"label":file},"source":{"name":record["source"],"url":record["url"],"path":record["route"]},"data":record["data"]}}));
        }
        let start = std::time::Instant::now();
        let index=id[..2].parse::<usize>().unwrap()-1;
        let operations=planned.as_ref().map(|p|p[index]["intent"]["answer_operations"].clone()).unwrap_or(json!([question]));
        // Every question is answered from its own saved results, with or
        // without an explicitly requested comparison.
        let analysis = match parser.analyze(question,&operations,&sources,&failures).await {Ok(a)=>a,Err(e)=>json!({"status":"failed","error":e.to_string()})};
        let row = json!({"id":id,"question":question,"failures":failures,"mapping_audit":mappings,"analysis_ms":start.elapsed().as_millis(),"analysis":analysis});
        let prefix = if final_run {"final-"} else if planned.is_some() && only.is_some() {"actual-retry-"} else if planned.is_some() {"actual-"} else if only.is_some() { "retry-" } else { "" };
        std::fs::write(
            dir.join(format!("{prefix}{id}-analysis.json")),
            serde_json::to_vec_pretty(&row).unwrap(),
        )
        .unwrap();
        println!(
            "{}: analysis={} findings={} mappings={}",
            id,
            analysis["status"],
            analysis["observations"].as_array().map_or(0, Vec::len),
            serde_json::to_string(&mappings).unwrap()
        );
        report.push(row);
    }
    if only.is_none() {
        std::fs::write(
            dir.join(if final_run {"final-answer-audit.json"} else if planned.is_some() {"actual-answer-audit.json"} else {"answer-audit.json"}),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
    assert_eq!(
        report.len(),
        if only.is_some() { 1 } else { 10 },
        "Audit records all outcomes; model completion is not itself a content pass"
    );
    if final_run {
        for row in &report {
            if row["id"]=="01-financial-comparison" {
                assert_eq!(row["analysis"]["status"],"partial");
                assert_eq!(row["failures"].as_object().unwrap().len(),2);
                assert!(row["analysis"]["observations"].as_array().unwrap().is_empty());
                continue;
            }
            assert!(row["failures"].as_object().unwrap().is_empty(),"{row}");
            let mappings=row["mapping_audit"].as_array().unwrap();assert!(!mappings.is_empty());
            assert!(mappings.iter().all(|m|m.get("error").is_none()&&m["rejected"]=="[]"&&m["missing_outputs"].as_array().unwrap().is_empty()),"{row}");
            assert_eq!(row["analysis"]["status"],"completed","{row}");
        }
    }
}
