//! Stable source snapshots. No model, prompt, source discovery or dynamic
//! presentation choice runs while repeating an approved source_snapshot_v1.
use serde_json::{json, Value};

pub const VERSION: &str = "source_snapshot_v1";

pub fn result(
    question: &str,
    scheduled: chrono::DateTime<chrono::Utc>,
    sources: Vec<Value>,
    workflow_hash: &str,
) -> Value {
    // Compare provider data only, in approved source order. Our generated call
    // IDs, receipt IDs and retrieval timestamps must not cause false changes.
    // Provider fields are intentionally kept: silently ignoring a source field
    // could hide a real change. More selective comparisons need a new mandate.
    let data: Vec<_> = sources
        .iter()
        .map(|s| {
            json!({
                "source":s["document"]["source"]["api_slug"],
                "endpoint":s["document"]["source"]["endpoint_id"],
                "data":s["document"]["data"],
            })
        })
        .collect();
    let observations: Vec<_> = sources.iter().filter(|s|s["document"]["data"]!=Value::Null).map(|s| json!({
        "text":format!("Saved source result: {}",s["document"]["source"]["name"].as_str().unwrap_or("Source")),
        "evidence":[{"result_ref":s["result_ref"],"pointer":"/source/name","value":s["document"]["source"]["name"]}]
    })).collect();
    json!({"question":question,"scheduled_at":scheduled,"workflow_hash":workflow_hash,
        "processing":VERSION,"comparison_hash":crate::state::digest(&data),"sources":sources,
        "analysis":{"status":"complete","method":VERSION,"observations":observations,"limitations":[],"partial":false}})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(id: &str, data: Value) -> Value {
        json!({"result_ref":id,"document":{"retrieved_at":id,"source":{"name":"Registry","api_slug":"registry","endpoint_id":"profile"},"data":data}})
    }
    #[test]
    fn receipts_and_retrieval_times_do_not_count_as_source_changes() {
        let a = result(
            "Q",
            chrono::Utc::now(),
            vec![source("a", json!({"status":"active","count":0}))],
            "hash",
        );
        let b = result(
            "Q",
            chrono::Utc::now(),
            vec![source("b", json!({"count":0,"status":"active"}))],
            "hash",
        );
        assert_eq!(a["comparison_hash"], b["comparison_hash"]);
        let c = result(
            "Q",
            chrono::Utc::now(),
            vec![source("b", json!({"count":1,"status":"active"}))],
            "hash",
        );
        assert_ne!(a["comparison_hash"], c["comparison_hash"]);
    }
    #[test]
    fn keeps_null_false_zero_and_approved_array_order() {
        let a = result(
            "Q",
            chrono::Utc::now(),
            vec![source("a", json!([null, false, 0]))],
            "hash",
        );
        let b = result(
            "Q",
            chrono::Utc::now(),
            vec![source("a", json!([0, false, null]))],
            "hash",
        );
        assert_ne!(a["comparison_hash"], b["comparison_hash"]);
        assert_eq!(a["sources"][0]["document"]["data"], json!([null, false, 0]));
    }
}
