//! Saved, owner/workspace-scoped Bazaar purchases. The existing App ledger
//! reserves and settles money; this adapter only records the approved offer
//! and dispatches it once. Reading or recovering a purchase never dispatches.
use super::external::{BoundInput, Offer};
use crate::{auth::Principal, engine::Engine, state::{digest, Task}, store::StoreAccessError};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Purchase {
    pub request_hash: String,
    pub source: Offer,
    pub input: BoundInput,
    pub quote_ref: Uuid,
    pub expires_at: DateTime<Utc>,
    pub execution_id: Uuid,
    pub selection_id: Uuid,
    pub approved_at: Option<DateTime<Utc>>,
    pub refusal: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Receipt>,
}

/// A result received from the authenticated Edge executor. It is used only
/// beside a matching settled ledger row, never as proof of payment itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub execution_id: Uuid,
    pub charged_micro_usd: i64,
    pub provider_micro_usd: i64,
    pub fee_micro_usd: i64,
    pub result: Value,
    pub result_truncated: bool,
}

impl Receipt {
    fn from_response(body: &Value, cap: i64) -> Option<Self> {
        if body["ok"] != true || body["status"] != "charged" || !crate::billing::valid_capture(body) {
            return None;
        }
        let charged_micro_usd = body["charged_micro_usd"].as_i64()?;
        if charged_micro_usd > cap { return None; }
        let result = body.get("result")?;
        let serialized = serde_json::to_string(result).ok()?;
        let (result, clipped) = if serialized.len() <= 512_000 {
            (result.clone(), false)
        } else {
            let mut end = 64_000;
            while !serialized.is_char_boundary(end) { end -= 1; }
            (json!({"truncated_preview":&serialized[..end],"original_bytes":serialized.len()}), true)
        };
        Some(Self {
            execution_id: body["execution_id"].as_str()?.parse().ok()?,
            charged_micro_usd,
            provider_micro_usd: body["provider_micro_usd"].as_i64()?,
            fee_micro_usd: body["fee_micro_usd"].as_i64()?,
            result,
            result_truncated: clipped || body["result_truncated"] == true,
        })
    }
    fn matches_ledger(&self, id: Uuid, charged: i64, provider: i64, fee: i64) -> bool {
        self.execution_id == id && self.charged_micro_usd == charged
            && self.provider_micro_usd == provider && self.fee_micro_usd == fee
    }
}

impl Purchase {
    fn check_approval(&self, quote: Uuid, cap: &str) -> anyhow::Result<()> {
        anyhow::ensure!(quote == self.quote_ref && cap == self.source.total_atomic.to_string(), "quote_changed");
        // A replay observes the first approval, including after quote expiry.
        anyhow::ensure!(self.approved_at.is_some() || self.expires_at > Utc::now(), "quote_expired");
        Ok(())
    }
}

/// Request UUID is also the durable purchase reference. A lost preparation
/// response can therefore be recovered without creating another quote.
pub async fn prepare(engine: &Engine, person: &Principal, id: Uuid, hash: String, source: Offer, input: BoundInput) -> anyhow::Result<Value> {
    engine.store.check_spending(person).await?;
    let purchase = Purchase { request_hash: hash.clone(), source, input,
        quote_ref: Uuid::new_v4(), expires_at: Utc::now() + chrono::Duration::minutes(10),
        execution_id: Uuid::new_v4(), selection_id: Uuid::new_v4(), approved_at: None, refusal: None, receipt: None };
    let task = Task { workspace_id: person.workspace, external_purchase: Some(purchase), ..Task::default() };
    anyhow::ensure!(task.bounded(), "input_too_large");
    sqlx::query("INSERT INTO tasks(id,owner_id,connection_id,document) VALUES($1,$2,NULL,$3) ON CONFLICT(id) DO NOTHING")
        .bind(id).bind(person.owner).bind(serde_json::to_value(task)?).execute(&engine.store.pool).await?;
    let saved = load(engine, person, id).await?;
    anyhow::ensure!(saved.request_hash == hash, "idempotency_conflict");
    view(engine, person, id).await
}

async fn load(engine: &Engine, person: &Principal, id: Uuid) -> anyhow::Result<Purchase> {
    let (_, task, connection) = engine.store.load_scoped(person, id).await?;
    anyhow::ensure!(connection.is_none(), "purchase_unavailable");
    task.external_purchase.ok_or_else(|| StoreAccessError::TaskUnavailable.into())
}

pub async fn prepared(engine: &Engine, person: &Principal, id: Uuid, hash: &str) -> anyhow::Result<Option<Value>> {
    match load(engine, person, id).await {
        Ok(saved) => {
            anyhow::ensure!(saved.request_hash == hash, "idempotency_conflict");
            Ok(Some(view(engine, person, id).await?))
        }
        Err(error) if matches!(error.downcast_ref::<StoreAccessError>(), Some(StoreAccessError::TaskUnavailable)) => Ok(None),
        Err(error) => Err(error),
    }
}

pub async fn view(engine: &Engine, person: &Principal, id: Uuid) -> anyhow::Result<Value> {
    let purchase = load(engine, person, id).await?;
    let mut result = Value::Null;
    let mut result_present = false;
    let mut result_truncated = false;
    let mut billing = Value::Null;
    let mut error = purchase.refusal.clone();
    let mut status = if purchase.approved_at.is_some() { "running" } else { "requires_approval" };
    if error.is_some() { status = "failed"; }
    if purchase.approved_at.is_some() {
        // Ledger identity belongs to this saved approval, never to a query or
        // a client-supplied result/charge id. This is a read-only recovery.
        let charge = sqlx::query("SELECT id,status,charged_micro_usd,coalesce(actual_provider_micro_usd,provider_price_micro_usd) provider_micro_usd,fee_micro_usd FROM balance_charges WHERE user_id=$1 AND idempotency_key=$2")
            .bind(person.owner).bind(purchase.execution_id.to_string()).fetch_optional(&engine.store.pool).await?;
        if let Some(charge) = charge {
            let state: String = charge.get("status");
            let charged = charge.get::<Option<i64>,_>("charged_micro_usd").unwrap_or(0);
            let provider = charge.get::<Option<i64>,_>("provider_micro_usd").unwrap_or(0);
            let fee = charge.get::<Option<i64>,_>("fee_micro_usd").unwrap_or(0);
            if state == "charged" {
                let proof = json!({"provider_micro_usd":provider,"fee_micro_usd":fee,"charged_micro_usd":charged});
                anyhow::ensure!(charged <= purchase.source.total_atomic && crate::billing::valid_capture(&proof), "payment_requires_reconciliation");
                let run = sqlx::query("SELECT output,output_truncated FROM call_runs WHERE charge_id=$1 AND user_id=$2 AND status='succeeded' ORDER BY started_at DESC LIMIT 1")
                    .bind(charge.get::<Uuid,_>("id")).bind(person.owner).fetch_optional(&engine.store.pool).await?;
                if let Some(run) = run {
                    result_present = true;
                    result = run.get::<Option<Value>,_>("output").unwrap_or(Value::Null);
                    result_truncated = run.get::<Option<bool>,_>("output_truncated").unwrap_or(false);
                } else if let Some(receipt) = purchase.receipt.as_ref().filter(|receipt|
                    receipt.matches_ledger(charge.get("id"), charged, provider, fee)) {
                    result_present = true;
                    result = receipt.result.clone();
                    result_truncated = receipt.result_truncated;
                }
                status = if result_present { "succeeded" } else { "running" };
                error = None;
                billing = json!({"status":"charged","provider_micro_usd":provider,"fee_micro_usd":fee,"charged_micro_usd":charged});
            } else if state == "refunded" {
                status = "failed";
                error = Some(json!({"code":"refunded","message":"The source could not complete the request. The full amount was refunded to your internal balance."}));
                billing = json!({"status":"refunded","provider_micro_usd":0,"fee_micro_usd":0,"charged_micro_usd":0});
            } else {
                status = "running";
                error = None;
                billing = json!({"status":state,"provider_micro_usd":null,"fee_micro_usd":null,"charged_micro_usd":null});
            }
        } else if purchase.refusal.is_some() {
            billing = json!({"status":"not_charged","provider_micro_usd":0,"fee_micro_usd":0,"charged_micro_usd":0});
        }
        if status == "running" && purchase.approved_at.is_some_and(|at| at + chrono::Duration::seconds(90) <= Utc::now()) {
            error = Some(json!({"code":"pending_reconciliation","message":if billing["status"] == "charged" {
                "Payment completed. Your saved source result is being recovered. No new purchase will be made."
            } else {
                "This purchase is still being checked. Its payment outcome is not yet confirmed; no new purchase will be made."
            }}));
        }
    }
    Ok(json!({"purchase_id":id,"status":status,"source":purchase.source,
        "quote":{"quote_ref":purchase.quote_ref,"max_total_atomic":purchase.source.total_atomic.to_string(),"currency":"USDC","expires_at":purchase.expires_at},
        "result":result,"result_present":result_present,"result_truncated":result_truncated,"billing":billing,"error":error}))
}

async fn save_receipt(engine: &Engine, person: &Principal, id: Uuid, purchase: &Purchase, body: &Value) -> anyhow::Result<()> {
    let Some(receipt) = Receipt::from_response(body, purchase.source.total_atomic) else { return Ok(()); };
    let row = sqlx::query("SELECT id,charged_micro_usd,coalesce(actual_provider_micro_usd,provider_price_micro_usd) provider_micro_usd,fee_micro_usd FROM balance_charges WHERE user_id=$1 AND idempotency_key=$2 AND status='charged'")
        .bind(person.owner).bind(purchase.execution_id.to_string()).fetch_optional(&engine.store.pool).await?;
    let Some(row) = row else { return Ok(()); };
    if !receipt.matches_ledger(row.get("id"), row.get::<Option<i64>,_>("charged_micro_usd").unwrap_or(0), row.get::<Option<i64>,_>("provider_micro_usd").unwrap_or(0), row.get::<Option<i64>,_>("fee_micro_usd").unwrap_or(0)) {
        return Ok(());
    }
    // Do not replace the quote or input, and keep the whole task bounded even
    // if source metadata is unusually large. Read-only recovery uses this
    // result when the Edge call_runs bookkeeping write failed after charging.
    sqlx::query("UPDATE tasks SET document=jsonb_set(document,'{external_purchase,receipt}',$3),updated_at=now(),revision=revision+1 WHERE id=$1 AND owner_id=$2 AND document->'external_purchase'->>'execution_id'=$4 AND octet_length(jsonb_set(document,'{external_purchase,receipt}',$3)::text)<=1048576")
        .bind(id).bind(person.owner).bind(serde_json::to_value(receipt)?).bind(purchase.execution_id.to_string()).execute(&engine.store.pool).await?;
    Ok(())
}

/// Save approval, selected offer and workspace spending cap atomically before
/// starting the existing treasury-backed Edge execution. Concurrent clicks or
/// a lost HTTP response cannot create a second dispatch.
pub async fn authorize(engine: Arc<Engine>, person: Principal, bearer: String, id: Uuid, quote: Uuid, cap: &str) -> anyhow::Result<Value> {
    anyhow::ensure!(engine.executor.enabled, "purchases_unavailable");
    engine.store.check_spending(&person).await?;
    let mut tx = engine.store.pool.begin().await?;
    let row = sqlx::query("SELECT document,connection_id FROM tasks WHERE id=$1 AND owner_id=$2 AND expires_at>now() FOR UPDATE")
        .bind(id).bind(person.owner).fetch_optional(&mut *tx).await?.ok_or(StoreAccessError::TaskUnavailable)?;
    let mut task: Task = serde_json::from_value(row.get("document"))?;
    anyhow::ensure!(task.workspace_id == person.workspace && row.get::<Option<Uuid>,_>("connection_id").is_none(), "purchase_unavailable");
    anyhow::ensure!(!task.cancelled, "purchase_cancelled");
    let purchase = task.external_purchase.as_mut().ok_or(StoreAccessError::TaskUnavailable)?;
    purchase.check_approval(quote, cap)?;
    let first = purchase.approved_at.is_none();
    if first {
        purchase.approved_at = Some(Utc::now());
        // The source, price and input are the saved server-verified offer.
        // Nothing in the approval body can replace them.
        sqlx::query("INSERT INTO user_selected_apis(id,user_id,connection_id,prompt,kind,source,name,capability,resource_url,method,provider_price_micro_usd,charged_micro_usd,metadata) VALUES($1,$2,NULL,$3,'external','coinbase-x402-bazaar',$4,'external.request',$5,$6,$7,$8,$9)")
            .bind(purchase.selection_id).bind(person.owner).bind(format!("Ask v2: {}", purchase.source.name))
            .bind(&purchase.source.name).bind(&purchase.source.resource).bind(&purchase.source.method)
            .bind(purchase.source.provider_atomic).bind(purchase.source.total_atomic)
            .bind(json!({"ask_v2":true,"purchase_id":id,"quote_ref":purchase.quote_ref,"inputs":purchase.input.inputs,"fields":purchase.source.inputs}))
            .execute(&mut *tx).await?;
        if let Some(workspace) = person.workspace {
            sqlx::query("SELECT public.workspace_bind_execution($1,NULL,$2,$3,$4,$5,$6)")
                .bind(person.owner).bind(workspace).bind(purchase.selection_id).bind(id)
                .bind(purchase.source.total_atomic).bind(purchase.expires_at).execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE tasks SET document=$3,revision=revision+1,updated_at=now() WHERE id=$1 AND owner_id=$2")
            .bind(id).bind(person.owner).bind(serde_json::to_value(&task)?).execute(&mut *tx).await?;
    }
    let saved = task.external_purchase.clone().unwrap();
    tx.commit().await?;
    if first {
        let engine = engine.clone();
        let person = person.clone();
        tokio::spawn(async move {
            let response = engine.executor.http.post(format!("{}/functions/v1/use-selection", engine.executor.supabase_url))
                .bearer_auth(bearer).timeout(Duration::from_secs(90))
                .json(&json!({"selection_id":saved.selection_id,"idempotency_key":saved.execution_id,
                    "inputs":saved.input.inputs,"input_parts":saved.input.input_parts,"deterministic":true})).send().await;
            let body = match response {
                Ok(response) => crate::parser::bounded_json(response, 4 * 1024 * 1024).await.ok(),
                Err(_) => None,
            };
            if let Some(body) = body.as_ref() {
                if let Err(error) = save_receipt(&engine, &person, id, &saved, body).await {
                    tracing::warn!(purchase=%id,error=%error,"external source receipt could not be saved");
                }
            }
            // Only a proved refusal before any reservation can be declared a
            // zero-cost failure. Every other outcome is read from the ledger.
            if let Some(body) = body.filter(crate::billing::refused_before_reservation) {
                let exists = sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM balance_charges WHERE user_id=$1 AND idempotency_key=$2)")
                    .bind(person.owner).bind(saved.execution_id.to_string()).fetch_one(&engine.store.pool).await;
                if matches!(exists, Ok(false)) {
                    let refusal = crate::billing::public_error(&body);
                    let _ = sqlx::query("UPDATE tasks SET document=jsonb_set(document,'{external_purchase,refusal}',$3),updated_at=now(),revision=revision+1 WHERE id=$1 AND owner_id=$2")
                        .bind(id).bind(person.owner).bind(refusal).execute(&engine.store.pool).await;
                }
            }
            crate::server_events::task_changed(id);
        });
    }
    view(&engine, &person, id).await
}

pub fn request_hash(person: &Principal, resource: &str, method: &str, query: &str, input: &Value) -> String {
    digest(&("ask-v2-external", person.owner, person.workspace, resource, method, query, input))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(result: Value) -> Value {
        json!({"ok":true,"status":"charged","execution_id":Uuid::new_v4(),"provider_micro_usd":100,"fee_micro_usd":15,"charged_micro_usd":115,"result":result,"result_truncated":false})
    }

    #[test]
    fn receipt_requires_a_valid_bounded_capture_and_a_result_field() {
        let body = response(Value::Null);
        let receipt = Receipt::from_response(&body, 115).unwrap();
        assert!(receipt.result.is_null(), "a returned JSON null is still a present result");
        assert!(Receipt::from_response(&body, 114).is_none());
        let mut missing = body.clone();
        missing.as_object_mut().unwrap().remove("result");
        assert!(Receipt::from_response(&missing, 115).is_none());
        let mut invalid = body.clone();
        invalid["fee_micro_usd"] = json!(0);
        assert!(Receipt::from_response(&invalid, 115).is_none());
        invalid = body;
        invalid["execution_id"] = json!("not-a-charge-id");
        assert!(Receipt::from_response(&invalid, 115).is_none());
    }

    #[test]
    fn receipt_cannot_supply_results_for_another_charge_or_amount() {
        let receipt = Receipt::from_response(&response(json!({"rows":[1]})), 115).unwrap();
        assert!(receipt.matches_ledger(receipt.execution_id, 115, 100, 15));
        assert!(!receipt.matches_ledger(Uuid::new_v4(), 115, 100, 15));
        assert!(!receipt.matches_ledger(receipt.execution_id, 114, 100, 14));
        assert!(!receipt.matches_ledger(receipt.execution_id, 115, 99, 16));
    }

    #[test]
    fn large_successful_results_remain_valid_bounded_json_with_truncation_provenance() {
        let body = response(json!({"text":"🟣".repeat(150_000)}));
        let receipt = Receipt::from_response(&body, 115).unwrap();
        assert!(receipt.result_truncated);
        assert!(receipt.result["truncated_preview"].as_str().unwrap().len() <= 64_000);
        assert!(receipt.result["original_bytes"].as_u64().unwrap() > 512_000);
        assert!(serde_json::to_vec(&receipt).unwrap().len() < 512_000);
        let mut body = response(json!({"small":"preview"}));
        body["result_truncated"] = json!(true);
        assert!(Receipt::from_response(&body, 115).unwrap().result_truncated);
    }
}
