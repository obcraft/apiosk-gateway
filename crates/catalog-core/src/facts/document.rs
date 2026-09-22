//! Opaque source documents carry no normalized identity or financial claims.
//! This admits documented JSON retrieval without guessing undocumented fields.
use serde_json::{json, Value};

pub fn response(body: &Value) -> Option<Value> {
    // A successful HTTP envelope can still contain an upstream error. Do not
    // turn that, a pending job, or a null response into usable source evidence.
    if body.is_null() || body.is_boolean() || body.is_number()
        || body.as_str().is_some_and(|s| s.trim().is_empty())
        || body.as_object().is_some_and(|o| o.is_empty())
        || body.get("error").is_some_and(|v| !v.is_null() && v != false && v != "")
        || body.get("errors").is_some_and(|v| !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty()))
        || body["success"] == false || body["ok"] == false
        || body["status"].as_str().is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(),
            "error" | "failed" | "pending" | "queued" | "processing"))
        || body["statusCode"].as_u64().is_some_and(|s| s >= 400)
    { return None; }
    Some(json!({"document":body}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_source_documents_without_inventing_fields() {
        for body in [json!({"quarters":[{"revenue":12,"unit":"USD"}]}), json!([]), json!("Source text")] {
            assert_eq!(response(&body).unwrap()["document"], body);
        }
    }
    #[test]
    fn rejects_error_and_job_envelopes() {
        for body in [Value::Null, json!({}), json!(false), json!({"error":"unavailable"}),
            json!({"errors":["failed"]}), json!({"ok":false}), json!({"success":false}),
            json!({"status":"pending","id":"job"}), json!({"statusCode":502})] {
            assert!(response(&body).is_none(), "{body}");
        }
    }
}
