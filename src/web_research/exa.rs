//! Supplemental discovery only. All results still pass through our page reader.
use super::*;

/// Explicit Exa actions do not wait for unrelated discovery providers.
pub(super) async fn only(question: &str) -> Result<(Vec<Candidate>, Value, Value)> {
    let key = std::env::var("EXA_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("Exa search unavailable"))?;
    let data = search(question, key.trim()).await?;
    combine(Ok((vec![], json!([]), json!({"cost_usd":0,"configured_api_requests":0}))), Ok(data))
}

pub(super) async fn supplement(question: &str) -> Result<(Vec<Candidate>, Value, Value)> {
    let key = std::env::var("EXA_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty());
    let Some(key) = key else {
        return primary_search(question).await;
    };
    let (primary, extra) = tokio::join!(primary_search(question), search(question, &key));
    combine(primary, extra)
}

fn combine(
    primary: Result<(Vec<Candidate>, Value, Value)>,
    extra: Result<Value>,
) -> Result<(Vec<Candidate>, Value, Value)> {
    let (candidates, mut providers, mut accounting) = match primary {
        Ok(result) => result,
        Err(error) if extra.as_ref().ok().is_none_or(|v| parse(v).is_empty()) => return Err(error),
        Err(_) => (
            vec![],
            json!([]),
            json!({"cost_usd":null,"configured_api_requests":null}),
        ),
    };
    let requests = accounting["configured_api_requests"]
        .as_u64()
        .map(|n| n + 1);
    accounting["configured_api_requests"] = json!(requests);
    match extra {
        Ok(data) => {
            let extra = parse(&data);
            let cost = data["costDollars"]["total"]
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0);
            accounting["cost_usd"] = json!(accounting["cost_usd"]
                .as_f64()
                .zip(cost)
                .map(|(a, b)| a + b));
            accounting["exa"] = json!({"status":"ok","cost_usd":cost,"result_count":extra.len()});
            if !extra.is_empty() {
                if let Some(providers) = providers.as_array_mut() {
                    providers.push(json!("Exa"));
                }
            }
            Ok((interleave(candidates, extra), providers, accounting))
        }
        Err(_) => {
            tracing::warn!(
                operation = "web-research-search",
                provider = "Exa",
                "supplemental search unavailable"
            );
            accounting["cost_usd"] = Value::Null;
            accounting["exa"] = json!({"status":"unavailable","cost_usd":null});
            Ok((candidates, providers, accounting))
        }
    }
}

async fn search(question: &str, key: &str) -> Result<Value> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8))
        .build()?;
    let mut response = client
        .post("https://api.exa.ai/search")
        .header("x-api-key", key)
        .json(&json!({"query":question,"type":"auto","numResults":10}))
        .send()
        .await?
        .error_for_status()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= 512_000,
            "Exa response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    let data: Value = serde_json::from_slice(&bytes)?;
    ensure!(data["results"].is_array(), "invalid Exa results");
    Ok(data)
}

fn parse(data: &Value) -> Vec<Candidate> {
    data["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let url = row["url"].as_str()?;
            url_key(url)?;
            Some(Candidate {
                title: normalized(row["title"].as_str().unwrap_or(url), 300),
                url: url.into(),
                rank: 0,
            })
        })
        .take(TARGET)
        .collect()
}

fn interleave(primary: Vec<Candidate>, extra: Vec<Candidate>) -> Vec<Candidate> {
    let mut primary = primary.into_iter();
    let mut extra = extra.into_iter();
    let mut merged = Vec::new();
    loop {
        let pair = [primary.next(), extra.next()];
        if pair.iter().all(Option::is_none) {
            break;
        }
        merged.extend(pair.into_iter().flatten());
    }
    prioritize_links(vec![], merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn result(url: &str) -> Value {
        json!({"results":[{"url":url,"title":"Evidence"}],"costDollars":{"total":0.005}})
    }
    fn primary() -> Result<(Vec<Candidate>, Value, Value)> {
        Ok((
            parse(&result("https://example.org/primary")),
            json!(["SerpApi"]),
            json!({"cost_usd":null,"configured_api_requests":1}),
        ))
    }
    #[test]
    fn supplements_even_successful_primary_and_tracks_cost() {
        let (rows, providers, cost) =
            combine(primary(), Ok(result("https://example.org/extra"))).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].rank, 2);
        assert_eq!(providers, json!(["SerpApi", "Exa"]));
        assert_eq!(cost["configured_api_requests"], 2);
        assert_eq!(cost["exa"]["cost_usd"], 0.005);
        assert!(cost["cost_usd"].is_null());
    }
    #[test]
    fn outages_preserve_other_provider() {
        assert_eq!(
            combine(primary(), Err(anyhow::anyhow!("offline")))
                .unwrap()
                .0
                .len(),
            1
        );
        assert_eq!(
            combine(
                Err(anyhow::anyhow!("offline")),
                Ok(result("https://example.org/extra"))
            )
            .unwrap()
            .0
            .len(),
            1
        );
        assert!(combine(Err(anyhow::anyhow!("offline")), Ok(json!({"results":[]}))).is_err());
    }
    #[test]
    fn deduplicates_and_rejects_private_urls() {
        assert_eq!(
            combine(
                primary(),
                Ok(result("https://www.example.org/primary?utm_source=exa"))
            )
            .unwrap()
            .0
            .len(),
            1
        );
        assert!(parse(&result("http://127.0.0.1/secret")).is_empty());
    }
    #[test]
    fn supplemental_results_reach_read_budget() {
        let primary = (0..25)
            .map(|i| Candidate {
                title: "Primary".into(),
                url: format!("https://example.org/{i}"),
                rank: i + 1,
            })
            .collect();
        let merged = interleave(primary, parse(&result("https://example.org/extra")));
        assert_eq!(merged[1].url, "https://example.org/extra");
        assert_eq!(merged.len(), MAX_CANDIDATES);
    }
    #[test]
    fn explicit_exa_accounts_for_one_search_and_assigns_citable_ranks() {
        let (rows, providers, operation) = combine(
            Ok((vec![], json!([]), json!({"cost_usd":0,"configured_api_requests":0}))),
            Ok(result("https://example.org/evidence")),
        ).unwrap();
        assert_eq!(rows[0].rank, 1);
        assert_eq!(providers, json!(["Exa"]));
        assert_eq!(operation["cost_usd"], 0.005);
        assert_eq!(operation["configured_api_requests"], 1);
    }
}
