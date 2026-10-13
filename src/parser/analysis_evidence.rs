//! Compact, request-local evidence IDs prevent the model from inventing paths.
//! IDs resolve only to exact saved scalar fields or contiguous source excerpts.
use anyhow::{ensure, Context};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Index {
    pub sources: Vec<Value>,
    references: BTreeMap<String, Value>,
}

pub(super) fn multiple_providers(sources: &[Value]) -> bool {
    sources.iter().map(|s| s["document"]["source"]["provider"].as_str()
        .filter(|s|!s.trim().is_empty()).unwrap_or(s["result_ref"].as_str().unwrap_or_default()).to_lowercase())
        .collect::<BTreeSet<_>>().len() > 1
}

/// Reserve the same bounded input share for each provider before indexing;
/// a large collection or codebook cannot consume another provider's share.
pub(super) fn project(sources: &[Value], operations: &Value, budget: usize) -> Vec<Value> {
    let mut providers: BTreeMap<String, Vec<Value>>=BTreeMap::new();
    for source in sources {
        let key=source["document"]["source"]["provider"].as_str().filter(|s|!s.trim().is_empty())
            .unwrap_or(source["result_ref"].as_str().unwrap_or_default()).to_lowercase();
        providers.entry(key).or_default().push(source.clone());
    }
    // Measure the public projection, so geometry and embedded files do not
    // reserve bytes that can instead retain another provider's useful facts.
    let groups: Vec<Vec<Value>>=providers.values().map(|group|
        crate::evidence_selection::project(group,operations,usize::MAX)).collect();
    let sizes: Vec<usize>=groups.iter().map(|group|serde_json::to_vec(group).map_or(usize::MAX,|bytes|bytes.len())).collect();
    let mut allocations=vec![0;groups.len()];
    let mut active: Vec<usize>=(0..groups.len()).collect();
    let mut remaining=budget;
    while !active.is_empty() {
        let share=remaining/active.len();
        let remainder=remaining%active.len();
        let small: Vec<usize>=active.iter().enumerate().filter_map(|(rank,&index)|
            (sizes[index]<=share+usize::from(rank<remainder)).then_some(index)).collect();
        if small.is_empty() {
            for (rank,index) in active.into_iter().enumerate() {allocations[index]=share+usize::from(rank<remainder);}
            break;
        }
        for index in &small {allocations[*index]=sizes[*index];remaining-=sizes[*index];}
        active.retain(|index|!small.contains(index));
    }
    let projected: Vec<Value>=groups.iter().zip(allocations).flat_map(|(group,share)|
        crate::evidence_selection::project(group,operations,share)).collect();
    sources.iter().filter_map(|source|projected.iter().find(|p|p["result_ref"]==source["result_ref"]).cloned()).collect()
}

impl Index {
    /// Keep each record's sibling fields together for the model. The private
    /// reference map still uses their exact original absolute pointers.
    pub fn input_sources(&self) -> Vec<Value> {
        self.sources.iter().map(|source| {
            let mut source=source.clone();
            let mut records: Vec<Value>=Vec::new();
            for field in source["fields"].as_array().into_iter().flatten() {
                let path=field["path"].as_str().unwrap_or_default();
                let (parent,key)=path.rsplit_once('/').unwrap_or(("",path));
                let position=records.iter().position(|record|record["path"]==parent).unwrap_or_else(|| {
                    records.push(json!({"path":parent,"fields":[]}));
                    records.len()-1
                });
                let mut entry=json!({"field":key.replace("~1","/").replace("~0","~"),"value":field["value"]});
                if let Some(id)=field.get("id") {entry["id"]=id.clone();}
                records[position]["fields"].as_array_mut().unwrap().push(entry);
            }
            source.as_object_mut().unwrap().remove("fields");
            source["records"]=json!(records);
            source
        }).collect()
    }

    pub fn new(projected: &[Value], originals: &[Value]) -> Self {
        let mut index = Self { sources: Vec::new(), references: BTreeMap::new() };
        for source in projected {
            let mut fields = Vec::new();
            let original = originals.iter().find(|s|s["result_ref"]==source["result_ref"]);
            if let Some(original)=original {
                index.walk(&source["document"]["data"], "/data", source, original, &mut fields);
                if let Some(assets)=source["document"].pointer("/report_assets/assets").and_then(Value::as_array) {
                    for (i,asset) in assets.iter().enumerate() {
                        index.walk(&asset["extracted_text"],&format!("/report_assets/assets/{i}/extracted_text"),source,original,&mut fields);
                    }
                }
            }
            index.sources.push(json!({"result_ref":source["result_ref"],"source":source["document"]["source"],
                "retrieval_scope":source.get("retrieval_scope"),
                "subject":source["document"]["subject"],"coverage":source["document"]["coverage"],
                "coverage_note":source["document"]["coverage_note"],"missing_fields":source["document"]["missing_fields"],
                "evidence_selection":source["document"]["evidence_selection"],"fields":fields}));
        }
        index
    }

    fn walk(&mut self, value: &Value, path: &str, source: &Value, original: &Value, fields: &mut Vec<Value>) {
        match value {
            Value::Object(object) => for (key, value) in object {
                self.walk(value,&format!("{path}/{}",key.replace('~',"~0").replace('/',"~1")),source,original,fields);
            },
            Value::Array(array) => for (i,value) in array.iter().enumerate() {
                self.walk(value,&format!("{path}/{i}"),source,original,fields);
            },
            Value::Null => fields.push(json!({"path":path,"value":null})),
            _ => {
                let Some(saved)=original["document"].pointer(path) else {return};
                if let Some(text)=value.as_str() {
                    if text.trim().is_empty() {return}
                    if text.len()>400 || saved != value {
                        let Some(saved_text)=saved.as_str().filter(|saved|saved.contains(text)) else{return};
                        let _=saved_text;
                        for chunk in chunks(text) { self.add(source,path,json!(chunk),Some(chunk),fields); }
                        return;
                    }
                }
                if saved==value { self.add(source,path,value.clone(),None,fields); }
            }
        }
    }

    fn add(&mut self, source: &Value, path: &str, value: Value, quote: Option<&str>, fields: &mut Vec<Value>) {
        let id=format!("f{}",self.references.len()+1);
        let mut reference=json!({"result_ref":source["result_ref"],"pointer":path});
        if let Some(quote)=quote { reference["quote"]=json!(quote); }
        self.references.insert(id.clone(),reference);
        fields.push(json!({"id":id,"path":path,"value":value}));
    }

    pub fn expand(&self, raw: &str) -> anyhow::Result<String> {
        let mut value: Value=serde_json::from_str(raw)?;
        for observation in value["observations"].as_array().into_iter().flatten() {
            let mut ids=BTreeSet::new();
            for reference in observation["evidence"].as_array().into_iter().flatten() {
                if let Some(id)=reference["fact_id"].as_str() {
                    ensure!(ids.insert(id),"duplicate indexed evidence");
                }
            }
        }
        self.expand_value(&mut value)?;
        // Requirement claims reuse the cited observations instead of asking
        // the model to duplicate every source pointer and value a second time.
        let observations=value["observations"].as_array().cloned().unwrap_or_default();
        if let Some(claims)=value["requirement_status"].as_array_mut() {
            for claim in claims {
                if claim["evidence"].as_array().is_none_or(Vec::is_empty) {
                    let mut evidence=Vec::new();
                    for id in claim["answered_by"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                        let Some(i)=id.strip_prefix('o').and_then(|id|id.parse::<usize>().ok()).and_then(|i|i.checked_sub(1)) else {continue};
                        for fact in observations.get(i).and_then(|o|o["evidence"].as_array()).into_iter().flatten() {
                            if !evidence.contains(fact) {evidence.push(fact.clone());}
                        }
                    }
                    claim["evidence"]=json!(evidence);
                }
            }
        }
        Ok(serde_json::to_string(&value)?)
    }

    fn expand_value(&self, value: &mut Value) -> anyhow::Result<()> {
        match value {
            Value::Object(object) if object.contains_key("fact_id") => {
                ensure!(object.len()==1,"invalid indexed evidence");
                let id=object["fact_id"].as_str().context("invalid indexed evidence")?;
                *value=self.references.get(id).context("unknown indexed evidence")?.clone();
            },
            Value::Object(object) => for value in object.values_mut() {self.expand_value(value)?;},
            Value::Array(array) => for value in array {self.expand_value(value)?;},
            _=>{}
        }
        Ok(())
    }
}

fn chunks(mut text: &str) -> Vec<&str> {
    let mut out=Vec::new();
    while !text.is_empty() {
        let mut end=text.len().min(400);
        while !text.is_char_boundary(end) {end-=1;}
        if end<text.len() {
            if let Some(at)=text[..end].rfind(|c: char|c.is_whitespace()).filter(|at|*at>=200) {end=at+text[at..].chars().next().unwrap().len_utf8();}
        }
        out.push(&text[..end]);
        text=&text[end..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(id: &str, provider: &str, data: Value) -> Value {
        json!({"result_ref":id,"document":{"source":{"provider":provider,"name":"Metadata only"},"data":data}})
    }

    #[test]
    fn preserves_escaped_scalar_pointers_and_false_zero_without_metadata_facts() {
        let original=source("r","registry",json!({"a/b":{"~key":false},"zero":0,"truth":true,"missing":null}));
        let index=Index::new(std::slice::from_ref(&original),std::slice::from_ref(&original));
        let fields=index.sources[0]["fields"].as_array().unwrap();
        for (path,value) in [("/data/a~1b/~0key",json!(false)),("/data/zero",json!(0)),("/data/truth",json!(true))] {
            let field=fields.iter().find(|f|f["path"]==path).unwrap();
            assert_eq!(field["value"],value);
            let expanded: Value=serde_json::from_str(&index.expand(&json!({"fact_id":field["id"]}).to_string()).unwrap()).unwrap();
            assert_eq!(expanded["pointer"],path);
            assert_eq!(original["document"].pointer(path),Some(&value));
        }
        assert!(fields.iter().all(|f|f["path"].as_str().unwrap().starts_with("/data/")));
        assert!(fields.iter().find(|f|f["path"]=="/data/missing").unwrap().get("id").is_none());
    }

    #[test]
    fn rejects_unknown_duplicate_and_decorated_fact_references() {
        let sources=[source("r","p",json!({"name":"Example"}))];
        let index=Index::new(&sources,&sources);
        assert!(index.expand(r#"{"observations":[{"evidence":[{"fact_id":"missing"}]}]}"#).unwrap_err().to_string().contains("unknown"));
        assert!(index.expand(r#"{"observations":[{"evidence":[{"fact_id":"f1"},{"fact_id":"f1"}]}]}"#).unwrap_err().to_string().contains("duplicate"));
        assert!(index.expand(r#"{"fact_id":"f1","pointer":"/data/invented"}"#).is_err());
    }

    #[test]
    fn long_unicode_text_keeps_exact_contiguous_saved_excerpts() {
        let text="Een café — 東京 🌍 heeft officiële gegevens. ".repeat(40);
        let sources=[source("r","p",json!({"text":text}))];
        let index=Index::new(&sources,&sources);
        let fields=index.sources[0]["fields"].as_array().unwrap();
        assert!(fields.len()>1);
        let mut joined=String::new();
        for field in fields {
            let expanded: Value=serde_json::from_str(&index.expand(&json!({"fact_id":field["id"]}).to_string()).unwrap()).unwrap();
            let quote=expanded["quote"].as_str().unwrap();
            assert!(quote.len()<=400 && text.contains(quote));
            assert_eq!(expanded["pointer"],"/data/text");
            assert_eq!(field["value"],quote);
            joined.push_str(quote);
        }
        assert_eq!(joined,text);
    }

    #[test]
    fn projected_excerpts_must_match_original_and_reduced_array_indices_stay_original() {
        let original=source("r","p",json!({"text":"prefix Exact café excerpt suffix","rows":[{"n":0},{"n":1},{"n":2}]}));
        let projected=source("r","p",json!({"text":"Exact café excerpt","rows":{"2":{"n":2}},"invented":"never saved"}));
        let index=Index::new(&[projected],std::slice::from_ref(&original));
        let fields=index.sources[0]["fields"].as_array().unwrap();
        assert!(fields.iter().any(|f|f["path"]=="/data/rows/2/n" && f["value"]==2));
        let field=fields.iter().find(|f|f["path"]=="/data/text").unwrap();
        let expanded: Value=serde_json::from_str(&index.expand(&json!({"fact_id":field["id"]}).to_string()).unwrap()).unwrap();
        assert_eq!(expanded["quote"],"Exact café excerpt");
        assert!(!fields.iter().any(|f|f["path"]=="/data/invented"));
        let fabricated=source("r","p",json!({"text":"Exact different excerpt","rows":{"2":{"n":99}}}));
        assert!(Index::new(&[fabricated],&[original]).sources[0]["fields"].as_array().unwrap().is_empty());
    }

    #[test]
    fn input_records_keep_sibling_meanings_together_and_decode_field_names() {
        let sources=[source("r","statistics",json!({"rows":[{"code":"GDP","unit":"EUR","period":"2024","value":100,"a/b~c":false},{"code":"OTHER","value":2}]}))];
        let index=Index::new(&sources,&sources);
        let input=index.input_sources();
        let records=input[0]["records"].as_array().unwrap();
        assert_eq!(records.len(),2);
        let row=records.iter().find(|r|r["path"]=="/data/rows/0").unwrap();
        let fields=row["fields"].as_array().unwrap();
        for name in ["code","unit","period","value","a/b~c"] {assert!(fields.iter().any(|f|f["field"]==name));}
        let escaped=fields.iter().find(|f|f["field"]=="a/b~c").unwrap();
        let expanded: Value=serde_json::from_str(&index.expand(&json!({"fact_id":escaped["id"]}).to_string()).unwrap()).unwrap();
        assert_eq!(expanded["pointer"],"/data/rows/0/a~1b~0c");
        assert!(input[0].get("fields").is_none());
    }

    #[test]
    fn provider_budget_preserves_small_source_and_redistributes_unused_share_in_original_order() {
        let rows: Vec<Value>=(0..120).map(|n|json!({"code":format!("record-{n}"),"value":"x".repeat(80)})).collect();
        let big=source("big","z-large",json!({"rows":rows}));
        let small=source("small","a-small",json!({"rate":1.25,"currency":"EUR"}));
        let sources=[big.clone(),small.clone()];
        let budget=18000;
        let projected=project(&sources,&json!([]),budget);
        assert_eq!(projected.iter().map(|s|s["result_ref"].as_str().unwrap()).collect::<Vec<_>>(),vec!["big","small"]);
        assert_eq!(projected[1],crate::evidence_selection::project(&[small],&json!([]),usize::MAX)[0]);
        let equal_share=crate::evidence_selection::project(&[big],&json!([]),budget/2);
        let kept=|s:&Value|s["document"]["data"]["rows"].as_object().map_or_else(||s["document"]["data"]["rows"].as_array().unwrap().len(),|o|o.len());
        assert!(kept(&projected[0])>kept(&equal_share[0]),"unused small-provider share should retain additional large-provider records");
        assert!(serde_json::to_vec(&projected).unwrap().len()<=budget);
        assert_eq!(project(&sources,&json!([]),budget),projected,"allocation must be deterministic");
    }
}
