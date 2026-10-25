use super::*;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::time::Duration;

pub(super) fn start(engine: Arc<Engine>) {
    for _ in 0..4 {
        let engine = engine.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(10));
            loop {
                tick.tick().await;
                if let Err(e) = pass(&engine).await {
                    tracing::warn!(error=%e,"recurring execution pass failed");
                }
            }
        });
    }
    let engine = engine.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        loop {
            tick.tick().await;
            if let Err(e) = deliver(&engine).await {
                tracing::warn!(error=%e,"recurring delivery pass failed");
            }
        }
    });
}
fn token(secret: &str, call: Uuid) -> String {
    let expires = Utc::now().timestamp() + 90;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("recurring-v1:{call}:{expires}").as_bytes());
    format!(
        "recurring.{expires}.{}",
        hex::encode(mac.finalize().into_bytes())
    )
}

fn failed_call(call: Uuid, state: &str, body: &Value, refused_without_charge: bool) -> Value {
    let error = if state == "unresolved" {
        json!({"code":"recurring_outcome_unresolved", "message":"Payment or result needs reconciliation. This call was not retried."})
    } else if refused_without_charge && body["error"] == "unauthorized" {
        json!({"code":"recurring_execution_unauthorized", "message":"The recurring execution service rejected authorization. Check that Gateway and Supabase use the same RECURRING_WORKER_SECRET. No source purchase was made."})
    } else if refused_without_charge {
        crate::billing::public_error(body)
    } else {
        json!({"code":"recurring_source_failed", "message":"The source did not complete. Check the saved charge status."})
    };
    json!({"call_id":call,"state":state,"code":error["code"],"message":error["message"]})
}

async fn finish(
    e: &Engine,
    run: Uuid,
    state: &str,
    result: Value,
    error: Option<&str>,
) -> anyhow::Result<()> {
    sqlx::query("select billing.finish_recurring_run($1,$2,$3,$4)")
        .bind(run)
        .bind(state)
        .bind(result)
        .bind(error)
        .execute(&e.store.pool)
        .await?;
    Ok(())
}

pub(super) async fn pass(e: &Arc<Engine>) -> anyhow::Result<()> {
    let owners = allowed_owners();
    if !enabled() || owners.is_empty() {
        return Ok(());
    }
    refill(e).await?;
    let lease = Uuid::new_v4();
    let row=sqlx::query("with due as (select r.id from billing.recurring_runs r join billing.recurring_orders o on o.id=r.order_id where o.owner_id = any($2::uuid[]) and ((r.state='queued' and r.scheduled_at<=now() and (o.state='active' or r.scheduled_at<now()-interval '1 hour')) or (r.state='running' and r.lease_until<now())) and not exists(select 1 from billing.recurring_runs previous where previous.order_id=r.order_id and previous.scheduled_at<r.scheduled_at and previous.finished_at is null) order by r.scheduled_at for update of r skip locked limit 1) update billing.recurring_runs r set state='running',lease_id=$1,lease_until=now()+interval '3 minutes' from due where r.id=due.id returning r.id,r.order_id,r.scheduled_at")
  .bind(lease).bind(&owners).fetch_optional(&e.store.pool).await?;
    let Some(row) = row else { return Ok(()) };
    let run: Uuid = row.get("id");
    let order: Uuid = row.get("order_id");
    run_claimed(e, run, order, row.get("scheduled_at"), lease).await
}

pub(super) async fn run_claimed(
    e: &Arc<Engine>, run: Uuid, order: Uuid, scheduled: DateTime<Utc>, lease: Uuid,
) -> anyhow::Result<()> {
    let job = execute(e, run, order, scheduled);
    let renew = async {
        let mut tick = tokio::time::interval(Duration::from_secs(20));
        loop {
            tick.tick().await;
            let n=sqlx::query("update billing.recurring_runs set lease_until=now()+interval '3 minutes' where id=$1 and lease_id=$2 and state='running'").bind(run).bind(lease).execute(&e.store.pool).await?.rows_affected();
            anyhow::ensure!(n == 1, "Lease lost.");
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };
    tokio::select! {r=job=>r,r=renew=>r}
}

async fn execute(
    e: &Engine,
    run: Uuid,
    order: Uuid,
    scheduled: DateTime<Utc>,
) -> anyhow::Result<()> {
    let o=sqlx::query("select owner_id,workspace_id,state,sources,question,timezone,configuration,workflow,billing_mode,delivery from billing.recurring_orders where id=$1").bind(order).fetch_one(&e.store.pool).await?;
    let p = Principal {
        owner: o.get("owner_id"),
        workspace: o.get("workspace_id"),
        connection: None,
    };
    anyhow::ensure!(
        enabled_for(p.owner),
        "Recurring execution is not available for this account."
    );
    let sources: Vec<Source> = serde_json::from_value(o.get("sources"))?;
    let question: String = o.get("question");
    let workflow = match o
        .get::<Option<Value>, _>("workflow")
        .and_then(|v| serde_json::from_value::<LockedWorkflow>(v).ok())
    {
        Some(w)
            if w.validate(p.owner, &o.get("configuration"), &sources, &question)
                .is_ok() =>
        {
            w
        }
        _ => {
            sqlx::query("update billing.recurring_orders set state='attention' where id=$1 and state='active'").bind(order).execute(&e.store.pool).await?;
            return finish(e,run,"failed",Value::Null,Some("The approved workflow is unavailable or changed. Review a new plan; no replacement source was used.")).await;
        }
    };
    let delivery: Delivery = serde_json::from_value(o.get("delivery"))?;
    let count: i64 =
        sqlx::query_scalar("select count(*) from billing.recurring_calls where run_id=$1")
            .bind(run)
            .fetch_one(&e.store.pool)
            .await?;
    if count == 0 {
        if validate_self_delivery(e, p.owner, &delivery, &workflow.delivery, o.get::<Value, _>("configuration")["monitoring"].is_object())
            .await
            .is_err()
        {
            sqlx::query("update billing.recurring_orders set state='attention' where id=$1 and state='active'").bind(order).execute(&e.store.pool).await?;
            return finish(e, run, "failed", Value::Null, Some("The delivery destination changed or is unavailable. Review a new plan; no purchase was made.")).await;
        }
        if o.get::<String, _>("state") != "active" {
            return finish(
                e,
                run,
                "skipped",
                Value::Null,
                Some("This occurrence was paused or unavailable; no purchase was made."),
            )
            .await;
        }
        if Utc::now() - scheduled > chrono::Duration::hours(1) {
            return finish(
                e,
                run,
                "skipped",
                Value::Null,
                Some("The scheduled time was missed; no catch-up purchase was made."),
            )
            .await;
        }
        if e.store.check_spending(&p).await.is_err() {
            return finish(
                e,
                run,
                "failed",
                Value::Null,
                Some("Spending access is no longer available."),
            )
            .await;
        }
        let ids: Vec<Uuid> = workflow
            .recipe
            .steps
            .iter()
            .filter_map(|s| s.service.as_ref().map(|s| s.id))
            .collect();
        let catalog = e.catalog.fresh_execution(&ids).await?;
        if workflow.recipe.validate_catalog(&catalog.services).is_err() {
            sqlx::query("update billing.recurring_orders set state='attention' where id=$1 and state='active'").bind(order).execute(&e.store.pool).await?;
            return finish(e,run,"failed",Value::Null,Some("A source contract or price changed. Review a new plan; the source was not replaced.")).await;
        }
        let Some(composio) = crate::integrations::configured() else {
            return finish(
                e,
                run,
                "failed",
                Value::Null,
                Some("The approved delivery connection is unavailable."),
            )
            .await;
        };
        if composio
            .check_frozen(p.owner, &workflow.delivery)
            .await
            .is_err()
        {
            sqlx::query("update billing.recurring_orders set state='attention' where id=$1 and state='active'").bind(order).execute(&e.store.pool).await?;
            return finish(
                e,
                run,
                "failed",
                Value::Null,
                Some("Reconnect the exact approved delivery account or review a new plan."),
            )
            .await;
        }
        anyhow::ensure!(
            enabled_for(p.owner),
            "Recurring execution access was revoked."
        );
        let funded: bool = sqlx::query_scalar("select billing.fund_recurring_run($1)")
            .bind(run)
            .fetch_one(&e.store.pool)
            .await?;
        if !funded {
            return finish(
                e,
                run,
                "skipped_insufficient_balance",
                Value::Null,
                Some("Insufficient available balance for the entire run. No source was purchased."),
            )
            .await;
        }
    }
    let local: NaiveDate = sqlx::query_scalar("select ($1::timestamptz at time zone $2)::date")
        .bind(scheduled)
        .bind(o.get::<String, _>("timezone"))
        .fetch_one(&e.store.pool)
        .await?;
    let catalog = e.catalog.get().await?;
    let mut results = Vec::new();
    let mut prior: Vec<recipe::StepResult> = Vec::new();
    let mut failed = false;
    let mut unresolved = false;
    for (ordinal, source) in sources.iter().enumerate() {
        anyhow::ensure!(
            enabled_for(p.owner),
            "Recurring execution access was revoked."
        );
        let mut call=sqlx::query("select id,ordinal,state,response from billing.recurring_calls where run_id=$1 and ordinal=$2").bind(run).bind(ordinal as i32).fetch_optional(&e.store.pool).await?;
        if call.is_none() {
            let (inputs, input_parts) = match workflow
                .recipe
                .materialize(ordinal, &prior, scheduled, local, &catalog.vocabulary)
                .and_then(|inputs| {
                    let parts = workflow.recipe.input_parts(ordinal, &inputs)?;
                    Ok((inputs, parts))
                }) {
                Ok(inputs) => inputs,
                Err(_) => {
                    failed = true;
                    results.push(json!({"state":"failed","message":"A required source value is missing or ambiguous. The approved sequence stopped without changing sources."}));
                    break;
                }
            };
            let mut tx = e.store.pool.begin().await?;
            let state: String =
                sqlx::query_scalar("select billing.lock_recurring_order_for_call($1,$2)")
                    .bind(p.owner)
                    .bind(order)
                    .fetch_one(&mut *tx)
                    .await?;
            if state != "active" {
                tx.rollback().await?;
                let mut partial =
                    processing::result(&question, scheduled, results, &workflow.fingerprint);
                partial["analysis"] = Value::Null;
                partial["partial"] = json!(true);
                return finish(e,run,if state=="cancelled" {"cancelled"}else{"skipped"},partial,Some("The sequence stopped before the next source. Completed paid source results remain saved.")).await;
            }
            let selection = Uuid::new_v4();
            let s = &source.selection;
            let mut meta = s["metadata"].clone();
            meta["inputs"] = inputs.clone();
            meta["recurring_run_id"] = json!(run);
            meta["workflow_hash"] = json!(workflow.fingerprint);
            sqlx::query("insert into actions.user_selected_apis(id,user_id,connection_id,prompt,kind,source,name,capability,api_slug,candidate_id,resource_url,method,provider_price_micro_usd,charged_micro_usd,metadata) values($1,$2,null,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)")
                .bind(selection).bind(p.owner).bind(s["prompt"].as_str().unwrap_or("Recurring order")).bind(s["kind"].as_str()).bind(s["source"].as_str()).bind(s["name"].as_str()).bind(s["capability"].as_str()).bind(s["api_slug"].as_str()).bind(s["candidate_id"].as_str().and_then(|v|v.parse::<Uuid>().ok())).bind(s["resource_url"].as_str()).bind(s["method"].as_str()).bind(s["provider_price_micro_usd"].as_i64()).bind(s["charged_micro_usd"].as_i64()).bind(meta).execute(&mut *tx).await?;
            if let Some(workspace) = p.workspace {
                sqlx::query("select public.workspace_bind_execution($1,null,$2,$3,$4,$5,$6)")
                    .bind(p.owner)
                    .bind(workspace)
                    .bind(selection)
                    .bind(run)
                    .bind(s["charged_micro_usd"].as_i64())
                    .bind(Utc::now() + chrono::Duration::hours(2))
                    .execute(&mut *tx)
                    .await?;
            }
            sqlx::query("insert into billing.recurring_calls(run_id,ordinal,selection_id,inputs,input_parts,cap_micro_usd) values($1,$2,$3,$4,$5,$6)").bind(run).bind(ordinal as i32).bind(selection).bind(inputs).bind(input_parts).bind(s["charged_micro_usd"].as_i64()).execute(&mut *tx).await?;
            tx.commit().await?;
            call=sqlx::query("select id,ordinal,state,response from billing.recurring_calls where run_id=$1 and ordinal=$2").bind(run).bind(ordinal as i32).fetch_optional(&e.store.pool).await?;
        }
        let call = call.ok_or_else(|| anyhow::anyhow!("Missing recurring call."))?;
        let id: Uuid = call.get("id");
        let state: String = call.get("state");
        if matches!(state.as_str(), "succeeded" | "failed" | "unresolved") {
            let response = call
                .get::<Option<Value>, _>("response")
                .unwrap_or(Value::Null);
            if state != "succeeded" {
                failed = true;
                unresolved = state == "unresolved";
                results.push(response);
                break;
            }
            prior.push(recipe::StepResult {
                ordinal,
                source_selection_id: workflow.recipe.steps[ordinal].source_selection_id,
                body: json!({"ok":true,"result":response["document"]["data"]}),
            });
            results.push(response);
            continue;
        }
        let mut body = Value::Null;
        if state == "pending" {
            // A lease can resume after a catalog update. Recheck every still
            // undispatched call against the approved contracts, even when a
            // previous step of this occurrence has already been paid for.
            let ids: Vec<Uuid> = workflow
                .recipe
                .steps
                .iter()
                .filter_map(|s| s.service.as_ref().map(|s| s.id))
                .collect();
            let current = e.catalog.fresh_execution(&ids).await?;
            if workflow.recipe.validate_catalog(&current.services).is_err() {
                sqlx::query("update billing.recurring_orders set state='attention' where id=$1 and state='active'").bind(order).execute(&e.store.pool).await?;
                let response = json!({"call_id":id,"state":"failed","message":"A source contract or price changed. Completed paid results remain saved; no replacement source was used."});
                sqlx::query("update billing.recurring_calls set state='failed',response=$2 where id=$1 and state='pending'").bind(id).bind(&response).execute(&e.store.pool).await?;
                results.push(response);
                failed = true;
                break;
            }
            let state: String =
                sqlx::query_scalar("select state from billing.recurring_orders where id=$1")
                    .bind(order)
                    .fetch_one(&e.store.pool)
                    .await?;
            if state != "active" {
                let mut partial =
                    processing::result(&question, scheduled, results, &workflow.fingerprint);
                partial["analysis"] = Value::Null;
                partial["partial"] = json!(true);
                return finish(e,run,if state=="cancelled" {"cancelled"}else{"skipped"},partial,Some("The sequence stopped before the next source. Completed paid source results remain saved.")).await;
            }
            anyhow::ensure!(
                enabled_for(p.owner),
                "Recurring execution access was revoked."
            );
            let n=sqlx::query("update billing.recurring_calls set state='dispatching',dispatched_at=now() where id=$1 and state='pending'").bind(id).execute(&e.store.pool).await?.rows_affected();
            if n == 0 {
                anyhow::bail!("Call claimed by another worker.");
            }
            let secret = std::env::var("RECURRING_WORKER_SECRET")?;
            let response = e
                .executor
                .http
                .post(format!(
                    "{}/functions/v1/recurring-execute",
                    e.executor.supabase_url
                ))
                .bearer_auth(token(&secret, id))
                .header("x-apiosk-recurring-call", id.to_string())
                .json(&json!({}))
                .timeout(Duration::from_secs(80))
                .send()
                .await;
            if let Ok(r) = response {
                if let Ok(v) = r.json::<Value>().await {
                    body = v;
                }
            }
        }
        // A restarted worker only reads the charge journal. It never resends a
        // dispatch whose outcome might already have cost money.
        let charge:Option<(String,i64,Uuid)>=sqlx::query_as("select status,charged_micro_usd,id from balance_charges where user_id=$1 and idempotency_key=$2").bind(p.owner).bind(id.to_string()).fetch_optional(&e.store.pool).await?;
        let refused_without_charge = charge.is_none() && crate::billing::refused_before_reservation(&body);
        let final_state = match charge {
            Some((ref status, amount, charge)) if status == "charged" => {
                let saved:Option<(Value,DateTime<Utc>)>=sqlx::query_as("select output,started_at from call_runs where charge_id=$1 and user_id=$2 and status='succeeded' and output_truncated is not true order by started_at desc limit 1").bind(charge).bind(p.owner).fetch_optional(&e.store.pool).await?;
                let retrieved_at=saved.as_ref().map(|(_,at)|*at).unwrap_or_else(Utc::now);
                let data = if body["ok"] == true && body["result_truncated"] != true {
                    body.get("result").cloned()
                } else {
                    None
                }
                .or_else(||saved.map(|(data,_)|data));
                if let Some(data) = data {
                    let source = &sources[call.get::<i32, _>("ordinal") as usize].selection;
                    body = json!({"call_id":id,"result_ref":id,"document":{"data":data,"source":{"name":source["name"],"api_slug":source["api_slug"],"endpoint_id":source["candidate_id"],"url":sources[call.get::<i32,_>("ordinal") as usize].service.as_ref().and_then(|s|s.source_url.as_deref()).map(Value::from).unwrap_or_else(||source["resource_url"].clone())},"retrieved_at":Utc::now()},"charged_micro_usd":amount.to_string()});
                    if workflow.processing == crate::monitoring::VERSION {
                        body["document"]["source"]["capability"]=source["capability"].clone();
                        body["document"]["retrieved_at"]=json!(retrieved_at);
                        if source["capability"]=="company.branch-profile" {body["document"]["source"]["establishment_number"]=source["metadata"]["inputs"]["vestigingsnummer"].clone();}
                    }
                    if let Some(notice) = sources[call.get::<i32, _>("ordinal") as usize]
                        .service
                        .as_ref()
                        .and_then(|s| s.usage_notice_view())
                    {
                        body["document"]["source"]["usage_notice"] = notice;
                    }
                    "succeeded"
                } else {
                    "unresolved"
                }
            }
            Some((ref status, _, _)) if status == "refunded" => "failed",
            None if crate::billing::refused_before_reservation(&body) => "failed",
            _ => "unresolved",
        };
        failed |= final_state != "succeeded";
        unresolved |= final_state == "unresolved";
        if final_state != "succeeded" {
            body = failed_call(id, final_state, &body, refused_without_charge);
        }
        sqlx::query("update billing.recurring_calls set state=$2,response=$3 where id=$1")
            .bind(id)
            .bind(final_state)
            .bind(&body)
            .execute(&e.store.pool)
            .await?;
        if final_state == "succeeded" {
            prior.push(recipe::StepResult {
                ordinal,
                source_selection_id: workflow.recipe.steps[ordinal].source_selection_id,
                body: json!({"ok":true,"result":body["document"]["data"]}),
            });
        }
        results.push(body);
        if failed || unresolved {
            break;
        }
    }
    let mut execution_error = if failed || unresolved {
        results.last().and_then(|result| result["message"].as_str()).map(str::to_owned)
    } else { None };
    let mut result = if workflow.processing == crate::monitoring::VERSION {
        let configuration: Value = o.get("configuration");
        let company = serde_json::from_value(configuration["monitoring"]["company"].clone())?;
        // The queue serializes one order. Failed lookups never erase the last
        // valid observations, and every retry reuses its saved source receipts.
        let previous: Option<Value> = sqlx::query_scalar("select result from billing.recurring_runs where order_id=$1 and id<>$2 and scheduled_at<$3 and state='succeeded' and finished_at is not null and result->'monitoring' is not null order by scheduled_at desc limit 1")
            .bind(order).bind(run).bind(scheduled).fetch_optional(&e.store.pool).await?;
        let value = crate::monitoring::result(&company, &configuration["monitoring"]["baseline"], previous.as_ref(), results, scheduled, &question, &workflow.fingerprint);
        if value["monitoring"]["coverage"].as_array().is_none_or(|rows|rows.iter().any(|c|c["status"] != "checked")) {
            failed = true;
            if execution_error.is_none() {
                execution_error = Some("One or more selected company sources did not return complete coverage for the approved identity. The previous verified baseline is retained; missing results are not clearance.".into());
            }
        }
        value
    } else { processing::result(&question, scheduled, results, &workflow.fingerprint) };
    if failed || unresolved {
        result["analysis"] = Value::Null;
        result["partial"] = json!(true);
    }
    finish(
        e,
        run,
        if unresolved {
            "unresolved"
        } else if failed {
            "failed"
        } else {
            "succeeded"
        },
        result,
        execution_error.as_deref(),
    )
    .await
}

pub(super) async fn deliver(e: &Engine) -> anyhow::Result<()> {
    let owners = allowed_owners();
    if !enabled() || owners.is_empty() {
        return Ok(());
    }
    // A crash after the send is ambiguous. Do not automatically duplicate mail.
    sqlx::query("update billing.recurring_deliveries d set state='uncertain',error='Delivery outcome unknown; automatic retry stopped.' where state='sending' and next_attempt_at<now()-interval '5 minutes' and exists(select 1 from billing.recurring_runs r join billing.recurring_orders o on o.id=r.order_id where r.id=d.run_id and o.owner_id=any($1::uuid[]))").bind(&owners).execute(&e.store.pool).await?;
    // Lock the order before the delivery, matching cancellation. Once claimed,
    // this send is in flight; cancellation suppresses all still-queued sends.
    let row=sqlx::query("with candidate as materialized (select o.id from billing.recurring_orders o join billing.recurring_runs r on r.order_id=o.id join billing.recurring_deliveries d on d.run_id=r.id where o.owner_id=any($1::uuid[]) and o.state<>'cancelled' and d.state='queued' and d.next_attempt_at<=now() order by d.next_attempt_at for update of o skip locked limit 1), due as (select d.id from billing.recurring_deliveries d join billing.recurring_runs r on r.id=d.run_id join candidate o on o.id=r.order_id where d.state='queued' and d.next_attempt_at<=now() order by d.next_attempt_at for update of d skip locked limit 1) update billing.recurring_deliveries d set state='sending',attempts=attempts+1,next_attempt_at=now() from due where d.id=due.id returning d.id,d.run_id,d.attempts")
  .bind(&owners).fetch_optional(&e.store.pool).await?;
    let Some(row) = row else { return Ok(()) };
    let id: Uuid = row.get("id");
    let run: Uuid = row.get("run_id");
    let attempts: i32 = row.get("attempts");
    let row=sqlx::query("select o.id,o.owner_id,o.workspace_id,o.question,o.configuration,o.delivery,o.workflow,r.result,r.state as run_state,r.error as run_error from billing.recurring_orders o join billing.recurring_runs r on r.order_id=o.id where r.id=$1").bind(run).fetch_one(&e.store.pool).await?;
    let p = Principal {
        owner: row.get("owner_id"),
        workspace: row.get("workspace_id"),
        connection: None,
    };
    let workflow: LockedWorkflow = serde_json::from_value(
        row.get::<Option<Value>, _>("workflow")
            .ok_or_else(|| anyhow::anyhow!("Missing frozen delivery."))?,
    )?;
    let delivery: Delivery = serde_json::from_value(row.get("delivery"))?;
    let configuration: Value = row.get("configuration");
    if validate_self_delivery(e, p.owner, &delivery, &workflow.delivery, configuration["monitoring"].is_object())
        .await
        .is_err()
    {
        sqlx::query("update billing.recurring_deliveries set state='failed',error='The delivery destination changed or is unavailable. Saved results remain available.' where id=$1 and state='sending'").bind(id).execute(&e.store.pool).await?;
        return Ok(());
    }
    let question: String = row.get("question");
    let result: Value = row.get("result");
    let url = format!(
        "{}/{}?{}={}",
        e.app_url,
        if workflow.processing == crate::monitoring::VERSION {"monitoring"}else{"recurring-orders"},
        if workflow.processing == crate::monitoring::VERSION {"monitor"}else{"order"},
        row.get::<Uuid, _>("id")
    );
    let limitations = result["analysis"]["limitations"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let summary = if row.get::<String, _>("run_state") != "succeeded" {
        format!("The scheduled sequence did not complete. {} Completed source results, if any, remain saved.",row.get::<Option<String>,_>("run_error").unwrap_or_default())
    } else if workflow.processing == crate::monitoring::VERSION {
        crate::monitoring::summary(&result)
    } else {
        crate::integrations::report::summary(&result)
    };
    let usage_notices = result["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let notice = &entry["document"]["source"]["usage_notice"];
            notice["text"]
                .as_str()
                .map(|text| match notice["url"].as_str() {
                    Some(url) => format!("{text}\n{url}"),
                    None => text.to_owned(),
                })
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join("\n\n");
    let alert = &configuration["monitoring"]["alert"];
    let alert_title = alert["title"].as_str().unwrap_or(&question);
    let alert_message = alert["message"].as_str().unwrap_or(&question);
    let text = format!(
        "{}\n\n{}\n{}\n\n{}\n\nFull saved results: {}",
        alert_message.chars().take(500).collect::<String>(),
        summary.chars().take(8000).collect::<String>(),
        limitations.chars().take(2000).collect::<String>(),
        usage_notices,
        url
    );
    // Provider data and the question are text, never Slack mention commands.
    let text = if workflow.delivery.toolkit == "slack" {
        text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
    } else { text };
    let outcome = if e.store.check_workspace(&p).await.is_err() || !enabled_for(p.owner) {
        None
    } else {
        match crate::integrations::configured() {
            Some(c) => Some(
                c.execute_frozen(
                    p.owner,
                    &workflow.delivery,
                    &workflow
                        .delivery
                        .arguments(&alert_title.chars().take(180).collect::<String>(), &text),
                )
                .await,
            ),
            None => None,
        }
    };
    let provider_log = outcome
        .as_ref()
        .and_then(|o| o.as_ref().ok())
        .and_then(|o| o.log_id.clone());
    let (state, error) = match outcome {
        Some(Ok(out)) if out.successful => ("sent", None),
        Some(Ok(_)) => (
            "failed",
            Some("Delivery was refused. The paid results remain saved."),
        ),
        None => (
            "failed",
            Some("Delivery access is unavailable. The paid results remain saved."),
        ),
        Some(Err(crate::integrations::ExecuteError::Provider(
            crate::integrations::ComposioError::Status(429),
        ))) if attempts < 5 => (
            "queued",
            Some("Delivery rate limited; retrying the saved result."),
        ),
        Some(Err(crate::integrations::ExecuteError::AccountUnavailable | crate::integrations::ExecuteError::InvalidContract | crate::integrations::ExecuteError::InvalidSchema | crate::integrations::ExecuteError::SchemaArguments | crate::integrations::ExecuteError::InvalidArguments | crate::integrations::ExecuteError::UnknownAction)) => ("failed",Some("The approved delivery contract or account is unavailable. Review a new plan; no other destination was used.")),
        Some(Err(_)) => (
            "uncertain",
            Some(
                "Delivery outcome is unknown. The paid results remain saved; no automatic resend.",
            ),
        ),
    };
    sqlx::query("update billing.recurring_deliveries d set state=case when $2='queued' and exists(select 1 from billing.recurring_runs r join billing.recurring_orders o on o.id=r.order_id where r.id=d.run_id and o.state='cancelled') then 'cancelled' else $2 end,error=$3,provider_log_id=$4,sent_at=case when $2='sent' then now() else null end,next_attempt_at=case when $2='queued' then now()+make_interval(secs=>least(3600,power(2,attempts)*30)::int) else next_attempt_at end where id=$1").bind(id).bind(state).bind(error).bind(provider_log).execute(&e.store.pool).await?;
    Ok(())
}

async fn refill(e: &Engine) -> anyhow::Result<()> {
    let owners = allowed_owners();
    if !enabled() || owners.is_empty() {
        return Ok(());
    }
    let orders=sqlx::query("select id,owner_id,configuration from billing.recurring_orders o where owner_id=any($1::uuid[]) and state='active' and billing_mode='per_run' and workflow is not null and not exists(select 1 from billing.recurring_runs r where r.order_id=o.id and r.finished_at is null and r.scheduled_at>now()+interval '7 days') order by updated_at limit 100").bind(&owners).fetch_all(&e.store.pool).await?;
    for order in orders {
        let id: Uuid = order.get("id");
        let schedule: Schedule =
            serde_json::from_value(order.get::<Value, _>("configuration")["schedule"].clone())?;
        let today: NaiveDate = sqlx::query_scalar("select (now() at time zone $1)::date")
            .bind(&schedule.timezone)
            .fetch_one(&e.store.pool)
            .await?;
        let calendar = schedule.dates(today, 32)?;
        let dates = utc_dates(e, &schedule, &calendar).await?;
        let mut tx = e.store.pool.begin().await?;
        let active: bool = sqlx::query_scalar(
            "select state='active' from billing.recurring_orders where id=$1 for update",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if active && enabled_for(order.get("owner_id")) {
            sqlx::query("insert into billing.recurring_runs(order_id,scheduled_at) select $1,unnest($2::timestamptz[]) on conflict(order_id,scheduled_at) do nothing").bind(id).bind(dates).execute(&mut *tx).await?;
        }
        tx.commit().await?;
    }
    Ok(())
}

#[cfg(test)]
mod manual_failure_tests {
    use super::*;

    #[test]
    fn refusal_before_purchase_preserves_safe_authorization_diagnostics() {
        let error = failed_call(Uuid::nil(), "failed", &json!({"ok":false,"error":"unauthorized","stage":"validating","secret":"never expose"}), true);
        assert_eq!(error["code"], "recurring_execution_unauthorized");
        assert!(error["message"].as_str().unwrap().contains("No source purchase"));
        assert!(!error.to_string().contains("never expose"));
    }

    #[test]
    fn unknown_payment_outcomes_never_claim_that_nothing_was_purchased() {
        let error = failed_call(Uuid::nil(), "unresolved", &json!({"error":"unauthorized"}), false);
        assert_eq!(error["code"], "recurring_outcome_unresolved");
        assert!(error["message"].as_str().unwrap().contains("not retried"));
    }
}
