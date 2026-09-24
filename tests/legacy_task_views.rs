//! Tasks saved by earlier gateway versions stay readable: each file in
//! eval/replay/v1/legacy-tasks is inserted as stored and read back through
//! the production task view. Run by scripts/test-legacy-tasks.mjs.

use apiosk_gateway_v2::{
    auth::Principal,
    catalog::CatalogCache,
    engine::Engine,
    execution::Executor,
    parser::Parser,
    state::Signer,
    store::Store,
};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use std::path::Path;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires an isolated support-db (scripts/test-legacy-tasks.mjs); no live database"]
async fn saved_tasks_of_every_shape_stay_readable() {
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|conn, _| Box::pin(async move {
            sqlx::query("SET search_path TO public,gateway_v2").execute(conn).await?;
            Ok(())
        }))
        .connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let store = Store { pool: pool.clone() };
    let http = reqwest::Client::new();
    let engine = Engine {
        store: store.clone(),
        catalog: CatalogCache::new(pool.clone()),
        parser: Parser::new(http.clone(), "http://127.0.0.1:1".into(), "".into(), "fixture".into()),
        signer: Signer::new(vec![9; 32]).unwrap(),
        executor: Executor { store, http, supabase_url: "http://127.0.0.1:1".into(), enabled: false },
        app_url: "http://localhost:5173".into(),
    };
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("eval/replay/v1/legacy-tasks");
    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|entry| entry.unwrap().path()).collect();
    files.sort();
    assert!(files.len() >= 7, "every legacy shape has a fixture");
    for file in files {
        let name = file.file_stem().unwrap().to_string_lossy().into_owned();
        // Result ids are global keys: give each inserted copy its own.
        let mut text = std::fs::read_to_string(&file).unwrap();
        let stored: Value = serde_json::from_str(&text).unwrap();
        for result in stored["results"].as_object().unwrap().keys() {
            text = text.replace(result.as_str(), &Uuid::new_v4().to_string());
        }
        let fixture: Value = serde_json::from_str(&text).unwrap();
        let person = Principal { owner: Uuid::new_v4(), connection: None, workspace: None };
        let task = Uuid::new_v4();
        sqlx::query("INSERT INTO tasks(id,owner_id,connection_id,document) VALUES($1,$2,NULL,$3)")
            .bind(task).bind(person.owner).bind(&fixture["task_document"]).execute(&pool).await.unwrap();
        for (result, document) in fixture["results"].as_object().unwrap() {
            let execution = Uuid::new_v4();
            sqlx::query("INSERT INTO executions(id,owner_id,task_id,action_id,payload_hash,selection_id,reserved_atomic,actual_atomic,status) VALUES($1,$2,$3,$1,'fixture',$1,0,0,'succeeded')")
                .bind(execution).bind(person.owner).bind(task).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO results(id,owner_id,task_id,execution_id,document) VALUES($1,$2,$3,$4,$5)")
                .bind(result.parse::<Uuid>().unwrap()).bind(person.owner).bind(task).bind(execution).bind(document).execute(&pool).await.unwrap();
        }
        let view = engine.view(&person, task, Uuid::new_v4(), None).await.unwrap_or_else(|error| panic!("{name}: {error}"));
        let expect = &fixture["expect"];
        assert_eq!(json!(view.status), expect["status"], "{name}: status");
        let turns = view.context_view["conversation"].as_array().map_or(0, Vec::len);
        assert_eq!(json!(turns), expect["turns"], "{name}: conversation turns");
        let analysis = &view.context_view["analysis"];
        assert_eq!(json!(analysis["observations"].as_array().map(Vec::len)), expect["observations"], "{name}: observations");
        assert_eq!(analysis["presentation"]["kind"], expect["presentation_kind"], "{name}: presentation kind");
        assert_eq!(*analysis, fixture["task_document"]["analysis"], "{name}: the saved analysis passes through unchanged");
        assert!(view.result.as_ref().is_some_and(Value::is_object), "{name}: the last saved result is readable");
        let active = fixture["task_document"]["active_results"].as_array().map_or(0, Vec::len);
        assert_eq!(view.context_view["results"].as_array().map_or(0, Vec::len), active, "{name}: current results");
        assert!(view.context_view["report"].is_null() == fixture["task_document"]["proposal"].is_null(), "{name}: a report link needs a saved quote");
    }
}
