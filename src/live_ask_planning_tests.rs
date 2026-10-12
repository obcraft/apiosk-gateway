//! Opt-in production-model verification. Reads catalog metadata and builds
//! ephemeral quotes only: source execution and task persistence are disabled.
use serde_json::{json, Value};

pub(crate) fn record_validation_failure(raw:&str,error:&anyhow::Error) {
    use std::io::Write;
    let Ok(path)=std::env::var("ASK_PLANNING_DIAGNOSTICS") else {return};
    let mut row=json!({"reply":serde_json::from_str::<Value>(raw).ok(),"error":error.to_string()});
    if let Some(coverage)=error.downcast_ref::<crate::parser::source_grounding::CoverageError>() {
        row["missing_outputs"]=json!(coverage.outputs);row["missing_inputs"]=json!(coverage.inputs);
    }
    let mut file=std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(file,"{row}").unwrap();
}

async fn live_context() -> (crate::engine::Engine, std::sync::Arc<crate::catalog::Catalog>) {
    dotenvy::dotenv().ok();
    let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1)
        .connect(&std::env::var("DATABASE_URL").unwrap()).await.unwrap();
    let catalog = crate::catalog::CatalogCache::new(pool.clone()).get().await.unwrap();
    pool.close().await;
    let key = std::env::var("OPENROUTER_API_KEY").ok().filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::env::var("INTENT_API_KEY").unwrap());
    let http = reqwest::Client::new();
    let parser = crate::parser::Parser::new(http.clone(), "https://openrouter.ai/api/v1/chat/completions".into(),
        key.clone(), crate::parser::MODEL.into())
        .with_ask_v2(crate::ask_v2::Client::new(http.clone(), key, crate::ask_v2::DEFAULT_MODEL.into()));
    let unreachable = sqlx::postgres::PgPoolOptions::new().connect_lazy("postgresql://postgres@127.0.0.1:1/unused").unwrap();
    let store = crate::store::Store { pool: unreachable.clone() };
    (crate::engine::Engine {
        store: store.clone(), catalog: crate::catalog::CatalogCache::new(unreachable), parser,
        signer: crate::state::Signer::new(vec![8;32]).unwrap(), app_url: "http://localhost".into(),
        executor: crate::execution::Executor { store, http, supabase_url: "http://127.0.0.1:1".into(), enabled: false },
    }, catalog)
}

#[tokio::test]
#[ignore = "reads selected production catalog contracts into a local replay fixture; no source or model calls"]
async fn record_live_planning_catalog() {
    let (_,catalog)=live_context().await;
    let capabilities:Vec<String>=serde_json::from_str(&std::env::var("ASK_PLANNING_CATALOG_CAPABILITIES").expect("explicit capability list required")).unwrap();
    let services:Vec<_>=catalog.services.iter().filter(|service|capabilities.contains(&service.contract.capability_slug)).cloned().collect();
    assert!(capabilities.iter().all(|cap|services.iter().any(|service|&service.contract.capability_slug==cap)));
    let fields:std::collections::BTreeSet<_>=services.iter().flat_map(|service|service.contract.requires.iter().map(|required|&required.fact_type)
        .chain(service.contract.output_map.produces.keys())).collect();
    let snapshot=crate::catalog::Snapshot {vocabulary:catalog.vocabulary.iter().filter(|(key,_)|fields.contains(key)).map(|(_,fact)|fact.clone()).collect(),
        known_sources:catalog.known_sources.iter().filter(|source|services.iter().any(|service|service.api_slug==source.slug)).cloned().collect(),services};
    std::fs::write(std::env::var("ASK_PLANNING_CATALOG_FIXTURE").expect("local fixture path required"),serde_json::to_vec_pretty(&snapshot).unwrap()).unwrap();
}

#[tokio::test]
#[ignore = "uses actual interpretation model and read-only production catalog; no source purchases or task writes"]
async fn live_selected_sources_form_one_complete_quote() {
    use crate::ask_v2::source_selection::{Context, SelectedSource, Selection};
    let _ = tracing_subscriber::fmt().with_env_filter("warn,sqlx=error").try_init();
    let (engine,catalog)=live_context().await;
    let cases = [
        ("Show the registered company details for Mollie B.V.",vec!["company.registry-profile","company.crunchbase-search","get-realtime-glassdoor-data-company-search"],None),
        ("Search Crunchbase for Starbucks and show its company description and profile link.",vec!["company.crunchbase-search","get-realtime-glassdoor-data-company-search","get-yelp-business-data-business-search"],Some("Seattle")),
        ("Show the registered company details for Monzo in the United Kingdom.",vec!["company.registry-profile","company.crunchbase-search","get-realtime-glassdoor-data-company-search"],None),
    ];
    let mut reports=vec![];
    for (question,wanted,location) in cases {
        let started=std::time::Instant::now();
        let intake=engine.parser.ask_intake(question,&crate::state::Task::default(),&catalog,None).await.unwrap();
        let parsed=json!(intake.parsed_request);
        let options=crate::ask_v2::source_options::options(&intake.parsed_request,&intake.matches,&catalog);
        let sources:Vec<_>=wanted.iter().map(|fragment| {
            let option=options.iter().find(|option|option.source.capability.as_deref().is_some_and(|cap|cap.contains(fragment)))
                .unwrap_or_else(||panic!("No option for {fragment}: {question}"));
            let mut input=option.input_values.clone();
            if let Some(location)=location { for missing in &option.missing_inputs {
                if missing.ends_with(".location") {input.insert(missing.clone(),json!(location));}
            }}
            SelectedSource {endpoint_id:option.source.endpoint_id.as_ref().unwrap().parse().unwrap(),
                capability:option.source.capability.clone().unwrap(),subject_id:option.subject_id.clone(),input}
        }).collect();
        let selected=json!(sources);
        let context=Context {question:question.into(),original_intake:intake,search_id:"ephemeral-test".into(),discovery_id:None,
            options,selection:Selection {sources,..Default::default()}};
        let outcome=engine.preview_selected_sources(question,context,&catalog).await;
        let report=match outcome {
            Ok((status,task))=>json!({"question":question,"parsed":parsed,"selected":selected,"status":status,
                "proposal":task.proposal.as_ref().map(|proposal|json!({"total_atomic":proposal["cap_atomic"],"steps":proposal["steps"].as_array().unwrap().iter().map(|step|json!({"capability":step["service"]["contract"]["capability_slug"],"endpoint_id":step["service"]["endpoint_id"],"entity_ref":step["entity_ref"]})).collect::<Vec<_>>()})),
                "actions":task.actions,"intent":task.intent}),
            Err(error)=>json!({"question":question,"parsed":parsed,"selected":selected,"error":error.to_string()}),
        };
        println!("{}: {} ({}ms)",question,report["status"],started.elapsed().as_millis());
        reports.push(report);
        std::fs::write(std::env::var("ASK_SOURCE_SELECTION_EVIDENCE").expect("evidence file required"),serde_json::to_vec_pretty(&reports).unwrap()).unwrap();
    }
    assert!(reports.iter().all(|report|report["status"]=="ready"),"Inspect selected source evidence");
}

#[tokio::test]
#[ignore = "uses configured model credentials and a read-only production catalog; no source purchases"]
async fn live_questions_use_ai_and_reach_complete_quotes() {
    dotenvy::dotenv().ok();
    let _ = tracing_subscriber::fmt().with_env_filter("warn,sqlx=error").try_init();
    let questions: Vec<String> = serde_json::from_slice(&std::fs::read(
        std::env::var("ASK_PLANNING_QUESTIONS").expect("question JSON file required")
    ).unwrap()).unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1)
        .connect(&std::env::var("DATABASE_URL").unwrap()).await.unwrap();
    let catalog = crate::catalog::CatalogCache::new(pool.clone()).get().await.unwrap();
    pool.close().await;
    let key = std::env::var("OPENROUTER_API_KEY").ok().filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::env::var("INTENT_API_KEY").unwrap());
    let http = reqwest::Client::new();
    let parser = crate::parser::Parser::new(http.clone(), "https://openrouter.ai/api/v1/chat/completions".into(),
        key.clone(), std::env::var("ASK_PLANNING_TEST_MODEL").unwrap_or_else(|_| crate::parser::MODEL.into()))
        .with_ask_v2(crate::ask_v2::Client::new(http.clone(), key, crate::ask_v2::DEFAULT_MODEL.into()));
    let unreachable = sqlx::postgres::PgPoolOptions::new().connect_lazy("postgresql://postgres@127.0.0.1:1/unused").unwrap();
    let store = crate::store::Store { pool: unreachable.clone() };
    let engine = crate::engine::Engine {
        store: store.clone(), catalog: crate::catalog::CatalogCache::new(unreachable), parser,
        signer: crate::state::Signer::new(vec![8;32]).unwrap(), app_url: "http://localhost".into(),
        executor: crate::execution::Executor { store, http, supabase_url: "http://127.0.0.1:1".into(), enabled: false },
    };
    let mut reports = Vec::<Value>::new();
    let mut fixtures = Vec::<Value>::new();
    for question in questions {
        let started = std::time::Instant::now();
        let intake = engine.parser.ask_intake(&question, &crate::state::Task::default(), &catalog, None).await;
        let report = match intake {
            Ok(intake) => {
                let parsed = json!(intake.parsed_request);
                let matches = json!(intake.matches);
                let source_options = json!(crate::ask_v2::source_options::options(&intake.parsed_request, &intake.matches, &catalog));
                match engine.guest_plan(&question, intake, &catalog).await {
                    Ok(quote) => {
                        use base64::Engine;
                        // Inspect only our ephemeral locally signed quote's
                        // literal inputs; never persist its approval token.
                        let task = quote["preview_token"].as_str().and_then(|token| token.split_once('.'))
                            .and_then(|(payload,_)| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).ok())
                            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                            .map(|preview| preview["task"].clone()).unwrap_or(Value::Null);
                        if quote["status"] == "ready" && std::env::var("ASK_PLANNING_FIXTURES").is_ok() {
                            fixtures.push(planning_fixture(&question,&parsed,&quote,&task,&catalog));
                            std::fs::write(std::env::var("ASK_PLANNING_FIXTURES").unwrap(),serde_json::to_vec_pretty(&fixtures).unwrap()).unwrap();
                        }
                        json!({"question":question,"parsed":parsed,"matches":matches,"source_options":source_options,"status":quote["status"],
                            "sources":quote["sources"],"total_atomic":quote["total_atomic"],"message":quote["message"],
                            "intent":task["intent"],"facts":task["facts"],"output_grounding":task["output_grounding"],"coverage_notices":quote["coverage_notices"]})
                    },
                    Err(error) => json!({"question":question,"parsed":parsed,"matches":matches,"error":error.to_string()}),
                }
            },
            Err(error) => json!({"question":question,"error":error.to_string()}),
        };
        let mut report = report;
        report["elapsed_ms"] = json!(started.elapsed().as_millis());
        println!("{}: {} ({}ms)", question, report["status"], report["elapsed_ms"]);
        reports.push(report);
        std::fs::write(std::env::var("ASK_PLANNING_EVIDENCE").expect("evidence path required"),
            serde_json::to_vec_pretty(&reports).unwrap()).unwrap();
    }
    assert!(reports.iter().all(|row| row["status"] == "ready"), "Inspect saved evidence for incomplete plans");
}

// Opt-in fixture recording replays actual model interpretation and bindings,
// then runs discovery, resolution and coverage again; it never mocks a plan.
fn planning_fixture(question:&str, parsed:&Value, quote:&Value, task:&Value, catalog:&crate::catalog::Catalog)->Value {
    let mut output=parsed.clone(); output.as_object_mut().unwrap().remove("meta");
    for cap in output["capabilities"].as_array_mut().unwrap() {cap.as_object_mut().unwrap().remove("missing");}
    let original:crate::parser::Intent=serde_json::from_value(task["intent"].clone()).unwrap();
    let mut intent=task["intent"].clone();
    intent["subjects"]=json!(original.subjects.iter().map(|subject| {
        let parsed_subject=parsed["subjects"].as_array().unwrap().iter().find(|s|s["label"]==subject.label).unwrap();
        let reference=parsed_subject["id"].as_str().unwrap();
        let values=subject.values.iter().map(|(key,value)| {
            let reference=if value==&parsed_subject["label"] {Some(format!("{reference}.label"))} else {
                parsed_subject["identifiers"].as_object().into_iter().flatten().find(|(_,input)|*input==value).map(|(key,_)|format!("{reference}.identifiers.{key}"))
            };
            (key.clone(),reference.map(|reference|json!({"ref":reference})).unwrap_or_else(||json!({"literal":value})))
        }).collect::<serde_json::Map<_,_>>();
        json!({"subject_ref":reference,"values":values})
    }).collect::<Vec<_>>());
    let mut proposal=task["proposal"].clone();
    for step in proposal["steps"].as_array_mut().unwrap() {
        let service=catalog.services.iter().find(|service|step["service"]["id"]==service.id.to_string()).unwrap();
        step["service"]=json!(service);
    }
    let plan:crate::resolver::Plan=serde_json::from_value(proposal).unwrap();
    let task:crate::state::Task=serde_json::from_value(task.clone()).unwrap();
    let shown=crate::resolver::describe_steps(&plan,&[]);
    let steps=plan.steps.iter().enumerate().map(|(i,step)|json!({"source":step.service.api_slug,"capability":step.service.contract.capability_slug,
        "subject":original.subjects.iter().find(|subject|subject.entity_ref==Some(step.entity_ref)).unwrap().label,
        "method":step.service.method,"path":step.service.path,"inputs":step.service.inputs(&task,step.entity_ref,&catalog.vocabulary).ok(),
        "dependencies":shown[i]["dependencies"].as_array().unwrap().iter().map(|d|json!({"step":d["step"],"field":d["field"]})).collect::<Vec<_>>()})).collect::<Vec<_>>();
    json!({"id":crate::state::digest(&question)[..12],"question":question,"model_output":output,
        "planner_reply":{"message":quote["message"],"discover":true,"decline_reason":"none","intent":intent,
            "output_bindings":task.output_grounding.map(|grounding|grounding.bindings).unwrap_or_default()},
        "cap_atomic":plan.cap_atomic,"steps":steps})
}
