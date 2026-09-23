//! A model chooses the answer's shape; only saved source fields supply its values.
use anyhow::{ensure, Context};
use serde_json::{json, Value};

fn label(value: &Value) -> anyhow::Result<&str> {
    let text = value.as_str().context("missing presentation label")?;
    ensure!(
        !text.trim().is_empty()
            && text.len() <= 160
            && !text.contains("://")
            && !text.contains(['\n', '\r']),
        "invalid presentation label"
    );
    Ok(text)
}

fn source<'a>(reference: &Value, sources: &'a [Value]) -> anyhow::Result<&'a Value> {
    sources
        .iter()
        .find(|s| s["result_ref"].is_string() && s["result_ref"] == reference["result_ref"])
        .context("unknown presentation source")
}

fn cell(reference: &Value, sources: &[Value]) -> anyhow::Result<Value> {
    if reference.is_null() {
        return Ok(Value::Null);
    }
    let source = source(reference, sources)?;
    let pointer = reference["pointer"]
        .as_str()
        .filter(|p| crate::report_assets::evidence_pointer(p))
        .context("invalid presentation pointer")?;
    let value = source["document"]
        .pointer(pointer)
        .context("missing presentation field")?;
    // Missing facts stay unknown, not zero or model-generated text.
    if value.is_null() {
        return Ok(Value::Null);
    }
    ensure!(
        value.is_string() || value.is_number() || value.is_boolean(),
        "presentation field must be scalar"
    );
    let value = if let Some(quote) = reference.get("quote").filter(|q| !q.is_null()) {
        let quote = quote
            .as_str()
            .filter(|q| !q.trim().is_empty() && q.len() <= 1024)
            .context("invalid presentation quote")?;
        ensure!(
            value.as_str().is_some_and(|text| text.contains(quote)),
            "presentation quote not in source"
        );
        json!(quote)
    } else {
        ensure!(
            value.as_str().is_none_or(|text| text.len() <= 2048),
            "presentation text needs exact short quote"
        );
        value.clone()
    };
    Ok(
        json!({"value":value,"result_ref":reference["result_ref"],"pointer":pointer,"source":source["document"]["source"]["name"]}),
    )
}

/// Optional for older models and saved analyses. Invalid presentation fails validation
/// just like invalid narrative evidence, allowing the existing model repair path.
pub(crate) fn validate(value: &Value, sources: &[Value]) -> anyhow::Result<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    let kind = value["kind"]
        .as_str()
        .context("missing presentation kind")?;
    ensure!(
        matches!(kind, "text" | "table" | "comparison" | "chart" | "document"),
        "unknown presentation kind"
    );
    if matches!(kind, "text" | "document") {
        return Ok(json!({"kind":kind}));
    }
    let title = label(&value["title"])?;
    let columns = value["columns"]
        .as_array()
        .context("missing presentation columns")?;
    ensure!(
        (2..=8).contains(&columns.len()),
        "invalid presentation column count"
    );
    let headers = columns
        .iter()
        .map(|c| {
            if c.get("result_ref").is_some() {
                cell(c, sources)
            } else {
                Ok(json!({"value":label(&c["label"])?}))
            }
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    ensure!(headers.iter().all(|header| !header["value"].is_null()), "missing presentation heading");
    let mut out = json!({"kind":kind,"title":title,"columns":headers});
    let rows = if let Some(pointer) = value["rows_pointer"].as_str() {
        ensure!(
            kind == "table" && pointer.starts_with("/data/"),
            "invalid presentation collection"
        );
        let source = source(value, sources)?;
        let records = source["document"]
            .pointer(pointer)
            .and_then(Value::as_array)
            .context("missing presentation records")?;
        let indices = if let Some(indices) = value.get("row_indices") {
            indices
                .as_array()
                .context("invalid selected rows")?
                .iter()
                .map(|i| {
                    i.as_u64()
                        .and_then(|i| usize::try_from(i).ok())
                        .context("invalid row index")
                })
                .collect::<anyhow::Result<Vec<_>>>()?
        } else {
            (0..records.len().min(200)).collect()
        };
        ensure!(
            indices.len() <= 200 && indices.iter().all(|i| *i < records.len()),
            "invalid selected rows"
        );
        let mut seen = std::collections::BTreeSet::new();
        ensure!(
            indices.iter().all(|i| seen.insert(*i)),
            "duplicate presentation row"
        );
        out["returned_count"] = json!(records.len());
        let mut rows = Vec::new();
        for index in indices {
            let mut row = Vec::new();
            for column in columns {
                let suffix = column["pointer"]
                    .as_str()
                    .filter(|p| p.starts_with('/'))
                    .context("missing column pointer")?;
                let full = format!("{pointer}/{index}{suffix}");
                // Heterogeneous returned records may omit a column.
                row.push(if source["document"].pointer(&full).is_none() {
                    Value::Null
                } else {
                    cell(
                        &json!({"result_ref":value["result_ref"],"pointer":full}),
                        sources,
                    )?
                });
            }
            rows.push(json!(row));
        }
        rows
    } else {
        let rows = value["rows"]
            .as_array()
            .context("missing presentation rows")?;
        ensure!(rows.len() <= 50, "too many presentation rows");
        rows.iter()
            .map(|row| {
                let cells = row.as_array().context("invalid presentation row")?;
                ensure!(
                    cells.len() == columns.len(),
                    "presentation row width mismatch"
                );
                Ok(json!(cells
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        if kind == "comparison" && i == 0 && c.get("label").is_some() {
                            Ok(json!({"value":label(&c["label"])?}))
                        } else {
                            cell(c, sources)
                        }
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?))
            })
            .collect::<anyhow::Result<Vec<_>>>()?
    };
    if kind == "chart" {
        ensure!(
            columns.len() == 2 && !rows.is_empty() && rows.len() <= 30,
            "chart requires labels and a numeric series"
        );
        ensure!(
            rows.iter()
                .all(|r| r[0]["value"].is_string() && r[1]["value"].is_number()),
            "chart requires source labels and numeric values"
        );
    }
    out["rows"] = json!(rows);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sources() -> Vec<Value> {
        vec![
            json!({"result_ref":"saved","document":{"source":{"name":"Registry"},"data":{"items":[{"name":"First","amount":12,"url":"https://example.org/a"},{"name":"Second","amount":null}],"text":"Price is EUR 12 monthly."}}}),
        ]
    }
    #[test]
    fn overview_selects_saved_rows_and_preserves_missing_values() {
        let p = json!({"kind":"table","title":"Overview","result_ref":"saved","rows_pointer":"/data/items","columns":[{"label":"Name","pointer":"/name"},{"label":"Amount","pointer":"/amount"},{"label":"Source","pointer":"/url"}]});
        let output = validate(&p, &sources()).unwrap();
        assert_eq!(output["rows"][0][1]["value"], 12);
        assert_eq!(output["rows"][0][2]["pointer"], "/data/items/0/url");
        assert!(output["rows"][1][1].is_null() && output["rows"][1][2].is_null());
        let mut selected = p;
        selected["row_indices"] = json!([1]);
        assert_eq!(
            validate(&selected, &sources()).unwrap()["returned_count"],
            2
        );
        selected["row_indices"] = json!([2]);
        assert!(validate(&selected, &sources()).is_err());
    }
    #[test]
    fn comparisons_and_charts_require_saved_values() {
        let mut p = json!({"kind":"comparison","title":"Comparison","columns":[{"label":"Feature"},{"result_ref":"saved","pointer":"/data/items/0/name"}],"rows":[[{"label":"Price"},{"result_ref":"saved","pointer":"/data/text","quote":"EUR 12 monthly"}]]});
        assert_eq!(
            validate(&p, &sources()).unwrap()["rows"][0][1]["value"],
            "EUR 12 monthly"
        );
        p["rows"][0][1]["quote"] = json!("EUR 1 monthly");
        assert!(validate(&p, &sources()).is_err());
        p["rows"][0][1] = json!({"label":"invented value"});
        assert!(validate(&p, &sources()).is_err());
        p["kind"] = json!("chart");
        p["rows"] = json!([[{"result_ref":"saved","pointer":"/data/items/0/name"},{"result_ref":"saved","pointer":"/data/items/0/amount"}]]);
        assert!(validate(&p, &sources()).is_ok());
        p["rows"][0][1]["pointer"] = json!("/data/items/1/amount");
        assert!(validate(&p, &sources()).is_err());
        p["rows"][0][1] = json!({"result_ref":"other","pointer":"/data/items/0/amount"});
        assert!(validate(&p, &sources()).is_err());
        p["kind"] = json!("table");
        p["rows"] = json!([]);
        p["columns"][0] = json!({"result_ref":"saved","pointer":"/data/items/1/amount"});
        assert!(validate(&p, &sources()).is_err());
    }
}
