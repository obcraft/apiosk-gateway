use super::{Company, SavedSource};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(super) fn evidence(s: &SavedSource, pointer: &str) -> Value {
    json!({"result_ref":s.result_ref,"pointer":pointer,"value":s.document.pointer(pointer),
        "retrieved_at":s.retrieved_at,"source":s.document["source"]})
}
fn signal(
    c: &Company,
    s: &SavedSource,
    kind: &str,
    identity: &str,
    comparison: Value,
    message: &str,
    pointer: &str,
    previous: Value,
) -> Value {
    json!({"kvk_number":c.kvk_number,"company_name":c.name,"kind":kind,"identity":identity,
        "source":provider(s),"comparison_value":comparison,
        "message":message,"review_required":true,"previous":previous,"current":s.document.pointer(pointer),
        "evidence":[evidence(s,pointer)]})
}
pub(super) fn provider(s: &SavedSource) -> &str {
    s.document["source"]["provider"]
        .as_str()
        .or_else(|| s.document["source"]["api_slug"].as_str())
        .unwrap_or_default()
}
fn usable(s: &SavedSource) -> bool {
    !matches!(
        s.document["coverage"].as_str(),
        Some("unavailable" | "failed")
    ) && s.document["data"].is_object()
}
fn canonical_strings(value: &Value) -> Value {
    let mut strings: Vec<_> = value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    strings.sort_unstable();
    strings.dedup();
    json!(strings)
}

pub(super) fn project(
    c: &Company,
    sources: &[SavedSource],
    previous: Option<&Value>,
    observations: &mut BTreeMap<String, Value>,
    signals: &mut Vec<Value>,
    coverage: &mut Vec<Value>,
) {
    let mut registry = false;
    let mut sanctions = false;
    let mut news = false;
    for s in sources {
        let slug = provider(s);
        if !usable(s) {
            continue;
        }
        let complete = s.document["coverage"] != "partial"
            && s.document["missing_fields"]
                .as_array()
                .is_none_or(|fields| fields.is_empty());
        let d = &s.document["data"];
        // A KVK profile must itself return the exact registration number.
        if slug == "kvk-dutch-business-register"
            && d["kvkNummer"] == c.kvk_number
            && d["naam"].as_str().is_some_and(|n| !n.is_empty())
        {
            registry |= complete;
            for (field, pointer) in [
                ("name", "/data/naam"),
                ("trade_names", "/data/handelsnamen"),
                ("activities", "/data/sbiActiviteiten"),
                ("employees", "/data/totaalWerkzamePersonen"),
                ("legal_name", "/data/statutaireNaam"),
                ("legal_form", "/data/_embedded/eigenaar/rechtsvorm"),
                ("addresses", "/data/_embedded/hoofdvestiging/adressen"),
                ("websites", "/data/_embedded/hoofdvestiging/websites"),
                ("owner_addresses", "/data/_embedded/eigenaar/adressen"),
                ("owner_websites", "/data/_embedded/eigenaar/websites"),
                ("registration_end", "/data/materieleRegistratie/datumEinde"),
            ] {
                let absent_end = Value::Null;
                let raw = s
                    .document
                    .pointer(pointer)
                    .filter(|v| !v.is_null())
                    .or_else(|| {
                        (field == "registration_end"
                            && complete
                            && d["materieleRegistratie"].is_object())
                        .then_some(&absent_end)
                    });
                let Some(raw) = raw else {
                    continue;
                };
                // Honor the register's address shielding, as the existing
                // profile presentation does. Missing visibility is not consent.
                if field.ends_with("addresses")
                    && raw
                        .as_array()
                        .is_none_or(|rows| rows.iter().any(|r| r["indAfgeschermd"] != "Nee"))
                {
                    continue;
                }
                let current = canonical(raw);
                let key = format!("{}:{slug}:{field}", c.kvk_number);
                let before = previous.and_then(|p| p["observations"].get(&key));
                observations.insert(key, current.clone());
                if before.is_some_and(|b| b != &current) {
                    let comparison = json!({"before":before,"after":current,"baseline_at":previous.and_then(|p|p.get("checked_at"))});
                    let mut finding = signal(
                        c,
                        s,
                        "registry_change",
                        field,
                        comparison,
                        "Available company details in the KVK register have changed.",
                        pointer,
                        before.cloned().unwrap_or(Value::Null),
                    );
                    finding["current"] = current;
                    signals.push(finding);
                }
            }
        }

        // The engine's entity binding identifies the screened subject. The
        // returned candidate is still only a potential match, never that entity.
        if slug == "opensanctions" && s.kvk_number.as_deref() == Some(&c.kvk_number) {
            let Some(responses) = d["responses"].as_object().filter(|r| r.len() == 1) else {
                continue;
            };
            let (query, response) = responses.iter().next().unwrap();
            if response["status"].as_u64().is_some_and(|s| s != 200) {
                continue;
            }
            let Some(rows) = response["results"].as_array() else {
                continue;
            };
            let total = response["total"]["value"]
                .as_u64()
                .or_else(|| response["total"].as_u64());
            sanctions |= complete
                && response["status"].as_u64().is_none_or(|s| s == 200)
                && response["total"]["relation"]
                    .as_str()
                    .is_none_or(|r| r == "eq")
                && total.is_some_and(|total| total as usize == rows.len());
            let query = query.replace('~', "~0").replace('/', "~1");
            for (index, row) in rows.iter().enumerate().filter(|(_, r)| r["match"] == true) {
                let Some(id) = row["id"].as_str().filter(|s| !s.is_empty()) else {
                    continue;
                };
                // The requested scope is sanctions, not all politically
                // exposed people or other entries in the default dataset.
                if !row["properties"]["topics"]
                    .as_array()
                    .is_some_and(|t| t.iter().any(|v| v == "sanction"))
                {
                    continue;
                }
                // Score, candidate order and provider timestamps are not events.
                let stable = json!({"id":id,"topics":canonical_strings(&row["properties"]["topics"]),"datasets":canonical_strings(&row["datasets"])});
                let caption = row["caption"].as_str().unwrap_or(id);
                let mut finding=signal(c,s,"potential_screening_match",id,stable,
                    &format!("Potential match with a sanctions entry: {caption}. Manual identity review required; this is not a confirmed sanctions hit."),
                    &format!("/data/responses/{query}/results/{index}"),Value::Null);
                let mut link = url::Url::parse("https://www.opensanctions.org/entities/").unwrap();
                link.path_segments_mut().unwrap().push(id).push("");
                finding["url"] = json!(link.as_str());
                signals.push(finding);
            }
        }
        if matches!(slug, "exa-search" | "tavily" | crate::web_search::SLUG) && s.kvk_number.as_deref() == Some(&c.kvk_number)
        {
            news |= super::news::project(c, s, complete, signals);
        }
    }
    for (category, checked, gap) in [
        ("registry",registry,"No complete register result with exactly this KVK number is available."),
        ("news",news,"No complete supported news search result is available. Finding no news does not mean there are no developments."),
        ("sanctions",sanctions,"No complete sanctions screening is available for this company identity. Missing results are never a clearance.")
    ] {
        coverage.push(json!({"kvk_number":c.kvk_number,"category":category,"status":if checked {"checked"} else {"unknown"},"message":if checked {"Checked within the stated source scope."} else {gap}}));
    }
}

/// Provider array order and link metadata do not constitute business changes.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Array(items) => {
            let mut items: Vec<_> = items.iter().map(canonical).collect();
            items.sort_by_key(Value::to_string);
            items.dedup();
            json!(items)
        }
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .filter(|(k, _)| !matches!(k.as_str(), "links" | "_links"))
                .map(|(k, v)| (k.clone(), canonical(v)))
                .collect(),
        ),
        value => value.clone(),
    }
}
