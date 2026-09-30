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

async fn pass(e: &Arc<Engine>) -> anyhow::Result<()> {
    let lease = Uuid::new_v4();
    let row=sqlx::query("with due as (select r.id from billing.recurring_runs r join billing.recurring_orders o on o.id=r.order_id where ((r.state='queued' and r.scheduled_at<=now() and (o.state='active' or r.scheduled_at<now()-interval '1 hour')) or (r.state='running' and r.lease_until<now())) order by r.scheduled_at for update of r skip locked limit 1) update billing.recurring_runs r set state='running',lease_id=$1,lease_until=now()+interval '3 minutes' from due where r.id=due.id returning r.id,r.order_id,r.scheduled_at")
  .bind(lease).fetch_optional(&e.store.pool).await?;
    let Some(row) = row else { return Ok(()) };
    let run: Uuid = row.get("id");
    let order: Uuid = row.get("order_id");
    let job = execute(e, run, order, row.get("scheduled_at"));
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
    let o=sqlx::query("select owner_id,workspace_id,state,sources,question,timezone from billing.recurring_orders where id=$1").bind(order).fetch_one(&e.store.pool).await?;
    let p = Principal {
        owner: o.get("owner_id"),
        workspace: o.get("workspace_id"),
        connection: None,
    };
    let sources: Vec<Source> = serde_json::from_value(o.get("sources"))?;
    let count: i64 =
        sqlx::query_scalar("select count(*) from billing.recurring_calls where run_id=$1")
            .bind(run)
            .fetch_one(&e.store.pool)
            .await?;
    if count == 0 {
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
        let ids: Vec<Uuid> = sources
            .iter()
            .filter_map(|s| s.service.as_ref().map(|s| s.id))
            .collect();
        let catalog = e.catalog.fresh_execution(&ids).await?;
        for source in &sources {
            if let Some(saved) = &source.service {
                if !catalog
                    .services
                    .iter()
                    .any(|s| s.id == saved.id && digest(s) == digest(saved))
                {
                    sqlx::query("update billing.recurring_orders set state='attention' where id=$1 and state='active'").bind(order).execute(&e.store.pool).await?;
                    return finish(e,run,"failed",Value::Null,Some("A source contract or price changed. Review the order before scheduling again.")).await;
                }
            }
        }
        let mut tx = e.store.pool.begin().await?;
        // Serialize preparation with cancellation. No external call happens here.
        let state: String =
            sqlx::query_scalar("select state from billing.recurring_orders where id=$1 for update")
                .bind(order)
                .fetch_one(&mut *tx)
                .await?;
        if state != "active" {
            tx.rollback().await?;
            return finish(e, run, "cancelled", Value::Null, None).await;
        }
        let local: NaiveDate = sqlx::query_scalar("select ($1::timestamptz at time zone $2)::date")
            .bind(scheduled)
            .bind(o.get::<String, _>("timezone"))
            .fetch_one(&mut *tx)
            .await?;
        for (ordinal, source) in sources.iter().enumerate() {
            let s = &source.selection;
            let selection = Uuid::new_v4();
            let mut meta = s["metadata"].clone();
            meta["inputs"] = super::dates::materialize(
                &s["metadata"]["inputs"],
                &source.date_rules,
                scheduled,
                local,
            )?;
            let inputs = meta["inputs"].clone();
            meta["recurring_run_id"] = json!(run);
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
            sqlx::query("insert into billing.recurring_calls(run_id,ordinal,selection_id,inputs,cap_micro_usd) values($1,$2,$3,$4,$5)").bind(run).bind(ordinal as i32).bind(selection).bind(inputs).bind(s["charged_micro_usd"].as_i64()).execute(&mut *tx).await?;
        }
        tx.commit().await?;
    }
    let calls=sqlx::query("select id,ordinal,state,response from billing.recurring_calls where run_id=$1 order by ordinal").bind(run).fetch_all(&e.store.pool).await?;
    let mut results = Vec::new();
    let mut failed = false;
    let mut unresolved = false;
    for call in calls {
        let id: Uuid = call.get("id");
        let state: String = call.get("state");
        if matches!(state.as_str(), "succeeded" | "failed" | "unresolved") {
            failed |= state != "succeeded";
            unresolved |= state == "unresolved";
            results.push(
                call.get::<Option<Value>, _>("response")
                    .unwrap_or(Value::Null),
            );
            continue;
        }
        let mut body = Value::Null;
        if state == "pending" {
            let active: bool = sqlx::query_scalar(
                "select state='active' from billing.recurring_orders where id=$1",
            )
            .bind(order)
            .fetch_one(&e.store.pool)
            .await?;
            if !active {
                failed = true;
                break;
            }
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
        let final_state = match charge {
            Some((ref status, amount, charge)) if status == "charged" => {
                let saved:Option<Value>=sqlx::query_scalar("select output from call_runs where charge_id=$1 and user_id=$2 and status='succeeded' order by started_at desc limit 1").bind(charge).bind(p.owner).fetch_optional(&e.store.pool).await?;
                let data = body.get("result").cloned().or(saved);
                if let Some(data) = data {
                    let source = &sources[call.get::<i32, _>("ordinal") as usize].selection;
                    body = json!({"call_id":id,"result_ref":id,"document":{"data":data,"source":{"name":source["name"],"api_slug":source["api_slug"],"endpoint_id":source["candidate_id"],"url":sources[call.get::<i32,_>("ordinal") as usize].service.as_ref().and_then(|s|s.source_url.as_deref()).map(Value::from).unwrap_or_else(||source["resource_url"].clone())},"retrieved_at":Utc::now()},"charged_micro_usd":amount.to_string()});
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
            body = json!({"call_id":id,"state":final_state,"message":if final_state=="unresolved"{"Payment or result needs reconciliation. This call was not retried."}else{"The source did not complete. Check the saved charge status."}});
        }
        sqlx::query("update billing.recurring_calls set state=$2,response=$3 where id=$1")
            .bind(id)
            .bind(final_state)
            .bind(&body)
            .execute(&e.store.pool)
            .await?;
        results.push(body);
        if unresolved {
            break;
        }
    }
    let documents: Vec<Value> = results
        .iter()
        .filter(|v| v.get("document").is_some())
        .map(|v| json!({"result_ref":v["result_ref"],"document":v["document"]}))
        .collect();
    let failures = if failed {
        std::collections::BTreeMap::from([(
            "source".to_owned(),
            json!({"message":"Some scheduled sources did not complete."}),
        )])
    } else {
        Default::default()
    };
    let analysis = if !unresolved && !documents.is_empty() {
        e.parser
            .analyze(
                &json!([o.get::<String, _>("question")]),
                &documents,
                &failures,
                &[],
            )
            .await
            .ok()
    } else {
        None
    };
    finish(e,run,if unresolved{"unresolved"}else if failed{"failed"}else{"succeeded"},json!({"question":o.get::<String,_>("question"),"scheduled_at":scheduled,"sources":results,"analysis":analysis}),None).await
}

async fn deliver(e: &Engine) -> anyhow::Result<()> {
    // A crash after the send is ambiguous. Do not automatically duplicate mail.
    sqlx::query("update billing.recurring_deliveries set state='uncertain',error='Delivery outcome unknown; automatic retry stopped.' where state='sending' and next_attempt_at<now()-interval '5 minutes'").execute(&e.store.pool).await?;
    let row=sqlx::query("with due as (select id from billing.recurring_deliveries where state='queued' and next_attempt_at<=now() order by next_attempt_at for update skip locked limit 1) update billing.recurring_deliveries d set state='sending',attempts=attempts+1,next_attempt_at=now() from due where d.id=due.id returning d.id,d.run_id,d.attempts")
  .fetch_optional(&e.store.pool).await?;
    let Some(row) = row else { return Ok(()) };
    let id: Uuid = row.get("id");
    let run: Uuid = row.get("run_id");
    let attempts: i32 = row.get("attempts");
    let row=sqlx::query("select o.id,o.owner_id,o.workspace_id,o.question,o.delivery,r.result from billing.recurring_orders o join billing.recurring_runs r on r.order_id=o.id where r.id=$1").bind(run).fetch_one(&e.store.pool).await?;
    let p = Principal {
        owner: row.get("owner_id"),
        workspace: row.get("workspace_id"),
        connection: None,
    };
    let delivery: Delivery = serde_json::from_value(row.get("delivery"))?;
    let question: String = row.get("question");
    let result: Value = row.get("result");
    let url = format!("{}/?recurring={}", e.app_url, row.get::<Uuid, _>("id"));
    let observations = result["analysis"]["observations"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|v| v["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default();
    let limitations = result["analysis"]["limitations"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let summary = if observations.is_empty() {
        "Your scheduled source results are saved. Open the link to review them.".to_owned()
    } else {
        observations
    };
    let text = format!(
        "{}\n\n{}\n{}\n\nFull saved results: {}",
        question.chars().take(500).collect::<String>(),
        summary.chars().take(8000).collect::<String>(),
        limitations.chars().take(2000).collect::<String>(),
        url
    );
    let outcome = if e.store.check_workspace(&p).await.is_err() {
        None
    } else {
        match crate::integrations::configured() {
            Some(c) => Some(
                c.execute(
                    p.owner,
                    delivery.slug(),
                    &delivery.arguments(&question.chars().take(180).collect::<String>(), &text),
                )
                .await,
            ),
            None => None,
        }
    };
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
        Some(Err(_)) => (
            "uncertain",
            Some(
                "Delivery outcome is unknown. The paid results remain saved; no automatic resend.",
            ),
        ),
    };
    sqlx::query("update billing.recurring_deliveries set state=$2,error=$3,sent_at=case when $2='sent' then now() else null end,next_attempt_at=case when $2='queued' then now()+make_interval(secs=>least(3600,power(2,attempts)*30)::int) else next_attempt_at end where id=$1").bind(id).bind(state).bind(error).execute(&e.store.pool).await?;
    Ok(())
}
