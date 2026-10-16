//! Shared content summaries for every connected-app action and recurring delivery.
//! No additional discovery, model call, or paid source execution.
use serde_json::Value;

pub fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    format!(
        "{}…",
        text.chars()
            .take(limit.saturating_sub(1))
            .collect::<String>()
    )
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.trim().is_empty() => {
            Some(s.split_whitespace().collect::<Vec<_>>().join(" "))
        }
        Value::Bool(_) | Value::Number(_) => Some(value.to_string()),
        _ => None,
    }
}

fn first(value: &Value, paths: &[&str]) -> Option<String> {
    paths
        .iter()
        .find_map(|path| value.pointer(path).and_then(scalar))
}

fn record(value: &Value) -> String {
    if let Some(text) = scalar(value) {
        return clip(&text, 500);
    }
    let title = first(
        value,
        &["/aanbestedingNaam", "/title", "/headline", "/name"],
    );
    let description = first(
        value,
        &[
            "/opdrachtBeschrijving",
            "/summary",
            "/description",
            "/snippet",
            "/text",
            "/content",
            "/answer",
        ],
    );
    let mut lines = Vec::new();
    if let Some(title) = title {
        lines.push(clip(&title, 180));
    }
    for (label, paths) in [
        ("Buyer", &["/opdrachtgeverNaam"][..]),
        ("Notice", &["/typePublicatie/omschrijving"][..]),
        (
            "Published",
            &["/publicatieDatum", "/published_at", "/publishedAt"][..],
        ),
        ("Deadline", &["/sluitingsDatum", "/deadline"][..]),
    ] {
        if let Some(text) = first(value, paths) {
            lines.push(format!("{label}: {}", clip(&text, 180)));
        }
    }
    if let Some(description) = description {
        lines.push(clip(&description, 240));
    }
    {
        // Keep substantive fields even when the object also has a name/title.
        // A company name must not hide its status, registration or financials.
        // Unfamiliar schemas still expose actual facts, rather than a receipt.
        fn facts(value: &Value, prefix: &str, depth: usize, lines: &mut Vec<String>) {
            if lines.len() >= 10 {
                return;
            }
            if let Some(text) = scalar(value) {
                lines.push(format!("{prefix}: {}", clip(&text, 240)));
            } else if depth < 4 {
                if let Some(object) = value.as_object() {
                    for (key, child) in object {
                        if depth == 0
                            && [
                                "aanbestedingNaam",
                                "title",
                                "headline",
                                "name",
                                "opdrachtBeschrijving",
                                "summary",
                                "description",
                                "snippet",
                                "text",
                                "content",
                                "answer",
                                "opdrachtgeverNaam",
                                "typePublicatie",
                                "publicatieDatum",
                                "published_at",
                                "publishedAt",
                                "sluitingsDatum",
                                "deadline",
                                "link",
                                "url",
                            ]
                            .contains(&key.as_str())
                            && scalar(child).is_some()
                        {
                            continue;
                        }
                        if depth == 0 && key == "link" && child["href"].is_string() {
                            continue;
                        }
                        if depth == 0
                            && key == "typePublicatie"
                            && child["omschrijving"].is_string()
                        {
                            continue;
                        }
                        facts(
                            child,
                            &if prefix.is_empty() {
                                key.clone()
                            } else {
                                format!("{prefix} / {key}")
                            },
                            depth + 1,
                            lines,
                        );
                    }
                } else if let Some(items) = value.as_array() {
                    for (index, child) in items.iter().take(3).enumerate() {
                        facts(child, &format!("{prefix} {}", index + 1), depth + 1, lines);
                    }
                }
            }
        }
        facts(value, "", 0, &mut lines);
    }
    if let Some(url) = first(value, &["/link/href", "/url", "/link"]) {
        if url.starts_with("https://") || url.starts_with("http://") {
            lines.push(clip(&url, 400));
        }
    }
    if lines.is_empty() {
        "The source returned no readable content.".to_owned()
    } else {
        clip(&lines.join(" · "), 560)
    }
}

pub fn source_summary(source: &Value) -> String {
    let document = source.get("document").unwrap_or(source);
    let name = document["source"]["name"]
        .as_str()
        .or_else(|| document["source"]["provider"].as_str())
        .unwrap_or("Source");
    let original = document
        .get("data")
        .or_else(|| document.get("preview_data"))
        .unwrap_or(&Value::Null);
    let mut data = original;
    for _ in 0..4 {
        // Only unwrap conventional transport containers, preserving actual records.
        if data.as_object().is_some_and(|object| {
            object.keys().all(|key| {
                [
                    "data",
                    "result",
                    "response",
                    "payload",
                    "status",
                    "success",
                    "successful",
                    "meta",
                    "metadata",
                    "pagination",
                ]
                .contains(&key.as_str())
            })
        }) {
            if let Some(inner) = ["data", "result", "response", "payload"]
                .iter()
                .find_map(|key| data.get(*key).filter(|v| v.is_object() || v.is_array()))
            {
                data = inner;
                continue;
            }
        }
        break;
    }
    if data.is_null() {
        return format!("{name}\nNo source data was returned.");
    }
    let rows = data.as_array().or_else(|| {
        [
            "content", "results", "items", "articles", "data", "records", "entries", "hits",
        ]
        .iter()
        .find_map(|key| data[key].as_array())
    });
    let body = if let Some(rows) = rows {
        if rows.is_empty() {
            "No results in the returned page.".to_owned()
        } else {
            let count = rows.len().min(5);
            let total = data["totalElements"]
                .as_u64()
                .or_else(|| data["total"].as_u64());
            let mut text = match total {
                Some(total) => format!(
                    "{total} results reported by the source. Showing {count} of {} returned items.",
                    rows.len()
                ),
                None => format!("{} returned items. Showing {count}.", rows.len()),
            };
            for (index, row) in rows.iter().take(count).enumerate() {
                text.push_str(&format!("\n\n{}. {}", index + 1, record(row)));
            }
            text
        }
    } else {
        record(data)
    };
    format!("{name}\n{body}")
}

pub fn summary(result: &Value) -> String {
    let sources = result["sources"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if sources.is_empty() {
        return "No readable source results were returned.".to_owned();
    }
    // Divide the delivery budget so a large first source cannot hide the rest.
    let budget = 3200 / sources.len().max(1);
    sources
        .iter()
        .map(|source| {
            let text = source_summary(source);
            if text.chars().count() > budget {
                format!(
                    "{}\nSummary shortened; remaining details are in the full report.",
                    clip(&text, budget.saturating_sub(80))
                )
            } else {
                text
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Compact content for all action toolkits; no destination-specific fields or
/// recipients are inferred here. The normal proposal schema remains authoritative.
pub fn action_summary(context: &Value) -> Option<String> {
    if context.is_null() {
        return None;
    }
    if context.get("sources").is_some_and(Value::is_array) {
        return Some(summary(context));
    }
    let rows = context
        .as_array()
        .or_else(|| context.get("results").and_then(Value::as_array));
    if let Some(rows) = rows {
        let sources: Vec<_> = rows
            .iter()
            .map(|row| {
                let document = row.get("document").unwrap_or(row);
                if document.get("source").is_some()
                    && (document.get("data").is_some() || document.get("preview_data").is_some())
                {
                    row.clone()
                } else {
                    serde_json::json!({"document":{"source":{"name":"Results"},"data":row}})
                }
            })
            .collect();
        return Some(summary(&serde_json::json!({"sources":sources})));
    }
    if context.get("document").is_some() || context.get("source").is_some() {
        return Some(clip(&source_summary(context), 3200));
    }
    Some(clip(&record(context), 3200))
}

/// Same proposal context for all installed apps, including structured outputs.
pub fn action_input(question: &str, action: &super::AppAction, results: &Value) -> Value {
    serde_json::json!({"question":question,"results":results,"content_summary":action_summary(results),
        "action":{"slug":action.slug,"name":action.name,"description":action.description,"input_schema":action.input_schema}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn source(data: Value) -> Value {
        json!({"document":{"source":{"name":"TenderNed"},"data":data}})
    }
    #[test]
    fn summarizes_notice_content_not_a_receipt_and_preserves_notice_type() {
        let text = source_summary(&source(json!({"totalElements":37,"content":[{
            "aanbestedingNaam":"Glasbewassing","opdrachtgeverNaam":"ROC Mondriaan",
            "opdrachtBeschrijving":"Glas- en gevelbewassing voor onderwijslocaties.",
            "typePublicatie":{"omschrijving":"Aankondiging gegunde opdracht"},
            "sluitingsDatum":"2026-12-18T12:00:00","link":{"href":"https://www.tenderned.nl/444775"}
        }]})));
        for value in [
            "37 results",
            "Glasbewassing",
            "ROC Mondriaan",
            "Glas- en gevelbewassing",
            "Aankondiging gegunde opdracht",
            "2026-12-18",
            "https://www.tenderned.nl/444775",
        ] {
            assert!(text.contains(value), "{text}");
        }
        assert!(!text.contains("Saved source result"));
    }
    #[test]
    fn empty_missing_generic_and_long_results_are_honest() {
        assert!(source_summary(&source(json!({"content":[]})))
            .contains("No results in the returned page"));
        assert!(source_summary(&source(Value::Null)).contains("No source data"));
        let text = source_summary(&source(json!({"active":false,"count":0})));
        assert!(text.contains("active: false") && text.contains("count: 0"));
        let text = summary(
            &json!({"sources":[source(json!({"content":vec![json!({"title":"News", "description":"é".repeat(2000)});20]}))]}),
        );
        assert!(text.contains("Showing 5") && text.chars().count() < 8000);
    }
    #[test]
    fn every_connected_app_receives_the_findings_and_its_own_schema() {
        let results = serde_json::json!({"sources":[
            {"document":{"source":{"name":"Company registry"},"data":{"name":"Example BV","status":"active","registration_number":"12345678","employees":0}}},
            {"document":{"source":{"name":"Exchange rates"},"data":{"base":"EUR","rates":{"USD":1.08},"date":"2026-10-16"}}}
        ]});
        for (toolkit, name) in super::super::TOOLKITS {
            let schema = json!({"type":"object","properties":{"blocks":{"type":"array"}},"required":["blocks"]});
            let action = super::super::AppAction {
                slug: format!("app.{toolkit}.write"),
                name: name.into(),
                description: "Save findings".into(),
                domain: "app",
                toolkit,
                tool: "WRITE".into(),
                inputs: vec![],
                outputs: vec![],
                input_schema: schema.clone(),
            };
            let input = action_input("Share these findings", &action, &results);
            let text = input["content_summary"].as_str().unwrap();
            for fact in [
                "Example BV",
                "active",
                "12345678",
                "employees: 0",
                "EUR",
                "USD: 1.08",
                "2026-10-16",
            ] {
                assert!(text.contains(fact), "{toolkit}: missing {fact}: {text}");
            }
            assert_eq!(input["action"]["input_schema"], schema);
            assert_eq!(input["results"], results);
            assert!(
                input.get("arguments").is_none(),
                "content never invents recipients or destination fields"
            );
        }
    }

    #[test]
    fn supports_wrapped_news_and_generic_records_without_losing_values() {
        let text = source_summary(&source(
            json!({"status":"ok","data":{"results":[{"title":"Quarterly earnings","description":"Revenue grew 8%.","revenue":1234,"currency":"EUR","profitable":false}]}}),
        ));
        for fact in [
            "Quarterly earnings",
            "Revenue grew 8%.",
            "revenue: 1234",
            "currency: EUR",
            "profitable: false",
        ] {
            assert!(text.contains(fact), "{text}");
        }
        let text = source_summary(&source(
            json!({"records":[{"custom_metric":12,"unit":"kg"}]}),
        ));
        assert!(text.contains("custom_metric: 12") && text.contains("unit: kg"));
    }

    #[test]
    fn accepts_app_documents_and_previews_and_does_not_invent_missing_results() {
        assert!(action_summary(&Value::Null).is_none());
        let document = json!({"source":{"name":"Registry"},"preview_data":{"name":"Example BV","status":"dissolved"}});
        let single = action_summary(&document).unwrap();
        assert!(single.contains("Example BV") && single.contains("dissolved"));
        let batch = action_summary(
            &json!({"results":[document,{"source":{"name":"News"},"data":{"items":[]}}]}),
        )
        .unwrap();
        assert!(
            batch.contains("Registry")
                && batch.contains("News")
                && batch.contains("No results in the returned page")
        );
    }
}
