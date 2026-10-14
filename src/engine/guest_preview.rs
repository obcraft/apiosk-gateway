//! Guest quotes stay in memory until an authenticated person approves them.
//! The signed snapshot fixes inputs, steps and the buyer's total; adopting it
//! uses the ordinary task, authorization, worker and payment ledger.
use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};

const MAX_BYTES: usize = 128 * 1024;
pub(crate) const MAX_TOKEN_BYTES: usize = (MAX_BYTES * 4).div_ceil(3) + 65;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Preview {
    pub id: Uuid,
    pub quote_ref: Uuid,
    pub total_atomic: i64,
    pub expires_at: chrono::DateTime<Utc>,
    task: Task,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Refusal {
    #[error("guest_preview_expired")]
    Expired,
    #[error("guest_preview_changed")]
    Changed,
    #[error("guest_preview_used")]
    Used,
}

impl Preview {
    fn from_task(task: &Task) -> anyhow::Result<Self> {
        let plan: Plan = serde_json::from_value(task.proposal.clone().ok_or_else(|| anyhow::anyhow!("no guest quote"))?)?;
        anyhow::ensure!(!plan.steps.is_empty() && plan.cap_atomic >= 0, "empty guest quote");
        let mut task = task.clone();
        // Keep internal endpoint configuration on the server. The token only
        // carries service identities and their exact version hashes.
        for (step, quoted) in task.proposal.as_mut().unwrap()["steps"].as_array_mut().unwrap().iter_mut().zip(&plan.steps) {
            step["service"] = json!({"id":quoted.service.id,"version_hash":quoted.service.version_hash()});
        }
        Ok(Self { id:Uuid::new_v4(), quote_ref:plan.quote_ref, total_atomic:plan.cap_atomic, expires_at:plan.expires_at, task })
    }

    fn seal(&self, signer: &Signer) -> anyhow::Result<String> {
        let payload = serde_json::to_vec(self)?;
        anyhow::ensure!(payload.len() <= MAX_BYTES, "guest preview too large");
        Ok(format!("{}.{}", URL_SAFE_NO_PAD.encode(&payload), signer.guest_signature(&payload)))
    }

    /// Verify integrity before any account or task lookup. Expiry is checked
    /// later so an already-approved request remains recoverable after login.
    pub(crate) fn open(signer: &Signer, token: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(token.len() <= MAX_TOKEN_BYTES, "guest preview too large");
        let (payload, signature) = token.split_once('.').ok_or_else(|| anyhow::anyhow!("invalid guest preview"))?;
        anyhow::ensure!(signature.len() == 64, "invalid guest signature");
        let payload = URL_SAFE_NO_PAD.decode(payload)?;
        anyhow::ensure!(payload.len() <= MAX_BYTES, "guest preview too large");
        signer.verify_guest(&payload, signature)?;
        let preview: Self = serde_json::from_slice(&payload)?;
        anyhow::ensure!(!preview.id.is_nil() && !preview.quote_ref.is_nil() && preview.total_atomic >= 0
            && preview.task.workspace_id.is_none(), "invalid guest preview");
        Ok(preview)
    }

    fn current_task(&self, catalog: &Catalog) -> anyhow::Result<Task> {
        if self.expires_at <= Utc::now() { return Err(Refusal::Expired.into()); }
        let mut task = self.task.clone();
        let proposal = task.proposal.as_mut().ok_or(Refusal::Changed)?;
        for step in proposal["steps"].as_array_mut().ok_or(Refusal::Changed)? {
            let id = step["service"]["id"].as_str().and_then(|value| value.parse::<Uuid>().ok()).ok_or(Refusal::Changed)?;
            let hash = step["service"]["version_hash"].as_str().ok_or(Refusal::Changed)?;
            let service = catalog.services.iter().find(|service| service.id == id && service.version_hash() == hash).ok_or(Refusal::Changed)?;
            step["service"] = serde_json::to_value(service)?;
        }
        let plan: Plan = serde_json::from_value(proposal.clone())?;
        plan.validate_independent_inputs(&task, &catalog.vocabulary).map_err(|_| Refusal::Changed)?;
        let current = requote::same_steps(catalog, &plan).ok_or(Refusal::Changed)?;
        anyhow::ensure!(plan.quote_ref == self.quote_ref && plan.expires_at == self.expires_at
            && plan.cap_atomic == self.total_atomic && current.cap_atomic == self.total_atomic
            && plan.hash == digest(&plan.steps), Refusal::Changed);
        Ok(task)
    }
}

impl Engine {
    /// All interpretation and resolution happens against an ephemeral Task.
    /// This function performs no task writes, authorization or source calls.
    /// The deterministic compiler plans first; the semantic planner is the
    /// bounded fallback for wording that exact contract fields cannot bind.
    pub(crate) async fn guest_plan(&self, question: &str, intake: crate::capability_intake::Intake, catalog: &Catalog) -> anyhow::Result<Value> {
        let task = Task { capability_intake:Some(intake.clone()), ..Task::default() };
        let reply = self.parser.chat_from_capabilities(question, None, &task, catalog, &intake).await?;
        self.guest_plan_from_reply(question, intake, catalog, reply).await
    }

    /// Price one validated planning reply; quotation itself cannot invoke AI.
    pub(crate) async fn guest_plan_from_reply(&self, question: &str, intake: crate::capability_intake::Intake, catalog: &Catalog, reply: crate::parser::ChatReply) -> anyhow::Result<Value> {
        let mut task = Task { capability_intake:Some(intake.clone()), ..Task::default() };
        task.output_grounding = crate::parser::source_grounding::Grounding::new(&intake, catalog, reply.output_bindings);
        task.record_turn(question, Some(reply.message.clone()));
        let status = if reply.discover {
            if let Some(mut intent) = reply.intent {
                // Ask calls remain independent; explicit workflow expansion is separate.
                input::bind_subjects(&mut task, catalog, &mut intent)?;
                name_lookup::seed(&mut task, catalog, &mut intent)?;
                task.focus.entity_refs = intent.subjects.iter().filter_map(|subject| subject.entity_ref).collect();
                task.focus.goal_refs = vec![Uuid::new_v4()];
                task.intent = Some(serde_json::to_value(&intent)?);
                self.plan(&mut task, catalog, &intent)?
            } else { Status::NeedsInput }
        } else if planning_message::is_terminal_discovery(&task, &reply.message) {
            planning_message::finish_without_plan(&mut task, json!({}))
        } else { Status::NeedsInput };
        guest_view(&self.signer, question, &task, status)
    }

    /// The server-minted preview ID is the primary key across all accounts,
    /// request IDs and replicas. An atomic insert never overwrites a running
    /// task, and a second account cannot adopt the same preview.
    pub(crate) async fn adopt_guest(&self, person: &Principal, preview: &Preview) -> anyhow::Result<(Uuid, bool)> {
        anyhow::ensure!(person.is_person() && person.workspace.is_none(), "guest approval requires a personal account");
        let existing = self.guest_adoption(person, preview).await?;
        if let Some(existing @ (_, true)) = existing { return Ok(existing); }
        if preview.expires_at <= Utc::now() { return Err(Refusal::Expired.into()); }
        let catalog = self.catalog.get().await?;
        let task = preview.current_task(&catalog)?;
        if let Some(existing) = existing { return Ok(existing); }
        sqlx::query("INSERT INTO tasks(id,owner_id,connection_id,document) VALUES($1,$2,NULL,$3) ON CONFLICT(id) DO NOTHING")
            .bind(preview.id).bind(person.owner).bind(serde_json::to_value(task)?).execute(&self.store.pool).await?;
        self.guest_adoption(person, preview).await?.ok_or_else(|| anyhow::anyhow!("guest request could not be saved"))
    }

    async fn guest_adoption(&self, person: &Principal, preview: &Preview) -> anyhow::Result<Option<(Uuid, bool)>> {
        use sqlx::Row;
        let row = sqlx::query("SELECT owner_id,connection_id,document->>'workspace_id' AS workspace,EXISTS(SELECT 1 FROM authorizations a WHERE a.task_id=t.id AND a.owner_id=$2 AND a.quote_ref=$3) AS approved FROM tasks t WHERE t.id=$1")
            .bind(preview.id).bind(person.owner).bind(preview.quote_ref).fetch_optional(&self.store.pool).await?;
        let Some(row) = row else { return Ok(None); };
        if row.get::<Uuid,_>("owner_id") != person.owner || row.get::<Option<Uuid>,_>("connection_id").is_some()
            || row.get::<Option<String>,_>("workspace").is_some() { return Err(Refusal::Used.into()); }
        Ok(Some((preview.id, row.get("approved"))))
    }
}

fn guest_view(signer: &Signer, question: &str, task: &Task, status: Status) -> anyhow::Result<Value> {
    let plan = task.proposal.as_ref().map(|value| serde_json::from_value::<Plan>(value.clone())).transpose()?;
    let sources: Vec<Value> = if let Some(plan) = &plan {
        plan.steps.iter().map(|step| json!({"name":step.service.name,"description":step.service.description,
            "source_slug":step.service.api_slug,"endpoint_id":step.service.endpoint_id,"price_atomic":plan.step_total(step).to_string()})).collect()
    } else {
        task.capability_intake.iter().flat_map(|intake| &intake.matches).flat_map(|found| &found.apiosk).take(8)
            .map(|source| json!({"name":source.name,"description":source.description,"source_slug":source.source_slug,
                "endpoint_id":source.endpoint_id,"price_atomic":null})).collect()
    };
    let ready = status == Status::Ready && plan.as_ref().is_some_and(|plan| !plan.steps.is_empty());
    let preview = if ready { Some(Preview::from_task(task)?) } else { None };
    let token = preview.as_ref().map(|preview| preview.seal(signer)).transpose()?;
    let message = task.actions.iter().find_map(|action| action["message"].as_str().map(str::to_owned))
        .or_else(|| task.conversation.last().and_then(|turn| turn.reply.clone()))
        .or_else(|| task.actions.iter().find_map(|action| action["semantic_type"].as_str().map(|field|
            format!("What is the {} for this request?", field.replace(['.', '_'], " "))))).unwrap_or_default();
    Ok(json!({"question":question,"status":if ready { "ready" } else if status == Status::Unsupported { "unsupported" } else { "needs_input" },
        "sources":sources,"total_atomic":preview.as_ref().map(|preview| preview.total_atomic.to_string()),"currency":"USDC",
        "preview_token":token,"expires_at":preview.as_ref().map(|preview| preview.expires_at),"message":message,
        "coverage_notices":task.output_grounding.as_ref().map(|grounding|grounding.notices()).unwrap_or_default()}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Goal, Subject};
    use std::collections::BTreeMap;

    fn catalog() -> Catalog {
        Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/integration-catalog.json")).unwrap())
    }

    fn quoted_task(catalog: &Catalog) -> Task {
        let mut task = Task::default();
        task.record_turn("Search Example", Some("Find matching companies.".into()));
        let mut intent = Intent {
            subjects:vec![Subject { entity_ref:None, label:"Example".into(), values:BTreeMap::from([("company.name".into(), json!("Example"))]) }],
            goals:vec![Goal { capability:"company.search".into(), subject:0, constraints:BTreeMap::new() }],
            answer_operations:vec![], unresolved_references:vec![],
        };
        input::bind_subjects(&mut task, catalog, &mut intent).unwrap();
        task.intent = Some(serde_json::to_value(&intent).unwrap());
        let Resolution::Plan(plan) = resolver::resolve(catalog, &task, &intent) else { panic!("fixture must resolve") };
        task.proposal = Some(serde_json::to_value(plan).unwrap());
        task.actions = vec![serde_json::to_value(Action::Run { action_id:Uuid::new_v4(), step:0 }).unwrap()];
        task
    }

    #[tokio::test]
    async fn independent_batch_collects_input_before_exposing_a_single_approval() {
        let catalog = catalog();
        let pool = sqlx::postgres::PgPoolOptions::new().connect_lazy("postgresql://postgres@127.0.0.1:1/unused").unwrap();
        let store = crate::store::Store { pool: pool.clone() };
        let http = reqwest::Client::new();
        let engine = Engine { store: store.clone(), catalog: crate::catalog::CatalogCache::new(pool),
            parser: crate::parser::Parser::new(http.clone(), "offline://unused".into(), String::new(), "fixture".into()),
            signer: Signer::new(vec![8;32]).unwrap(), app_url: "http://localhost".into(),
            executor: crate::execution::Executor { store, http, supabase_url: "http://127.0.0.1:1".into(), enabled: false } };
        let mut task = quoted_task(&catalog);
        // This boundary test supplies a draft directly; no interpretation or
        // provider is mocked as a successful live answer.
        task.capability_intake = Some(serde_json::from_value(json!({
            "parsed_request":{"language":"en","subjects":[],"capabilities":[],"deliverable":{"format":"chat","operations":[]},
                "meta":{"model":"fixture","parsed_at":"2026-10-13T00:00:00Z","text_sha256":"fixture"}},
            "matches":[],"selected_capabilities":[],"catalog_version":catalog.version,"apiosk_sources_searched":0
        })).unwrap());
        let mut intent: Intent = serde_json::from_value(task.intent.clone().unwrap()).unwrap();
        intent.goals.push(Goal { capability: "company.accounts".into(), subject: 0, constraints: BTreeMap::new() });
        let status = engine.plan(&mut task, &catalog, &intent).unwrap();
        assert_eq!(status, Status::NeedsInput);
        assert!(task.proposal.is_none());
        assert_eq!(task.actions.len(), 1);
        assert_eq!(task.actions[0]["kind"], "supply");
        let action: Action = serde_json::from_value(task.actions[0].clone()).unwrap();
        assert_eq!(action.public().kind, "supply_input");
        let preview = guest_view(&engine.signer, "Search Example and retrieve its accounts", &task, status).unwrap();
        assert!(preview["preview_token"].is_null() && preview["total_atomic"].is_null());
        assert!(preview["message"].as_str().unwrap().contains("What is"));
        let field = task.actions[0]["semantic_type"].as_str().unwrap().to_owned();
        engine.user_fact(&mut task, &catalog, intent.subjects[0].entity_ref.unwrap(), &field, json!("001234")).unwrap();
        assert_eq!(engine.plan(&mut task, &catalog, &intent).unwrap(), Status::Ready);
        let plan: Plan = serde_json::from_value(task.proposal.clone().unwrap()).unwrap();
        assert!(plan.independent_calls);
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(task.actions.len(), 1, "one approval for the complete batch");
        assert_eq!(task.actions[0]["kind"], "run");
        assert!(task.pending_dispatches.is_empty(), "planning cannot dispatch any API");
        let search = plan.steps.iter().find(|step| step.service.contract.capability_slug == "company.search").unwrap();
        let report = plan.steps.iter().find(|step| step.service.contract.capability_slug == "company.accounts").unwrap();
        engine.fail_step(&mut task, search, json!({"code":"source_unavailable"})).unwrap();
        assert!(task.failed_steps.contains_key(&search.key()));
        assert!(!task.failed_steps.contains_key(&report.key()), "a failed independent source cannot cancel another call with complete inputs");
        assert!(task.actions.iter().any(|action| action["kind"] == "run"));
    }

    #[test]
    fn guest_preview_preserves_exact_inputs_prices_and_quote_across_replicas() {
        let catalog = catalog();
        let task = quoted_task(&catalog);
        let signer = Signer::new(vec![8;32]).unwrap();
        let value = guest_view(&signer, "Search Example", &task, Status::Ready).unwrap();
        assert_eq!(value["status"], "ready");
        assert_eq!(value["total_atomic"], "23", "the fee is included");
        assert_eq!(value["sources"][0]["price_atomic"], "23");
        assert_eq!(value["sources"][0]["source_slug"], "fixture");
        let token = value["preview_token"].as_str().unwrap();
        let replica = Signer::new(vec![8;32]).unwrap();
        let preview = Preview::open(&replica, token).unwrap();
        let compact = serde_json::to_value(&preview).unwrap();
        assert!(compact["task"]["proposal"]["steps"][0]["service"]["fixed_params"].is_null());
        let restored = preview.current_task(&catalog).unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap(), serde_json::to_value(task).unwrap());
    }

    #[tokio::test]
    async fn unknown_founder_search_reaches_a_priced_quote_without_an_identity_or_provider_call() {
        let question="Look up to linkedin of the Founder of ExampleCo";
        let parsed=crate::capability_intake::normalize(&json!({"language":"en",
            "subjects":[{"id":"s1","label":"Founder of ExampleCo","type":"person",
                "identifiers":{"employer":"ExampleCo","role":"Founder","platform":"linkedin","query":question}}],
            "capabilities":[{"slug":"research.web.search","name":"Public web search",
                "description":"Find public professional profile pages matching employer and role criteria.","domain":"research",
                "inputs":["query"],"outputs":["url","title"],"subject_id":"s1","source_hint":null,"confidence":0.95}],
            "deliverable":{"format":"chat","operations":[]}}).to_string(),question,"fixture").unwrap();
        let mut snapshot:crate::catalog::Snapshot=serde_json::from_str(include_str!("../../tests/fixtures/workflows/catalog.json")).unwrap();
        snapshot.services.retain(|service|service.contract.capability_slug=="web.search");
        snapshot.known_sources.clear();
        let catalog=Catalog::build(snapshot);
        assert!(!catalog.services.is_empty(),"use the stored actual web-search contract");
        let matches=crate::ask_v2::search::search(&parsed,&catalog);
        let intake=crate::capability_intake::Intake {selected_capabilities:matches.iter().flat_map(|found|found.operations.iter().cloned()).collect(),
            parsed_request:parsed,matches,apiosk_sources_searched:1,catalog_version:catalog.version.clone()};
        // This explicit interpreted fixture compiles from reviewed contracts.
        // Model transport is covered by the separate intake/quote boundary tests.
        // Database and provider execution remain unreachable and disabled.
        let pool=sqlx::postgres::PgPoolOptions::new().connect_lazy("postgresql://postgres@127.0.0.1:1/unused").unwrap();
        let store=crate::store::Store {pool:pool.clone()};
        let http=reqwest::Client::new();
        let engine=Engine {store:store.clone(),catalog:crate::catalog::CatalogCache::new(pool),
            parser:crate::parser::Parser::new(http.clone(),"http://127.0.0.1:1/no-extra-model".into(),"fixture".into(),"fixture".into()),
            signer:Signer::new(vec![8;32]).unwrap(),app_url:"http://localhost".into(),
            executor:crate::execution::Executor {store,http,supabase_url:"http://127.0.0.1:1".into(),enabled:false}};
        let value=engine.guest_plan(question,intake.clone(),&catalog).await.unwrap();
        assert_eq!(value["status"],"ready","{value}");
        assert!(value["total_atomic"].as_str().unwrap().parse::<i64>().unwrap()>0);
        assert_eq!(value["sources"].as_array().unwrap().len(),1);
        let preview=Preview::open(&engine.signer,value["preview_token"].as_str().unwrap()).unwrap();
        let restored=preview.current_task(&catalog).unwrap();
        let plan:Plan=serde_json::from_value(restored.proposal.clone().unwrap()).unwrap();
        assert_eq!(plan.steps.len(),1);
        let step=&plan.steps[0];
        assert_eq!(step.service.contract.capability_slug,"web.search");
        assert_eq!(step.service.inputs(&restored,step.entity_ref,&catalog.vocabulary).unwrap()["query"],question);
        assert!(restored.completed.is_empty());
        assert!(!serde_json::to_string(&value).unwrap().to_lowercase().contains("siren"));

        // Search snippets/URLs must never be priced as a known person's full
        // profile or as detailed work-history retrieval.
        for output in ["profile","work_experience"] {
            let mut more=intake.clone();more.parsed_request.capabilities[0].outputs=vec![output.into()];
            let mut changed=restored.clone();changed.capability_intake=Some(more);
            let intent:Intent=serde_json::from_value(changed.intent.clone().unwrap()).unwrap();
            assert!(!crate::parser::plan_coverage::uncovered_outputs(&changed,&catalog,&intent,&plan).is_empty());
        }
    }

    #[tokio::test]
    #[ignore = "uses configured OpenRouter planner with saved synthetic parses and read-only catalog metadata; no provider execution"]
    async fn saved_live_people_searches_reach_quotes_against_production_contracts() {
        use axum::{extract::State,routing::post,Json,Router};
        use std::sync::{Arc,Mutex,atomic::{AtomicUsize,Ordering}};
        dotenvy::dotenv().ok();
        let snapshot:crate::catalog::Snapshot=serde_json::from_slice(&std::fs::read(std::env::var("FOUNDER_CATALOG_SNAPSHOT").unwrap()).unwrap()).unwrap();
        let evidence:Value=serde_json::from_slice(&std::fs::read(std::env::var("FOUNDER_PARSE_EVIDENCE").unwrap()).unwrap()).unwrap();
        let catalog=Catalog::build(snapshot);
        let pool=sqlx::postgres::PgPoolOptions::new().connect_lazy("postgresql://postgres@127.0.0.1:1/unused").unwrap();
        let store=crate::store::Store {pool:pool.clone()};
        let http=reqwest::Client::new();
        let key=std::env::var("OPENROUTER_API_KEY").ok().filter(|key|!key.trim().is_empty())
            .or_else(||std::env::var("INTENT_API_KEY").ok().filter(|key|!key.trim().is_empty()))
            .expect("configured OpenRouter credentials required");
        let calls=Arc::new(AtomicUsize::new(0));
        let completions=Arc::new(Mutex::new(Vec::<Value>::new()));
        // Count real completion requests without changing production code or
        // relying on Parser.calls, which only counts the separate parse API.
        type ModelState=(reqwest::Client,String,Arc<AtomicUsize>,Arc<Mutex<Vec<Value>>>);
        let proxy=Router::new().route("/",post(|State((http,key,calls,completions)):State<ModelState>,Json(body):Json<Value>|async move {
            calls.fetch_add(1,Ordering::Relaxed);
            let response=http.post("https://openrouter.ai/api/v1/chat/completions").bearer_auth(key).json(&body).send().await.unwrap();
            let status=response.status();
            let response=response.json::<Value>().await.unwrap();
            let reply=response.pointer("/choices/0/message/content").and_then(Value::as_str)
                .and_then(|raw|serde_json::from_str::<Value>(raw).ok());
            completions.lock().unwrap().push(json!({"status":status.as_u16(),
                "model":response["model"],"finish_reason":response.pointer("/choices/0/finish_reason"),"reply":reply}));
            (status,Json(response))
        })).with_state((http.clone(),key,calls.clone(),completions.clone()));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let model_url=format!("http://{}",listener.local_addr().unwrap());
        let model_server=crate::telemetry::spawn(async move {axum::serve(listener,proxy).await.unwrap()});
        let engine=Engine {store:store.clone(),catalog:crate::catalog::CatalogCache::new(pool),
            parser:crate::parser::Parser::new(http.clone(),model_url,"local-counting-proxy".into(),
                std::env::var("DISCOVERY_TEST_MODEL").unwrap_or_else(|_|crate::parser::MODEL.into())),
            signer:Signer::new(vec![8;32]).unwrap(),app_url:"http://localhost".into(),
            executor:crate::execution::Executor {store,http,supabase_url:"http://127.0.0.1:1".into(),enabled:false}};
        let mut reports=vec![];
        for row in evidence["rows"].as_array().unwrap() {
            if row["requested_goal"]!="discovery" {continue;}
            let question=row["question"].as_str().unwrap();
            assert!(["ExampleCo","Fictional Widgets","Voorbeeldbedrijf","Imaginary Labs","Fictitious Robotics","ImaginaryTool"].iter()
                .any(|synthetic|question.contains(synthetic)),"live replay is restricted to synthetic evidence");
            let mut raw=row["parsed_request"].clone();
            for cap in raw["capabilities"].as_array_mut().unwrap() {cap.as_object_mut().unwrap().remove("status");}
            let parsed:crate::capability_intake::ParsedRequest=serde_json::from_value(raw).unwrap();
            let matches=crate::ask_v2::search::search(&parsed,&catalog);
            let intake=crate::capability_intake::Intake {selected_capabilities:matches.iter().flat_map(|found|found.operations.iter().cloned()).collect(),
                parsed_request:parsed,matches,apiosk_sources_searched:catalog.known_sources.len(),catalog_version:catalog.version.clone()};
            let calls_before=calls.load(Ordering::Relaxed);
            let completions_before=completions.lock().unwrap().len();
            let value=engine.guest_plan(question,intake,&catalog).await.unwrap();
            let model_calls=calls.load(Ordering::Relaxed)-calls_before;
            let completed=completions.lock().unwrap()[completions_before..].to_vec();
            reports.push(json!({"question":question,"status":value["status"],"total_atomic":value["total_atomic"],"sources":value["sources"],
                "planner_model_calls":model_calls,"planner_completions":completed,"provider_calls":0}));
            if let Ok(path)=std::env::var("FOUNDER_QUOTE_EVIDENCE") {std::fs::write(path,serde_json::to_vec_pretty(&reports).unwrap()).unwrap();}
            // A ready quote after a model rejection/fallback is not evidence
            // that the model selected it. Require its first choice to succeed.
            assert_eq!(model_calls,1,"the first AI plan must succeed without repair or fallback for {question}");
            assert_eq!(completed.len(),1);
            assert_eq!(completed[0]["reply"]["discover"],true,"{question}: {completed:?}");
            assert_eq!(value["status"],"ready","{question}: {value}");
            let preview=Preview::open(&engine.signer,value["preview_token"].as_str().unwrap()).unwrap();
            let task=preview.current_task(&catalog).unwrap();
            let plan:Plan=serde_json::from_value(task.proposal.clone().unwrap()).unwrap();
            let chosen:Vec<_>=completed[0]["reply"]["intent"]["goals"].as_array().unwrap().iter()
                .map(|goal|(&goal["capability"],&goal["subject"])).collect();
            let retained=task.intent.as_ref().unwrap()["goals"].as_array().unwrap().iter()
                .map(|goal|(&goal["capability"],&goal["subject"])).collect::<Vec<_>>();
            assert_eq!(chosen,retained,"the final quote must retain the model's selected goals");
            assert_eq!(plan.steps.len(),1);
            let step=&plan.steps[0];
            assert_eq!(step.service.inputs(&task,step.entity_ref,&catalog.vocabulary).unwrap()["query"],question);
            assert!(task.completed.is_empty());
            reports.last_mut().unwrap()["literal_query_preserved"]=json!(true);
            reports.last_mut().unwrap()["selected_goals_retained"]=json!(true);
        }
        assert!(!reports.is_empty());
        assert!(calls.load(Ordering::Relaxed)>=reports.len());
        model_server.abort();
        if let Ok(path)=std::env::var("FOUNDER_QUOTE_EVIDENCE") {std::fs::write(path,serde_json::to_vec_pretty(&reports).unwrap()).unwrap();}
        println!("{} saved synthetic parses reached complete quotes through the live AI planner; no provider calls, approvals or task writes",reports.len());
    }

    #[test]
    fn guest_preview_rejects_tampering_other_signature_domains_and_oversize() {
        let signer = Signer::new(vec![8;32]).unwrap();
        let mut preview = Preview::from_task(&quoted_task(&catalog())).unwrap();
        let token = preview.seal(&signer).unwrap();
        let (_, signature) = token.split_once('.').unwrap();
        preview.total_atomic = 1;
        let bytes = serde_json::to_vec(&preview).unwrap();
        assert!(Preview::open(&signer, &format!("{}.{}", URL_SAFE_NO_PAD.encode(&bytes), signature)).is_err());
        assert!(Preview::open(&signer, &format!("{}.{}", URL_SAFE_NO_PAD.encode(&bytes), signer.search_signature(&bytes))).is_err());
        assert!(Preview::open(&Signer::new(vec![9;32]).unwrap(), &token).is_err());
        assert!(Preview::open(&signer, &"a".repeat(MAX_TOKEN_BYTES+1)).is_err());
    }

    #[test]
    fn guest_preview_requires_fresh_unchanged_contracts_and_total() {
        let catalog = catalog();
        let mut preview = Preview::from_task(&quoted_task(&catalog)).unwrap();
        let mut changed = catalog.clone();
        changed.services[0].list_atomic += 1;
        assert!(matches!(preview.current_task(&changed).unwrap_err().downcast_ref::<Refusal>(), Some(Refusal::Changed)));
        changed = catalog.clone();
        changed.services.clear();
        assert!(preview.current_task(&changed).is_err());
        preview.total_atomic += 1;
        assert!(preview.current_task(&catalog).is_err());
        preview.total_atomic -= 1;
        preview.expires_at = Utc::now() - chrono::Duration::seconds(1);
        assert!(matches!(preview.current_task(&catalog).unwrap_err().downcast_ref::<Refusal>(), Some(Refusal::Expired)));
    }

    #[test]
    fn guest_clarification_never_issues_an_approvable_token_or_fake_total() {
        let signer = Signer::new(vec![8;32]).unwrap();
        let mut task = Task::default();
        task.record_turn("Search a company", Some("Which company should I search for?".into()));
        let value = guest_view(&signer, "Search a company", &task, Status::NeedsInput).unwrap();
        assert_eq!(value["status"], "needs_input");
        assert!(value["preview_token"].is_null());
        assert!(value["total_atomic"].is_null());
        assert_eq!(value["message"], "Which company should I search for?");
    }

    #[tokio::test]
    #[ignore = "requires isolated support-db on TEST_DATABASE_URL; never uses a live database"]
    async fn guest_adoption_is_once_only_personal_scoped_and_recovers_after_expiry() {
        let url = std::env::var("TEST_DATABASE_URL").expect("isolated database required");
        assert!(url.starts_with("postgresql://postgres@127.0.0.1:"));
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1)
            .after_connect(|conn, _| Box::pin(async move {
                sqlx::query("SET search_path TO public,gateway_v2,catalog,providers,identity,billing,agents,actions,kyb,integrations,observability,research,extensions").execute(conn).await?;
                Ok(())
            })).connect(&url).await.unwrap();
        let store = Store { pool:pool.clone() };
        let http = reqwest::Client::new();
        let engine = Engine { store:store.clone(), catalog:CatalogCache::new(pool.clone()),
            parser:Parser::new(http.clone(), "http://127.0.0.1:1".into(), "unused".into(), "unused".into()),
            signer:Signer::new(vec![8;32]).unwrap(), app_url:"http://localhost:5173".into(),
            executor:Executor { store, http, supabase_url:"http://127.0.0.1:1".into(), enabled:false } };
        let catalog = engine.catalog.get().await.unwrap();
        let task = quoted_task(&catalog);
        let mut preview = Preview::from_task(&task).unwrap();
        let person = Principal { owner:Uuid::new_v4(), connection:None, workspace:None };
        let before:i64 = sqlx::query_scalar("SELECT count(*) FROM tasks").fetch_one(&pool).await.unwrap();
        let value = guest_view(&engine.signer, "Search Example", &task, Status::Ready).unwrap();
        assert!(value["preview_token"].is_string());
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM tasks").fetch_one(&pool).await.unwrap(), before);
        let (first, retry) = tokio::join!(engine.adopt_guest(&person, &preview), engine.adopt_guest(&person, &preview));
        assert_eq!(first.unwrap(), (preview.id,false));
        assert_eq!(retry.unwrap(), (preview.id,false));
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM tasks WHERE id=$1").bind(preview.id).fetch_one(&pool).await.unwrap(), 1);
        let (_, saved, _) = engine.store.load_scoped(&person, preview.id).await.unwrap();
        assert_eq!(saved.proposal, task.proposal);
        let other = Principal { owner:Uuid::new_v4(), ..person.clone() };
        assert!(matches!(engine.adopt_guest(&other,&preview).await.unwrap_err().downcast_ref::<Refusal>(), Some(Refusal::Used)));
        let workspace = Principal { workspace:Some(Uuid::new_v4()), ..person.clone() };
        assert!(engine.adopt_guest(&workspace,&preview).await.is_err());
        let reference = engine.executor.approve(&person, preview.id, preview.quote_ref, &preview.total_atomic.to_string()).await.unwrap();
        assert_eq!(reference, engine.executor.approve(&person, preview.id, preview.quote_ref, &preview.total_atomic.to_string()).await.unwrap());
        preview.expires_at = Utc::now()-chrono::Duration::seconds(1);
        assert_eq!(engine.adopt_guest(&person,&preview).await.unwrap(), (preview.id,true));
        let mut unclaimed = Preview::from_task(&task).unwrap();
        unclaimed.expires_at = Utc::now()-chrono::Duration::seconds(1);
        assert!(matches!(engine.adopt_guest(&person,&unclaimed).await.unwrap_err().downcast_ref::<Refusal>(), Some(Refusal::Expired)));
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM authorizations WHERE task_id=$1").bind(preview.id).fetch_one(&pool).await.unwrap(), 1);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM executions WHERE task_id=$1").bind(preview.id).fetch_one(&pool).await.unwrap(), 0);
    }
}
