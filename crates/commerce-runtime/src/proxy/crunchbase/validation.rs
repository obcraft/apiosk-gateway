use super::{Result, Value};
use serde_json::json;
// Public fields/cards discovered from this account's official cb_reference.
// The underlying MCP also exposes private notes and CRM collections. Those
// must never become accessible through Apiosk's shared provider account.
fn schema() -> &'static Value {
    static VALUE: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    VALUE.get_or_init(|| serde_json::from_str(include_str!("public-schema.json")).expect("reviewed Crunchbase public schema"))
}
fn contains(list: &Value, value: &str) -> bool {
    list.as_array().is_some_and(|items| items.iter().any(|item| item.as_str() == Some(value)))
}
pub(super) fn prepare(name: &str, args: &mut Value) -> Result<()> {
    arguments(name,args)?;
    let invalid="invalid_crunchbase_arguments";
    if name=="cb_reference" {
        let path=args["path"].as_str().unwrap_or("index");
        let parts:Vec<_>=path.split('/').collect();
        let public=matches!(parts[0],""|"index"|"field_types"|"operators"|"guidance")
            || (parts[0]=="entities" && (parts.len()==1 || matches!(parts[1],"organization"|"person"|"funding_round"|"location"|"category"|"category_group")));
        if !public || path.contains("..") { return Err(invalid); }
    } else if name=="cb_entity_autocomplete" {
        let collections=args["collection_ids"].as_str().unwrap_or("");
        if collections.trim().is_empty() {
            args["collection_ids"]=json!("organizations,people,funding_rounds,locations,categories,category_groups");
        } else if !collections.split(',').all(|s| matches!(s.trim(),"organizations"|"people"|"funding_rounds"|"locations"|"categories"|"category_groups")) { return Err(invalid); }
    } else {
        let kind=if name=="cb_entity_get" { args["entity_def_id"].as_str().unwrap_or("organization") }
            else {match args["collection_id"].as_str(){Some("organizations"|"organization.companies")=>"organization",Some("people")=>"person",Some("funding_rounds")=>"funding_round",_=>return Err(invalid)}};
        let public=&schema()[kind];
        let fields=&public["fields"];
        if let Some(selected)=args["field_ids"].as_array() {
            if selected.iter().any(|v| !v.as_str().is_some_and(|s| contains(fields,s))) { return Err(invalid); }
        } else {args["field_ids"]=fields.clone();}
        if name=="cb_entity_get" {
            let cards=&public["cards"];
            if let Some(selected)=args["card_ids"].as_array() {
                if selected.iter().any(|v| !v.as_str().is_some_and(|s| contains(cards,s))) { return Err(invalid); }
            } else {args["card_ids"]=public["default_cards"].clone();}
            if args.get("after_id").is_some() && args["card_ids"].as_array().is_none_or(|c| c.len()!=1) {return Err(invalid);}
        } else {
            // Complex relationship queries require a separate reviewed public
            // relationship contract. Do not expose saved-list membership.
            if args.get("query").is_some() {return Err(invalid);}
            for item in args["predicates"].as_array().into_iter().flatten().chain(args["order"].as_array().into_iter().flatten()) {
                if !item["field_id"].as_str().is_some_and(|s| contains(fields,s))
                    || matches!(item["operator_id"].as_str(),Some("in_list"|"not_in_list")) {return Err(invalid);}
            }
            if args.get("limit").is_none() {args["limit"]=json!(15);}
        }
    }
    Ok(())
}
fn text(v: &Value, max: usize, empty: bool) -> bool { v.as_str().is_some_and(|s| (empty || !s.trim().is_empty()) && s.len() <= max && !s.chars().any(char::is_control)) }
fn integer(v: &Value, min: u64, max: u64) -> bool { v.as_u64().is_some_and(|n| (min..=max).contains(&n)) }
fn strings(v: &Value) -> bool { v.as_array().is_some_and(|a| a.len() <= 100 && a.iter().all(|s| text(s,200,false))) }
pub(super) fn arguments(name: &str, args: &Value) -> Result<()> {
    let obj = args.as_object().ok_or("invalid_crunchbase_arguments")?;
    if args.to_string().len() > 32768 { return Err("invalid_crunchbase_arguments"); }
    let allowed: &[&str] = match name {
        "cb_reference" => &["path"],
        "cb_entity_autocomplete" => &["query","collection_ids","limit"],
        "cb_entity_get" => &["entity_id","entity_def_id","field_ids","card_ids","related_entity_limit","after_id"],
        "cb_search_query" => &["collection_id","predicates","query","field_ids","order","limit","after_id"],
        _ => return Err("unsupported_crunchbase_route"),
    };
    if obj.keys().any(|k| !allowed.contains(&k.as_str())) { return Err("invalid_crunchbase_arguments"); }
    let valid = match name {
        "cb_reference" => obj.get("path").is_none_or(|v| text(v,300,true)),
        "cb_entity_autocomplete" => text(&args["query"],200,false)
            && obj.get("collection_ids").is_none_or(|v| text(v,300,true))
            && obj.get("limit").is_none_or(|v| integer(v,1,25)),
        "cb_entity_get" => text(&args["entity_id"],200,false)
            && args["entity_id"].as_str().is_some_and(|s| s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
            && obj.get("entity_def_id").is_none_or(|v| matches!(v.as_str(),Some("organization"|"person"|"funding_round")))
            && obj.get("related_entity_limit").is_none_or(|v| integer(v,1,100)),
        "cb_search_query" => text(&args["collection_id"],100,false)
            && !(obj.contains_key("query") && obj.contains_key("predicates"))
            && obj.get("limit").is_none_or(|v| integer(v,1,15))
            && obj.get("predicates").is_none_or(|v| v.as_array().is_some_and(|a| a.len() <= 25 && a.iter().all(predicate)))
            && obj.get("query").is_none_or(|v| v.as_array().is_some_and(|a| a.len() <= 25 && a.iter().all(|i| query_item(i,0))))
            && obj.get("order").is_none_or(|v| v.as_array().is_some_and(|a| a.len() <= 5 && a.iter().all(|i| {
                text(&i["field_id"],200,false) && matches!(i["sort"].as_str(),Some("asc"|"desc"))
                    && i.get("nulls").is_none_or(|n| matches!(n.as_str(),Some("first"|"last")))
            }))),
        _ => false,
    };
    if !valid || ["field_ids","card_ids"].iter().any(|k| obj.get(*k).is_some_and(|v| !strings(v)))
        || obj.get("after_id").is_some_and(|v| !v.as_str().is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())) {
        return Err("invalid_crunchbase_arguments");
    }
    Ok(())
}
fn predicate(v: &Value) -> bool {
    text(&v["field_id"],200,false) && text(&v["operator_id"],100,false)
        && v["values"].as_array().is_some_and(|a| a.len() <= 100 && a.iter().all(|x| x.is_string() || x.is_number() || x.is_boolean()))
}
// Recursive query shapes are schema-dependent. Bound their size and depth,
// preserving the server's exact structure instead of guessing nested field names.
fn query_item(v: &Value, depth: usize) -> bool {
    if depth > 6 { return false; }
    match v {
        Value::Object(o) => o.len() <= 20 && o.values().all(|v| query_item(v,depth+1)),
        Value::Array(a) => a.len() <= 100 && a.iter().all(|v| query_item(v,depth+1)),
        Value::String(s) => s.len() <= 1000,
        _ => true,
    }
}
#[cfg(test)] mod tests {
    use super::*; use serde_json::json;
    #[test] fn filters_are_preserved_and_bounded() {
        let args=json!({"collection_id":"funding_rounds","predicates":[{"field_id":"money_raised","operator_id":"gte","values":[15000000]}],"field_ids":["identifier","money_raised"],"limit":15});
        assert!(arguments("cb_search_query",&args).is_ok());
        let mut conflicting=args.clone(); conflicting["query"]=json!([]);
        assert!(arguments("cb_search_query",&conflicting).is_err());
        let mut oversized=args; oversized["limit"]=json!(16);
        assert!(arguments("cb_search_query",&oversized).is_err());
        assert!(arguments("cb_entity_autocomplete",&json!({"query":"Starbucks","collection_ids":["organizations"]})).is_err());
        assert!(arguments("cb_entity_get",&json!({"entity_id":"https://evil.test"})).is_err());
        assert!(arguments("cb_entity_get",&json!({"entity_id":"starbucks","related_entity_limit":2000})).is_err());
        assert!(arguments("cb_entity_get",&json!({"entity_id":"starbucks","entity_def_id":"organization","field_ids":[],"card_ids":["founders"]})).is_ok());
    }
    #[test] fn shared_account_exposes_only_reviewed_public_data() {
        for (tool,args) in [
            ("cb_entity_autocomplete",json!({"query":"test","collection_ids":"private_notes"})),
            ("cb_search_query",json!({"collection_id":"xrm_accounts"})),
            ("cb_search_query",json!({"collection_id":"organizations","predicates":[{"field_id":"identifier","operator_id":"in_list","values":["saved-list"]}]})),
            ("cb_search_query",json!({"collection_id":"organizations","query":[]})),
            ("cb_entity_get",json!({"entity_id":"starbucks","card_ids":["private_notes"]})),
            ("cb_entity_get",json!({"entity_id":"starbucks","field_ids":["private_note"]})),
            ("cb_reference",json!({"path":"entities/private_note/fields"})),
        ] {assert!(prepare(tool,&mut args.clone()).is_err(),"{tool}");}
        let mut search=json!({"collection_id":"organizations","predicates":[{"field_id":"identifier","operator_id":"eq","values":["starbucks"]}]});
        prepare("cb_search_query",&mut search).unwrap();
        assert_eq!(search["limit"],15);assert_eq!(search["predicates"][0]["values"][0],"starbucks");
        let mut profile=json!({"entity_id":"starbucks"});prepare("cb_entity_get",&mut profile).unwrap();
        assert!(contains(&profile["card_ids"],"current_advisors_image_list"));
        assert!(contains(&profile["card_ids"],"funding_rounds_list"));
        let mut lookup=json!({"query":"Starbucks"});prepare("cb_entity_autocomplete",&mut lookup).unwrap();
        assert_eq!(lookup["collection_ids"],"organizations,people,funding_rounds,locations,categories,category_groups");
    }
}
