use super::{authenticated, failure, ServerState};
use crate::{company_checks::Company, protocol::DiscoverRequest, recurring};
use axum::{extract::{Path, Query, State}, http::{HeaderMap, StatusCode}, response::{IntoResponse, Response}, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

pub(super) async fn person(s: &ServerState, h: &HeaderMap) -> Result<crate::auth::Principal, Response> {
    let caller = authenticated(s, h).await.map_err(|r| failure(r.status, r.code, r.message))?;
    if caller.api_key.is_some() || caller.principal.connection.is_some() {
        return Err(failure(StatusCode::FORBIDDEN, "app_only", "Manage monitoring in the Apiosk app."));
    }
    Ok(caller.principal)
}

pub(super) fn reply(result: anyhow::Result<Value>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => {
            tracing::warn!(%error, "monitoring request refused");
            let message = if error.downcast_ref::<sqlx::Error>().is_some() {
                "The monitor is unavailable. Check your workspace and refresh the page.".to_owned()
            } else { error.to_string() };
            failure(StatusCode::CONFLICT, "monitoring_refused", &message)
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Prepare {
    request_id: Uuid,
    company: Company,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
    establishment_number: Option<String>,
}

pub(super) async fn prepare(State(s): State<ServerState>, h: HeaderMap, Json(b): Json<Prepare>) -> Response {
    let p = match person(&s, &h).await { Ok(p) => p, Err(r) => return r };
    if !recurring::enabled_for(p.owner) {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "monitoring_unavailable", "Monitoring is not enabled for this account yet.");
    }
    reply(async {
        crate::company_checks::validate_companies(std::slice::from_ref(&b.company))?;
        let sources=crate::monitoring::sources(b.sources)?;
        if let Some(number)=&b.establishment_number {
            anyhow::ensure!(number.len()==12 && number.bytes().all(|c|c.is_ascii_digit()),"Choose an exact twelve-digit KVK establishment number.");
        }
        let response=prepare_check(&s.engine,&p,b.request_id,&b.company,&sources,b.establishment_number.as_deref()).await?;
        Ok(serde_json::to_value(response)?)
    }.await)
}

/// Shared first-check preparation for the app wizard and its Slack dossiers.
pub(super) async fn prepare_check(engine:&crate::engine::Engine,p:&crate::auth::Principal,request_id:Uuid,company:&Company,sources:&[String],establishment:Option<&str>)->anyhow::Result<crate::protocol::Response> {
    engine.store.check_spending(p).await?;
    let question=format!("Nulmeting voor bedrijfsmonitoring: {} (KVK {}). Gebruik de geselecteerde bronnen voor dit exacte bedrijf. Sanctiematches zijn mogelijke overeenkomsten die menselijke controle vereisen.",company.name,company.kvk_number);
    let catalog=engine.catalog.fresh().await?;
    let (recipe,gaps)=crate::monitoring::prepare_recipe(company,sources,establishment,&catalog);
    let mut response=engine.discover_recipe(p,DiscoverRequest{request_id,question,state:None,context_delta:Default::default()},recipe).await?;
    response.context_view["monitoring_setup"]=json!({"company":company,"sources":sources,"coverage_gaps":gaps});
    Ok(response)
}

#[derive(Deserialize)]
pub(super) struct Search {q:String,#[serde(default="first_page")] page:u32}
fn first_page()->u32 {1}
pub(super) async fn search(State(s):State<ServerState>,h:HeaderMap,Query(b):Query<Search>)->Response {
    let p=match person(&s,&h).await {Ok(p)=>p,Err(r)=>return r};
    let Ok(_permit)=s.permits.try_acquire() else {return failure(StatusCode::TOO_MANY_REQUESTS,"busy","Try again shortly.");};
    match async {s.engine.store.check_workspace(&p).await?;crate::monitoring_search::search(&s.engine.store.pool,p.owner,&b.q,b.page).await}.await {
        Ok(value)=>Json(value).into_response(),
        Err(error)=>match error.downcast_ref::<crate::monitoring_search::SearchError>() {
            Some(crate::monitoring_search::SearchError::Invalid)=>failure(StatusCode::BAD_REQUEST,"invalid_search","Enter a company name or eight-digit KVK number."),
            Some(crate::monitoring_search::SearchError::RateLimited)=>failure(StatusCode::TOO_MANY_REQUESTS,"search_rate_limited","Please wait briefly before searching again."),
            Some(crate::monitoring_search::SearchError::Unavailable)=>failure(StatusCode::SERVICE_UNAVAILABLE,"search_unavailable","KVK search is temporarily unavailable."),
            None=>reply(Err(error)),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Quote {
    request_id: Uuid,
    task_id: Uuid,
    company: Company,
    schedule: recurring::Schedule,
    delivery: recurring::DeliveryRequest,
    billing_mode: String,
    #[serde(default)]
    selected_fields: Vec<String>,
    #[serde(default)]
    sources: Vec<String>,
    alert_title: String,
    alert_message: String,
}

pub(super) async fn quote(State(s): State<ServerState>, h: HeaderMap, Json(b): Json<Quote>) -> Response {
    let p = match person(&s, &h).await { Ok(p) => p, Err(r) => return r };
    if !recurring::enabled_for(p.owner) {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "monitoring_unavailable", "Monitoring is not enabled for this account yet.");
    }
    let request = recurring::QuoteRequest {request_id:b.request_id, task_ids:vec![b.task_id],
        question:format!("Bedrijfsmonitoring: {} (KVK {})", b.company.name, b.company.kvk_number),
        schedule:b.schedule, delivery:b.delivery, date_overrides:vec![], billing_mode:b.billing_mode, change_only:true};
    reply(recurring::quote_monitor(&s.engine, &p, request, b.company, b.selected_fields,b.sources,b.alert_title,b.alert_message).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Baseline { task_id: Uuid, company: Company, #[serde(default)] sources:Vec<String> }

pub(super) async fn baseline(State(s): State<ServerState>, h: HeaderMap, Json(b): Json<Baseline>) -> Response {
    let p = match person(&s, &h).await { Ok(p) => p, Err(r) => return r };
    reply(async {
        s.engine.store.check_workspace(&p).await?;
        let source_ids=crate::monitoring::sources(b.sources)?;
        let baseline = crate::monitoring::baseline(&s.engine, &p, b.task_id, &b.company,&source_ids).await?;
        let fields = crate::monitoring::fields(&baseline);
        Ok(json!({"company":b.company,"task_id":b.task_id,"baseline":baseline,"fields":fields}))
    }.await)
}

pub(super) async fn capabilities(State(s): State<ServerState>, h: HeaderMap) -> Response {
    let p=match person(&s,&h).await {Ok(p)=>p,Err(r)=>return r};
    reply(async {
        s.engine.store.check_workspace(&p).await?;
        let catalog=s.engine.catalog.fresh().await?;
        let sources:Vec<Value>=[(crate::monitoring::PROVIDER,"KVK Handelsregister","company.registry-profile",true),
            (crate::monitoring::SANCTIONS,"OpenSanctions","company.sanctions-screen",false),(crate::monitoring::GLEIF,"GLEIF","company.legal-entity-search",false)].into_iter().map(|(id,name,capability,required)| {
            let available=catalog.services.iter().any(|s|s.api_slug==id && s.contract.capability_slug==capability);
            let operations:Vec<Value> = if id==crate::monitoring::PROVIDER {vec!["company.registry-profile","company.registered-names","company.branch-profile","company.financial-statements"]}else{vec![capability]}.into_iter().map(|op|json!({"capability":op,"available":catalog.services.iter().any(|s|s.api_slug==id && s.contract.capability_slug==op)})).collect();
            json!({"id":id,"name":name,"available":available,"required":required,"default_selected":id!=crate::monitoring::GLEIF,
                "operations":operations})
        }).collect();
        Ok(json!({"sources":sources,"fields":crate::monitoring::fields(&json!({})),
            "limitations":["Only sources with implemented evidence comparisons are offered. Missing coverage is shown explicitly.","Sanctions matches require human identity review."]}))
    }.await)
}

pub(super) async fn list(State(s): State<ServerState>, h: HeaderMap) -> Response {
    let p = match person(&s, &h).await { Ok(p) => p, Err(r) => return r };
    reply(async {
        s.engine.store.check_workspace(&p).await?;
        let enabled = recurring::enabled_for(p.owner);
        let management_enabled = recurring::management_enabled_for(p.owner);
        if !management_enabled { return Ok(json!({"enabled":enabled,"management_enabled":false,"monitors":[]})); }
        let monitors: Vec<Value> = sqlx::query_scalar(r#"select jsonb_build_object(
            'id',o.id,'company',o.configuration#>'{monitoring,company}','state',o.state,
            'schedule',o.configuration->'schedule','delivery',o.delivery,'total_per_run',o.total_per_run::text,
            'last_run',(select jsonb_build_object('id',r.id,'scheduled_at',r.scheduled_at,'state',r.state,'error',r.error,
                'has_result',r.result is not null,'coverage_status',r.result#>>'{monitoring,status}',
                'coverage_attention_required',r.result#>'{monitoring,coverage_attention_required}',
                'delivery_state',coalesce(d.state,case when r.state='succeeded' then 'unchanged' end))
                from billing.recurring_runs r left join billing.recurring_deliveries d on d.run_id=r.id
                where r.order_id=o.id and r.finished_at is not null order by r.scheduled_at desc limit 1),
            'next_run',(select min(r.scheduled_at) from billing.recurring_runs r where r.order_id=o.id and r.finished_at is null))
            from billing.recurring_orders o where o.owner_id=$1 and o.workspace_id is not distinct from $2
                and o.workflow->>'processing'=$3 order by o.created_at desc limit 100"#)
            .bind(p.owner).bind(p.workspace).bind(crate::monitoring::VERSION).fetch_all(&s.engine.store.pool).await?;
        Ok(json!({"enabled":enabled,"management_enabled":management_enabled,"monitors":monitors}))
    }.await)
}

pub(super) async fn read(State(s): State<ServerState>, h: HeaderMap, Path(id): Path<Uuid>) -> Response {
    let p = match person(&s, &h).await { Ok(p) => p, Err(r) => return r };
    reply(async {
        let mut detail = recurring::read(&s.engine, &p, id).await?;
        anyhow::ensure!(detail["workflow"]["processing"] == crate::monitoring::VERSION, "This monitor is unavailable.");
        let latest: Option<Value> = sqlx::query_scalar("select result->'monitoring' from billing.recurring_runs where order_id=$1 and finished_at is not null and result->'monitoring' is not null order by scheduled_at desc limit 1")
            .bind(id).fetch_optional(&s.engine.store.pool).await?;
        let next: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar("select min(scheduled_at) from billing.recurring_runs where order_id=$1 and finished_at is null")
            .bind(id).fetch_one(&s.engine.store.pool).await?;
        detail["monitoring"] = json!({"company":detail["configuration"]["monitoring"]["company"],"task_id":detail["configuration"]["monitoring"]["task_id"],"selected_fields":detail["configuration"]["monitoring"]["selected_fields"],"sources":detail["configuration"]["monitoring"]["sources"],"alert":detail["configuration"]["monitoring"]["alert"],
            "baseline":detail["configuration"]["monitoring"]["baseline"],"latest":latest,"next_run":next});
        Ok(detail)
    }.await)
}
