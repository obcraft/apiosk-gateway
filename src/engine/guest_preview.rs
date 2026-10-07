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
    pub(crate) async fn guest_plan(&self, question: &str, intake: crate::capability_intake::Intake, catalog: &Catalog) -> anyhow::Result<Value> {
        let mut task = Task { capability_intake:Some(intake.clone()), ..Task::default() };
        let reply = self.parser.chat_from_capabilities(question, None, &task, catalog, &intake).await?;
        task.record_turn(question, Some(reply.message.clone()));
        let status = if reply.discover {
            if let Some(mut intent) = reply.intent {
                crate::uk_supplier::expand(&mut intent, catalog);
                input::bind_subjects(&mut task, catalog, &mut intent)?;
                name_lookup::seed(&mut task, catalog, &mut intent)?;
                task.focus.entity_refs = intent.subjects.iter().filter_map(|subject| subject.entity_ref).collect();
                task.focus.goal_refs = vec![Uuid::new_v4()];
                task.intent = Some(serde_json::to_value(&intent)?);
                self.plan(&mut task, catalog, &intent)?
            } else { Status::NeedsInput }
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
        .or_else(|| task.actions.iter().find_map(|action| action["semantic_type"].as_str().map(|field|
            format!("Add {} to your question so these sources can be priced.", field.replace(['.', '_'], " ")))))
        .or_else(|| task.conversation.last().and_then(|turn| turn.reply.clone())).unwrap_or_default();
    Ok(json!({"question":question,"status":if ready { "ready" } else if status == Status::Unsupported { "unsupported" } else { "needs_input" },
        "sources":sources,"total_atomic":preview.as_ref().map(|preview| preview.total_atomic.to_string()),"currency":"USDC",
        "preview_token":token,"expires_at":preview.as_ref().map(|preview| preview.expires_at),"message":message}))
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
