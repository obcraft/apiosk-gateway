//! A durable handoff to the host's authorized email connector, not a sender.
//! Only a successful host receipt acknowledges signals. Ambiguous handoffs block
//! another dispatch; they never expire into an automatic resend.
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub fn message(snapshot: &Value) -> Value {
    let signals: Vec<_> = snapshot["new_signals"]
        .as_array()
        .into_iter()
        .flatten()
        .take(100)
        .cloned()
        .collect();
    let mut body = format!(
        "Apiosk company monitor — {}\n",
        snapshot["checked_at"].as_str().unwrap_or("")
    );
    for s in &signals {
        body.push_str(&format!(
            "\n{} (KVK {})\n{}\n",
            s["company_name"].as_str().unwrap_or(""),
            s["kvk_number"].as_str().unwrap_or(""),
            s["message"].as_str().unwrap_or("")
        ));
        if s["carried_forward"] == true {
            body.push_str("Previously observed but not yet confirmed by email; see the original source date.\n");
        }
        if s["kind"] == "registry_change" {
            body.push_str(&format!(
                "{}: {} → {}\n",
                s["identity"].as_str().unwrap_or(""),
                s["previous"],
                s["current"]
            ));
        }
        if let Some(url) = s["url"].as_str() {
            body.push_str(&format!("Source: {url}\n"));
            if let Some(date) = s["published_at"].as_str() {
                body.push_str(&format!("Published: {date}\n"));
            }
        }
        for e in s["evidence"].as_array().into_iter().flatten() {
            body.push_str(&format!(
                "Source: {} · retrieved {} · evidence {} {}\n",
                e["source"]["name"]
                    .as_str()
                    .or_else(|| e["source"]["provider"].as_str())
                    .unwrap_or("source"),
                e["retrieved_at"].as_str().unwrap_or(""),
                e["result_ref"].as_str().unwrap_or(""),
                e["pointer"].as_str().unwrap_or("")
            ));
            if let Some(url) = e["source"]["url"]
                .as_str()
                .and_then(|s| url::Url::parse(s).ok())
                .filter(|u| {
                    u.scheme() == "https" && u.username().is_empty() && u.password().is_none()
                })
            {
                body.push_str(&format!("Source information: {url}\n"));
            }
        }
    }
    let mut notices = std::collections::BTreeSet::new();
    for source in snapshot["sources"].as_array().into_iter().flatten() {
        let notice = &source["document"]["source"]["usage_notice"];
        if let Some(text) = notice["text"]
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.len() <= 2400)
        {
            if notices.insert(text) {
                body.push_str(&format!("\n{text}\n"));
            }
        }
    }
    let gaps: Vec<_> = snapshot["coverage"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["status"] != "checked")
        .collect();
    if !gaps.is_empty() {
        body.push_str("\nIncomplete coverage:\n");
    }
    for g in gaps {
        body.push_str(&format!(
            "KVK {}: {}\n",
            g["kvk_number"].as_str().unwrap_or(""),
            g["message"].as_str().unwrap_or("")
        ));
    }
    body.push_str("\nPotential sanctions matches and news mentions do not confirm identity. New means not previously reported by this monitor.\n");
    json!({"subject":format!("Apiosk: {} new company signals",signals.len()),"body":body,
        "signal_ids":signals.iter().map(|s|&s["signal_id"]).collect::<Vec<_>>(),"recipient":"self"})
}

pub async fn prepare(
    pool: &PgPool,
    id: Uuid,
    occurrence: &str,
    delivery_id: Uuid,
) -> anyhow::Result<Value> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM gateway_v2.company_checks WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    if let Some(row) = sqlx::query("SELECT delivery_id,status,payload FROM gateway_v2.company_check_deliveries WHERE check_id=$1 AND (delivery_id=$2 OR status='awaiting_confirmation') ORDER BY created_at LIMIT 1")
        .bind(id).bind(delivery_id).fetch_optional(&mut *tx).await? {
        return Ok(json!({"view":"company_check_delivery","check_id":id,"delivery_id":row.get::<Uuid,_>("delivery_id"),"status":row.get::<String,_>("status"),"dispatch_allowed":false,"message":"This handoff already exists. Reconcile its email connector result; do not resend.","payload":row.get::<Value,_>("payload")}));
    }
    let mut snapshot: Value = sqlx::query_scalar("SELECT snapshot FROM gateway_v2.company_check_runs WHERE check_id=$1 AND occurrence=$2 AND completed_at IS NOT NULL")
        .bind(id).bind(occurrence).fetch_optional(&mut *tx).await?.ok_or_else(||anyhow::anyhow!("Collect the saved report first."))?;
    let reported: Vec<String> = sqlx::query_scalar(
        "SELECT signal_id FROM gateway_v2.company_check_reported_signals WHERE check_id=$1",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    snapshot["new_signals"] = json!(snapshot["signals"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["signal_id"]
            .as_str()
            .is_some_and(|id| !reported.iter().any(|r| r == id)))
        .collect::<Vec<_>>());
    let payload = message(&snapshot);
    if payload["signal_ids"]
        .as_array()
        .is_none_or(|ids| ids.is_empty())
    {
        return Ok(
            json!({"view":"company_check_delivery","check_id":id,"occurrence":occurrence,"status":"suppressed","dispatch_allowed":false,"reason":"no_new_signals"}),
        );
    }
    sqlx::query("INSERT INTO gateway_v2.company_check_deliveries(delivery_id,check_id,occurrence,payload) VALUES($1,$2,$3,$4)").bind(delivery_id).bind(id).bind(occurrence).bind(&payload).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(
        json!({"view":"company_check_delivery","check_id":id,"occurrence":occurrence,"delivery_id":delivery_id,"status":"awaiting_confirmation","dispatch_allowed":true,"sent":false,"payload":payload,"instructions":"Send this exact plain-text message once to the user's own verified address through the host's authorized email connector. Confirm only after a successful send receipt. On timeout reconcile, never resend blindly."}),
    )
}

pub async fn confirm(
    pool: &PgPool,
    id: Uuid,
    delivery_id: Uuid,
    provider: &str,
    message_id: &str,
) -> anyhow::Result<Value> {
    anyhow::ensure!(
        [provider, message_id]
            .iter()
            .all(|s| !s.trim().is_empty() && s.len() <= 512 && !s.chars().any(char::is_control)),
        "Supply the email connector's actual successful-send receipt."
    );
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM gateway_v2.company_checks WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let row=sqlx::query("SELECT status,occurrence,payload,provider,provider_message_id FROM gateway_v2.company_check_deliveries WHERE check_id=$1 AND delivery_id=$2 FOR UPDATE").bind(id).bind(delivery_id).fetch_one(&mut *tx).await?;
    let status: String = row.get("status");
    if status == "sent" {
        anyhow::ensure!(
            row.get::<Option<String>, _>("provider").as_deref() == Some(provider)
                && row
                    .get::<Option<String>, _>("provider_message_id")
                    .as_deref()
                    == Some(message_id),
            "The delivery already has a different receipt."
        );
    } else {
        let payload: Value = row.get("payload");
        let occurrence: String = row.get("occurrence");
        for signal in payload["signal_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            sqlx::query("INSERT INTO gateway_v2.company_check_reported_signals(check_id,signal_id,occurrence) VALUES($1,$2,$3) ON CONFLICT(check_id,signal_id) DO NOTHING").bind(id).bind(signal).bind(&occurrence).execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE gateway_v2.company_check_deliveries SET status='sent',provider=$3,provider_message_id=$4,confirmed_at=now() WHERE check_id=$1 AND delivery_id=$2").bind(id).bind(delivery_id).bind(provider).bind(message_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(
        json!({"view":"company_check_acknowledgement","check_id":id,"delivery_id":delivery_id,"status":"sent","confirmation_source":"host_email_connector_receipt","sent_by_gateway":false}),
    )
}
