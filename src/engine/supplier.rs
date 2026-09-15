use super::*;
use crate::supplier_check::{self, Invoice};
use sqlx::Row;
use std::collections::BTreeMap;

impl Engine {
    /// Evaluate a finished supplier check from this plan's saved documents and
    /// remember the invoice identifiers for comparison with the next invoice.
    pub(super) async fn supplier_verdict(
        &self,
        p: &Principal,
        id: Uuid,
        task: &Task,
        plan: &Plan,
        check: Value,
    ) -> anyhow::Result<Value> {
        let invoice: Invoice = serde_json::from_value(check)?;
        let mut sources = Vec::new();
        for result in &task.active_results {
            let document = self.store.result(p.owner, id, *result).await?;
            // Include dependency lookups (such as a registry search) that are
            // part of this approved plan, not only the requested goals.
            if plan.steps.iter().any(|step| {
                document["source"]["endpoint_id"] == json!(step.service.endpoint_id)
                    && document["subject"]["entity_ref"] == json!(invoice.entity_ref)
            }) {
                sources.push(json!({"result_ref":result,"document":document}));
            }
        }
        let failures: BTreeMap<String, Value> = plan
            .steps
            .iter()
            .filter_map(|step| {
                task.failed_steps.get(&step.key()).map(|error| {
                    (format!("{} {}", step.service.api_slug, step.service.path), error.clone())
                })
            })
            .collect();
        let keys = invoice.keys();
        // Corrections within one conversation re-plan the same invoice; only
        // other conversations count as earlier invoices from this supplier.
        let previous: Option<Value> = if keys.is_empty() {
            None
        } else {
            sqlx::query("SELECT record,observed_at FROM supplier_invoice_checks WHERE owner_id=$1 AND workspace_id IS NOT DISTINCT FROM $2 AND supplier_keys && $3 AND task_id<>$4 AND expires_at>now() ORDER BY observed_at DESC LIMIT 1")
                .bind(p.owner).bind(task.workspace_id).bind(&keys).bind(id)
                .fetch_optional(&self.store.pool).await?
                .map(|row| {
                    let mut record: Value = row.get("record");
                    record["observed_at"] = json!(row.get::<chrono::DateTime<Utc>, _>("observed_at").to_rfc3339());
                    record
                })
        };
        let analysis = supplier_check::evaluate(&invoice, &sources, &failures, previous.as_ref(), Utc::now());
        sqlx::query("INSERT INTO supplier_invoice_checks(owner_id,workspace_id,task_id,quote_ref,supplier_keys,record,decision) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(task_id,quote_ref) DO UPDATE SET supplier_keys=EXCLUDED.supplier_keys,record=EXCLUDED.record,decision=EXCLUDED.decision")
            .bind(p.owner).bind(task.workspace_id).bind(id).bind(plan.quote_ref).bind(&keys)
            .bind(supplier_check::record(&invoice)).bind(analysis["verdict"]["decision"].as_str())
            .execute(&self.store.pool).await?;
        if let Err(error) = self.record_supplier_run(p, id, task, plan.quote_ref, &analysis).await {
            // The verdict itself is saved on the task; run history must not hide it.
            tracing::warn!(error=%error, "supplier workflow run could not be recorded");
        }
        Ok(analysis)
    }
}
