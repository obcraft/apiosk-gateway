//! An opt-in, immutable KVK monitor on the existing recurring execution ledger.
//! Every source call and delivery uses the approved recurring mandate.
use crate::{auth::Principal, company_checks::{Company, SavedSource}, engine::Engine};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use uuid::Uuid;

pub const VERSION: &str = "company_monitor_v1";
pub const PROVIDER: &str = "kvk-dutch-business-register";
pub const SANCTIONS: &str = "opensanctions";
pub const GLEIF: &str = "gleif";

const FIELDS: [(&str, &str); 20] = [
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
    ("sanctions_matches", "/data/responses"),
    ("registered_names", "/data"),
    ("branch_profile", "/data"),
    ("financial_statements", "/data"),
    ("lei", "/attributes/lei"),
    ("gleif_legal_name", "/attributes/entity/legalName/name"),
    ("gleif_legal_address", "/attributes/entity/legalAddress"),
    ("gleif_entity_status", "/attributes/entity/status"),
    ("gleif_registration_status", "/attributes/registration/status"),
];

pub fn sources(input: Vec<String>) -> anyhow::Result<Vec<String>> {
    let sources = if input.is_empty() {vec![PROVIDER.into(),SANCTIONS.into()]}else{input};
    anyhow::ensure!(sources.iter().any(|s|s==PROVIDER) && sources.len()<=3
        && sources.iter().collect::<BTreeSet<_>>().len()==sources.len()
        && sources.iter().all(|s|matches!(s.as_str(),PROVIDER|SANCTIONS|GLEIF)), "Select KVK and supported trusted compliance sources.");
    Ok(sources)
}

/// The dedicated page's exact selected company is also the search input for
/// name-based sources. This fact does not prove that a returned match is the
/// company; source adapters still require their own exact-identity evidence.
pub fn prepare_recipe(company:&Company, source_ids:&[String], establishment_number:Option<&str>, catalog:&crate::catalog::Catalog) -> (crate::parser::Intent, Vec<Value>) {
    let mut recipe=crate::company_checks::monitoring_recipe(company,source_ids.iter().any(|s|s==SANCTIONS));
    recipe.subjects[0].values.insert("company.name".into(),json!(company.name));
    if source_ids.iter().any(|s|s==GLEIF) {recipe.goals.push(crate::parser::Goal{capability:"company.legal-entity-search".into(),subject:0,constraints:std::collections::BTreeMap::from([("source".into(),json!(GLEIF))])});}
    let mut gaps=Vec::new();
    for operation in ["company.registered-names","company.financial-statements","company.branch-profile"] {
        if operation=="company.branch-profile" && establishment_number.is_none() {
            gaps.push(json!({"source":PROVIDER,"capability":operation,"reason":"Choose an exact establishment to retrieve its branch profile."}));continue;
        }
        if catalog.services.iter().any(|s|s.api_slug==PROVIDER && s.contract.capability_slug==operation) {
            recipe.goals.push(crate::parser::Goal{capability:operation.into(),subject:0,constraints:std::collections::BTreeMap::from([("source".into(),json!(PROVIDER))])});
        } else { gaps.push(json!({"source":PROVIDER,"capability":operation,"reason":"This operation is not available in the active catalog."})); }
    }
    if let Some(number)=establishment_number {recipe.subjects[0].values.insert("company.vestigingsnummer".into(),json!(number));}
    (recipe,gaps)
}

pub fn field_source(key: &str) -> &'static str { if key == "sanctions_matches" {SANCTIONS}else if key=="lei" || key.starts_with("gleif_"){GLEIF}else{PROVIDER} }
pub fn field_operation(key: &str) -> &'static str {match key {
    "sanctions_matches"=>"company.sanctions-screen","registered_names"=>"company.registered-names",
    "branch_profile"=>"company.branch-profile","financial_statements"=>"company.financial-statements",
    "lei"|"gleif_legal_name"|"gleif_legal_address"|"gleif_entity_status"|"gleif_registration_status"=>"company.legal-entity-search",_=>"company.registry-profile"}}
pub fn operations(fields: &[String]) -> BTreeSet<String> {
    std::iter::once("company.registry-profile".to_owned()).chain(fields.iter().map(|f|field_operation(f).to_owned())).collect()
}
fn complete(s:&SavedSource)->bool { !matches!(s.document["coverage"].as_str(),Some("partial"|"unavailable"|"failed")) && s.document["missing_fields"].as_array().is_none_or(|v|v.is_empty()) }
fn canonical(value:&Value)->Value {match value {
    Value::Array(rows)=>{let mut rows:Vec<_>=rows.iter().map(canonical).collect();rows.sort_by_key(Value::to_string);rows.dedup();json!(rows)},
    Value::Object(fields)=>Value::Object(fields.iter().filter(|(k,_)|!matches!(k.as_str(),"links"|"_links")).map(|(k,v)|(k.clone(),canonical(v))).collect()),
    v=>v.clone()
}}
fn shielded(value:&Value)->bool {match value {
    Value::Object(fields)=>fields.get("indAfgeschermd").is_some_and(|v|v!="Nee") || fields.iter().any(|(key,value)|
        (key=="adressen" && value.as_array().is_none_or(|rows|rows.iter().any(|row|row["indAfgeschermd"]!="Nee"))) || shielded(value)),
    Value::Array(values)=>values.iter().any(shielded),_=>false
}}
fn public_source(value:&Value)->Value {match value {
    Value::Object(fields) if fields.get("indAfgeschermd").is_some_and(|v|v!="Nee")=>json!({"indAfgeschermd":fields["indAfgeschermd"]}),
    Value::Object(fields)=>Value::Object(fields.iter().filter(|(key,value)|!(key.as_str()=="adressen" && value.as_array().is_none_or(|rows|rows.iter().any(|row|row["indAfgeschermd"]!="Nee")))).map(|(k,v)|(k.clone(),public_source(v))).collect()),
    Value::Array(values)=>json!(values.iter().map(public_source).collect::<Vec<_>>()),v=>v.clone()
}}
fn usable_extra(s:&SavedSource,field:&str,company:&Company)->bool {
    let data=&s.document["data"];
    s.document["source"]["capability"]==field_operation(field) && s.kvk_number.as_deref()==Some(&company.kvk_number)
        && complete(s) && !data.is_null() && !shielded(data)
        && data.get("kvkNummer").is_none_or(|value|value==&company.kvk_number)
        && (!matches!(field,"branch_profile"|"registered_names") || data["kvkNummer"]==company.kvk_number)
        && (field!="branch_profile" || (s.document["source"]["establishment_number"].as_str().is_some_and(|n|n.len()==12) && data["vestigingsnummer"]==s.document["source"]["establishment_number"]))
        && (field!="financial_statements" || valid_financial_data(data))
}
fn valid_financial_data(data:&Value)->bool {
    fn year(value:&Value)->bool {match value {
        Value::Object(fields)=>(fields.get("key").is_some_and(|k|k=="FinancialYear") && fields.get("value").and_then(Value::as_str).is_some_and(|y|y.len()==4 && y.parse::<u32>().is_ok_and(|year|(1900..=2200).contains(&year)))) || fields.values().any(year),
        Value::Array(rows)=>rows.iter().any(year),_=>false
    }}
    data["opendataFields"].as_array().is_some_and(|rows|!rows.is_empty()) && year(data)
}
fn missing_values(before:&Value,current:&Value)->bool {match (before,current) {
    (Value::Object(old),Value::Object(new))=>old.iter().any(|(key,value)|!matches!(key.as_str(),"links"|"_links") && new.get(key).is_none_or(|next|(!value.is_null() && next.is_null()) || missing_values(value,next))),
    (Value::Array(old),Value::Array(new))=>old.iter().any(|row| {
        let matched=["key","id","vestigingsnummer","sbiCode","naam"].into_iter().find_map(|key|row.get(key).and_then(|id|new.iter().find(|r|r.get(key)==Some(id))));
        (row.get("key").is_some() && matched.is_none()) || matched.is_some_and(|next|missing_values(row,next))
    }),_=>false
}}
fn observed_extra(s:&SavedSource,field:&str,company:&Company,previous:Option<&Value>)->bool {
    usable_extra(s,field,company) && previous.and_then(|p|p["observations"].get(format!("{}:{PROVIDER}:{field}",company.kvk_number)))
        .is_none_or(|before|!missing_values(before,&s.document["data"]))
}
fn gleif_record<'a>(s:&'a SavedSource,company:&Company)->Option<(usize,&'a Value)> {
    if !(s.document["source"]["provider"]==GLEIF || s.document["source"]["api_slug"]==GLEIF) || !complete(s) {return None;}
    let matches:Vec<_>=s.document["data"]["data"].as_array()?.iter().enumerate().filter(|(_,row)|
        row["attributes"]["entity"]["registeredAt"]["id"]=="RA000463" && row["attributes"]["entity"]["registeredAs"]==company.kvk_number).collect();
    (matches.len()==1).then(||matches[0])
}

pub fn snapshot(company: &Company, sources: &[SavedSource], previous: Option<&Value>, at: &str) -> Value {
    snapshot_for_sources(company, sources, previous, at, &[PROVIDER.into()])
}

pub fn snapshot_for_sources(company: &Company, sources: &[SavedSource], previous: Option<&Value>, at: &str, source_ids: &[String]) -> Value {
    // Recurring delivery owns its durable queue. Do not carry a previous run's
    // signals into a new message; an uncertain send is reconciled independently.
    let reported: BTreeSet<String> = previous.into_iter()
        .flat_map(|p| p["signals"].as_array().into_iter().flatten())
        .filter_map(|s| s["signal_id"].as_str().map(str::to_owned)).collect();
    let core_sources:Vec<_>=sources.iter().filter(|s|s.document["source"]["capability"].as_str().is_none_or(|c|matches!(c,"company.registry-profile"|"company.sanctions-screen"))).cloned().collect();
    let mut out = crate::company_checks::snapshot(std::slice::from_ref(company), &core_sources, previous,
        &reported, at, Value::Null, "succeeded");
    out["sources"]=public_source(&json!(sources));out["source_count"]=json!(sources.len());
    out["coverage"] = json!(out["coverage"].as_array().into_iter().flatten()
        .filter(|c| c["category"] == "registry" || (c["category"] == "sanctions" && source_ids.iter().any(|s|s==SANCTIONS))).cloned().collect::<Vec<_>>());
    out["source_ids"] = json!(source_ids);
    let gleif=sources.iter().find_map(|s|gleif_record(s,company).map(|(index,row)|(s,index,row)));
    if source_ids.iter().any(|source|source==GLEIF) {
        out["coverage"].as_array_mut().unwrap().push(json!({"kvk_number":company.kvk_number,"category":"gleif","source":GLEIF,
            "status":if gleif.is_some(){"checked"}else{"unknown"},"message":if gleif.is_some(){"One LEI record matched the exact KVK registration and official registration authority RA000463."}else{"No unique LEI record with this exact KVK number and registration authority was returned. Name matches are not identity confirmation."}}));
        if let Some((source,index,row))=gleif {
            for (field,pointer) in FIELDS.iter().filter(|(field,_)|field_source(field)==GLEIF) {
                let Some(value)=row.pointer(pointer).filter(|v|!v.is_null()) else {continue;};
                let current=canonical(value);let key=format!("{}:{GLEIF}:{field}",company.kvk_number);
                let before=previous.and_then(|p|p["observations"].get(&key));
                if before.is_some_and(|old|missing_values(old,value)) {continue;}
                out["observations"][&key]=current.clone();
                if before.is_some_and(|value|value!=&current) {
                    let path=format!("/data/data/{index}{pointer}");
                    let comparison=json!({"before":before,"after":current,"baseline_at":previous.and_then(|p|p.get("checked_at"))});
                    out["new_signals"].as_array_mut().unwrap().push(json!({"kind":"trusted_source_change","identity":field,"kvk_number":company.kvk_number,"company_name":company.name,
                        "source":GLEIF,"previous":before,"current":current,"comparison_value":comparison,"signal_id":crate::state::digest(&comparison),"message":"GLEIF data for the exact KVK identity changed.","review_required":true,
                        "evidence":[{"result_ref":source.result_ref,"pointer":path,"value":value,"retrieved_at":source.retrieved_at,"source":source.document["source"]}]}));
                }
            }
        }
    }
    for field in ["registered_names","branch_profile","financial_statements"] {
        if let Some(source)=sources.iter().find(|s|observed_extra(s,field,company,previous)) {
            let current=canonical(&source.document["data"]);
            if current.is_null() {continue;}
            let key=format!("{}:{PROVIDER}:{field}",company.kvk_number);
            let before=previous.and_then(|p|p["observations"].get(&key));
            out["observations"][&key]=current.clone();
            if before.is_some_and(|v|v!=&current) {
                let comparison=json!({"before":before,"after":current,"baseline_at":previous.and_then(|p|p.get("checked_at"))});
                out["new_signals"].as_array_mut().unwrap().push(json!({"kind":"registry_change","identity":field,"kvk_number":company.kvk_number,"company_name":company.name,
                    "source":PROVIDER,"previous":before,"current":current,"comparison_value":comparison,"signal_id":crate::state::digest(&comparison),
                    "message":"Available KVK source data changed.","review_required":true,
                    "evidence":[{"result_ref":source.result_ref,"pointer":"/data","value":source.document["data"],"retrieved_at":source.retrieved_at,"source":source.document["source"]}]}));
            }
        }
    }
    // Some successful screening responses omit candidate classifications. A
    // positive name match without topics is not evidence of an empty sanctions
    // set. Keep the last observation until the source can classify every match.
    let unclassified_match = sources.iter().filter(|s|s.kvk_number.as_deref()==Some(&company.kvk_number)
        && (s.document["source"]["provider"]==SANCTIONS || s.document["source"]["api_slug"]==SANCTIONS))
        .flat_map(|s|s.document["data"]["responses"].as_object().into_iter().flat_map(|responses|responses.values()))
        .flat_map(|response|response["results"].as_array().into_iter().flatten())
        .any(|row|row["match"]==true && !row["properties"]["topics"].as_array().is_some_and(|topics|
            !topics.is_empty() && topics.iter().all(|topic|topic.as_str().is_some_and(|value|!value.trim().is_empty()))));
    if unclassified_match {
        for coverage in out["coverage"].as_array_mut().into_iter().flatten().filter(|c|c["category"]=="sanctions") {
            coverage["status"]=json!("unknown");
            coverage["message"]=json!("OpenSanctions returned a potential match without usable topic classification. Sanctions classification is unavailable; the previous observation is retained. This is not clearance.");
        }
    }
    if source_ids.iter().any(|s|s==SANCTIONS) && out["coverage"].as_array().is_some_and(|rows|rows.iter().any(|c|c["category"]=="sanctions" && c["status"]=="checked")) {
        let mut matches:Vec<Value> = out["signals"].as_array().into_iter().flatten().filter(|s|s["kind"]=="potential_screening_match" && s["carried_forward"]!=true)
            .map(|s|s["comparison_value"].clone()).collect();
        matches.sort_by_key(Value::to_string);matches.dedup();
        let current=json!(matches);
        let key=format!("{}:{SANCTIONS}:sanctions_matches",company.kvk_number);
        let before=previous.and_then(|p|p["observations"].get(&key));
        out["observations"][&key]=current.clone();
        if before.is_some_and(|v|v!=&current) {
            if let Some(source)=sources.iter().find(|s|s.document["source"]["provider"]==SANCTIONS || s.document["source"]["api_slug"]==SANCTIONS) {
                let comparison=json!({"before":before,"after":current,"baseline_at":previous.and_then(|p|p.get("checked_at"))});
                let signal=json!({"kind":"sanctions_change","identity":"sanctions_matches","kvk_number":company.kvk_number,"company_name":company.name,
                    "source":SANCTIONS,"previous":before,"current":current,"comparison_value":comparison,"signal_id":crate::state::digest(&comparison),
                    "message":"Potential sanctions matches changed. Manual identity review required; this is not a confirmed hit or clearance.","review_required":true,
                    "evidence":[{"result_ref":source.result_ref,"pointer":"/data/responses","value":source.document["data"]["responses"],"retrieved_at":source.retrieved_at,"source":source.document["source"]}]});
                out["new_signals"].as_array_mut().unwrap().push(signal);
            }
        }
    }
    let observation_keys: BTreeSet<String> = FIELDS.iter().map(|(field, _)| format!("{}:{}:{field}", company.kvk_number,field_source(field))).collect();
    if let Some(observations) = out["observations"].as_object_mut() {
        observations.retain(|key, _| observation_keys.contains(key));
    }
    let signals: Vec<Value> = out["new_signals"].as_array().into_iter().flatten()
        .filter(|s| (s["kind"] == "registry_change" || s["kind"] == "sanctions_change" || s["kind"] == "trusted_source_change") && FIELDS.iter().any(|(field, _)| s["identity"] == *field))
        .cloned().collect();
    out["signals"] = json!(signals);
    out["new_signals"] = json!(signals);
    out["new_signal_count"] = json!(signals.len());
    out["notification_required"] = json!(!signals.is_empty());
    let source = core_sources.iter().find(|s| {
        let d = &s.document;
        (d["source"]["provider"] == PROVIDER || d["source"]["api_slug"] == PROVIDER)
            && d["data"]["kvkNummer"] == company.kvk_number
            && d["data"]["naam"].as_str().is_some_and(|n| !n.is_empty())
            && !matches!(d["coverage"].as_str(), Some("failed" | "unavailable"))
    });
    out["field_coverage"] = json!(FIELDS.iter().filter(|(field,_)|source_ids.iter().any(|s|s==field_source(field))).map(|(field, pointer)| {
        let checked = if *field == "sanctions_matches" {out["coverage"].as_array().is_some_and(|rows|rows.iter().any(|c|c["category"]=="sanctions" && c["status"]=="checked"))}
        else if field_source(field)==GLEIF {gleif.is_some_and(|(_,_,row)|row.pointer(pointer).is_some_and(|value|!value.is_null() && previous.and_then(|p|p["observations"].get(format!("{}:{GLEIF}:{field}",company.kvk_number))).is_none_or(|old|!missing_values(old,value))))}
        else if matches!(*field,"registered_names"|"branch_profile"|"financial_statements") {sources.iter().any(|s|observed_extra(s,field,company,previous))}
        else{source.is_some_and(|s| {
            let value = s.document.pointer(pointer);
            if field.ends_with("addresses") {
                value.and_then(Value::as_array).is_some_and(|rows| rows.iter().all(|r| r["indAfgeschermd"] == "Nee"))
            } else if *field == "registration_end" {
                value.is_some_and(|v| !v.is_null()) || (s.document["data"]["materieleRegistratie"].is_object()
                    && s.document["coverage"] != "partial" && s.document["missing_fields"].as_array().is_none_or(|v|v.is_empty()))
            } else { value.is_some_and(|v| !v.is_null()) }
        })};
        json!({"field":field,"source":field_source(field),"status":if checked {"checked"}else{"unknown"},
            "message":if checked {"Available in the saved selected source."}else{"Unavailable, identity-unverified or shielded; the previous observed value is retained, not treated as a deletion."}})
    }).collect::<Vec<_>>());
    let partial = out["coverage"].as_array().is_none_or(|rows| rows.iter().any(|c|c["status"] != "checked"))
        || out["field_coverage"].as_array().is_some_and(|rows| rows.iter().any(|c|c["status"] != "checked"));
    out["status"] = json!(if partial {"partial"} else {"completed"});
    out["coverage_attention_required"] = json!(partial);
    out["limitations"] = json!(["Only available selected source fields are compared. Missing or shielded fields remain unknown.",
        "Changes are observed between checks; this is not a real-time KVK event feed.","OpenSanctions matches are potential matches requiring human identity review, not confirmed hits or clearance."]);
    out["schedule"] = json!({"owner":"apiosk","created":true});
    out["delivery"] = json!({"owner":"apiosk","mode":"changes_only"});
    out
}

pub fn comparison_hash(snapshot: &Value) -> String {
    crate::state::digest(&snapshot["observations"])
}

pub fn selected_fields(fields: Vec<String>) -> anyhow::Result<Vec<String>> {
    let fields = if fields.is_empty() { FIELDS.iter().map(|(key, _)| (*key).to_owned()).collect() } else { fields };
    let unique: BTreeSet<_> = fields.iter().collect();
    anyhow::ensure!(fields.len() <= FIELDS.len() && unique.len() == fields.len()
        && fields.iter().all(|key| FIELDS.iter().any(|(allowed, _)| key == allowed)), "Select available monitoring fields without duplicates.");
    Ok(fields)
}

pub fn select(snapshot: &mut Value, fields: &[String]) {
    if let Some(observations) = snapshot["observations"].as_object_mut() {
        observations.retain(|key, _| key.rsplit(':').next().is_some_and(|field| fields.iter().any(|f| f == field)));
    }
    for key in ["signals", "new_signals"] {
        if let Some(signals) = snapshot[key].as_array_mut() {
            signals.retain(|s| fields.iter().any(|f| s["identity"] == *f));
        }
    }
    if let Some(coverage) = snapshot["field_coverage"].as_array_mut() {
        coverage.retain(|c| fields.iter().any(|f| c["field"] == *f));
    }
    let extra:Vec<Value>=snapshot["field_coverage"].as_array().into_iter().flatten().filter(|c|c["field"].as_str().is_some_and(|field|matches!(field,"registered_names"|"branch_profile"|"financial_statements")))
        .map(|c|json!({"kvk_number":snapshot["companies"][0]["kvk_number"],"category":field_operation(c["field"].as_str().unwrap()),"source":PROVIDER,"status":c["status"],"message":c["message"]})).collect();
    if let Some(coverage)=snapshot["coverage"].as_array_mut(){coverage.retain(|c|!c["category"].as_str().is_some_and(|s|s.starts_with("company.")));coverage.extend(extra);}
    let count = snapshot["new_signals"].as_array().map_or(0, Vec::len);
    snapshot["new_signal_count"] = json!(count);
    snapshot["notification_required"] = json!(count > 0);
    let partial = snapshot["coverage"].as_array().is_none_or(|rows|rows.iter().any(|c|c["status"] != "checked"))
        || snapshot["field_coverage"].as_array().is_some_and(|rows| rows.iter().any(|c| c["status"] != "checked"));
    snapshot["status"] = json!(if partial {"partial"}else{"completed"});
    snapshot["coverage_attention_required"] = json!(partial);
}

pub fn fields(snapshot: &Value) -> Vec<Value> {
    let labels = ["Naam", "Handelsnamen", "Activiteiten (SBI)", "Werkzame personen", "Statutaire naam", "Rechtsvorm", "Vestigingsadres", "Websites vestiging", "Adres eigenaar", "Websites eigenaar", "Einddatum inschrijving","Potentiële sanctiematches","Geregistreerde namen en vestigingen","Vestigingsprofiel","Jaarrekeninggegevens","LEI","GLEIF juridische naam","GLEIF juridisch adres","GLEIF entiteitsstatus","LEI registratiestatus"];
    FIELDS.iter().zip(labels).filter(|((key,_),_)|snapshot["source_ids"].as_array().is_none_or(|ids|ids.iter().any(|id|id==field_source(key)))).map(|((key,pointer), label)| {
        let kvk=snapshot["companies"][0]["kvk_number"].as_str().unwrap_or("");
        let status=snapshot["field_coverage"].as_array().into_iter().flatten().find(|c|c["field"]==*key).map(|c|c["status"].clone()).unwrap_or(json!("unknown"));
        let evidence: Vec<Value> = snapshot["sources"].as_array().into_iter().flatten().filter_map(|s| {
            let path=if field_source(key)==GLEIF {
                let saved:SavedSource=serde_json::from_value(s.clone()).ok()?;
                let company:Company=serde_json::from_value(snapshot["companies"][0].clone()).ok()?;
                let (index,_)=gleif_record(&saved,&company)?;format!("/data/data/{index}{pointer}")
            }else{(*pointer).to_owned()};
            (s["document"].pointer(&path).is_some() && s["document"]["source"]["capability"].as_str().is_none_or(|c|c==field_operation(key)))
                .then(||json!({"result_ref":s["result_ref"],"pointer":path,"retrieved_at":s["retrieved_at"],"source":s["document"]["source"]}))
        }).collect();
        json!({"key":key,"label":label,"source":field_source(key),"capability":field_operation(key),"pointer":pointer,"status":status,"value":snapshot["observations"][format!("{kvk}:{}:{key}",field_source(key))],"evidence":evidence})
    }).collect()
}

pub fn validate(configuration: &Value, sources: &[crate::recurring::Source]) -> anyhow::Result<()> {
    let monitor = &configuration["monitoring"];
    let company: Company = serde_json::from_value(monitor["company"].clone())?;
    crate::company_checks::validate_companies(std::slice::from_ref(&company))?;
    let source_ids = self::sources(serde_json::from_value(monitor["sources"].clone())?)?;
    let fields=selected_fields(serde_json::from_value(monitor["selected_fields"].clone())?)?;
    let required=operations(&fields);
    anyhow::ensure!(sources.len() == required.len() && required.iter().all(|op|sources.iter().filter(|s|s.selection["capability"]==*op && s.selection["api_slug"]==match op.as_str(){"company.sanctions-screen"=>SANCTIONS,"company.legal-entity-search"=>GLEIF,_=>PROVIDER}).count()==1), "The monitor must use exactly the selected trusted sources and operations.");
    anyhow::ensure!(monitor["baseline"]["companies"] == json!([company])
        && monitor["baseline"]["observations"].as_object().is_some_and(|v| !v.is_empty())
        && monitor["baseline"]["coverage"].as_array().is_some_and(|v| !v.is_empty() && v.iter().all(|c|c["status"] == "checked")),
        "A verified saved KVK baseline is required.");
    anyhow::ensure!(configuration["change_only"] == true, "Registry monitors report changes only.");
    anyhow::ensure!(fields.iter().all(|field|source_ids.iter().any(|source|source==field_source(field))),"A selected field needs its selected source.");
    Ok(())
}

pub async fn baseline(engine: &Engine, p: &Principal, task_id: Uuid, company: &Company, source_ids: &[String]) -> anyhow::Result<Value> {
    crate::company_checks::validate_companies(std::slice::from_ref(company))?;
    let (_, task, connection) = engine.store.load_scoped(p, task_id).await?;
    anyhow::ensure!(connection.is_none(), "Prepare this monitor in the Apiosk app.");
    let response=engine.view(p,task_id,Uuid::new_v4(),None).await?;
    anyhow::ensure!(crate::company_checks::can_snapshot(&response) && task.pending_dispatches.is_empty(),"Wait for all initial source calls and settlement to finish.");
    let plan:crate::resolver::Plan=serde_json::from_value(task.proposal.clone().ok_or_else(||anyhow::anyhow!("Missing baseline plan."))?)?;
    let documents = engine.store.results(p.owner, task_id, &task.result_refs).await?;
    let mut saved = Vec::new();
    for (result_ref, document) in documents {
        let at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT created_at FROM results WHERE id=$1 AND owner_id=$2 AND task_id=$3")
            .bind(result_ref).bind(p.owner).bind(task_id).fetch_one(&engine.store.pool).await?;
        let kvk_number=crate::company_checks::source_company(&task,&document,std::slice::from_ref(company));
        let mut document=crate::report_assets::public_document(&document);
        if let Some(step)=plan.steps.iter().find(|step|document["source"]["endpoint_id"]==step.service.endpoint_id.to_string()) {
            document["source"]["capability"]=json!(step.service.contract.capability_slug);
            if step.service.contract.capability_slug=="company.branch-profile" {
                let numbers:BTreeSet<&str>=task.facts.iter().filter(|f|f.entity_ref==step.entity_ref && f.semantic_type=="company.vestigingsnummer").filter_map(|f|f.value.as_str()).collect();
                if numbers.len()==1 {document["source"]["establishment_number"]=json!(numbers.first().unwrap());}
            }
        }
        saved.push(SavedSource { result_ref, retrieved_at:at.to_rfc3339(), kvk_number, document });
    }
    anyhow::ensure!(!saved.is_empty(), "Complete the selected source lookups before scheduling this monitor.");
    let at=saved.iter().map(|s|s.retrieved_at.as_str()).max().unwrap_or("");
    let mut baseline = snapshot_for_sources(company, &saved, None, at, source_ids);
    baseline["initial_execution_status"]=serde_json::to_value(response.status)?;
    baseline["initial_source_errors"]=serde_json::to_value(response.errors)?;
    anyhow::ensure!(baseline["coverage"][0]["status"] == "checked", "The saved sources do not verify this exact KVK identity.");
    Ok(baseline)
}

pub fn result(company: &Company, baseline: &Value, previous: Option<&Value>, sources: Vec<Value>, scheduled: chrono::DateTime<chrono::Utc>, question: &str, hash: &str) -> Value {
    let saved: Vec<SavedSource> = sources.iter().filter_map(|s| Some(SavedSource {
        result_ref:s["result_ref"].as_str()?.parse().ok()?,
        retrieved_at:s["document"]["retrieved_at"].as_str()?.to_owned(),
        kvk_number:Some(company.kvk_number.clone()), document:s["document"].clone(),
    })).collect();
    let prior = previous.map(|p| &p["monitoring"]).filter(|v| v.is_object()).unwrap_or(baseline);
    let retrieved_at = saved.iter().map(|s| s.retrieved_at.as_str()).max().map(str::to_owned)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let source_ids:Vec<String>=serde_json::from_value(baseline["source_ids"].clone()).unwrap_or_else(|_|vec![PROVIDER.into()]);
    let mut snapshot = snapshot_for_sources(company, &saved, Some(prior), &retrieved_at, &source_ids);
    let selected: Vec<String> = baseline["field_coverage"].as_array().into_iter().flatten().filter_map(|c|c["field"].as_str().map(str::to_owned)).collect();
    select(&mut snapshot, &selected);
    // Newly available fields enrich the next baseline but are not changes.
    // The delivery ledger advances only for actual field deltas; a chain also
    // detects returning to an earlier value after intermediate changes.
    let prior_hash = previous.and_then(|p| p["comparison_hash"].as_str()).map(str::to_owned)
        .unwrap_or_else(|| comparison_hash(baseline));
    let next_hash = if snapshot["new_signal_count"].as_u64().unwrap_or(0) == 0 { prior_hash }
        else { crate::state::digest(&json!([prior_hash,snapshot["new_signals"]])) };
    let observations: Vec<Value> = snapshot["new_signals"].as_array().into_iter().flatten().map(|s| json!({
        "text":format!("{}: {} → {}", s["identity"].as_str().unwrap_or("field"), s["previous"], s["current"]),
        "evidence":s["evidence"]
    })).collect();
    json!({"question":question,"scheduled_at":scheduled,"workflow_hash":hash,"processing":VERSION,
        "comparison_hash":next_hash,"sources":public_source(&json!(sources)),"monitoring":snapshot,
        "analysis":{"status":"complete","method":VERSION,"observations":observations,
            "limitations":snapshot["limitations"],"partial":snapshot["coverage_attention_required"]}})
}

pub fn summary(result: &Value) -> String {
    let snapshot = &result["monitoring"];
    let mut text = format!("{} — KVK {}\nGecontroleerd: {}\n", snapshot["companies"][0]["name"].as_str().unwrap_or("Bedrijf"),
        snapshot["companies"][0]["kvk_number"].as_str().unwrap_or(""), snapshot["checked_at"].as_str().unwrap_or(""));
    for signal in snapshot["new_signals"].as_array().into_iter().flatten() {
        if signal["kind"] == "sanctions_change" {text.push_str("\nPotentiële sanctiematches gewijzigd. Menselijke identiteitscontrole is vereist; dit is geen bevestigde match of vrijwaring.\n");}
        text.push_str(&format!("\n{}: {} → {}\n", signal["identity"].as_str().unwrap_or("Veld"),signal["previous"],signal["current"]));
        for evidence in signal["evidence"].as_array().into_iter().flatten() {
            let source=evidence["source"]["name"].as_str().or_else(||evidence["source"]["provider"].as_str()).or_else(||evidence["source"]["api_slug"].as_str()).unwrap_or("Bron");
            text.push_str(&format!("Bron: {source} · opgehaald {} · bewijs {} {}\n",evidence["retrieved_at"].as_str().unwrap_or(""),evidence["result_ref"].as_str().unwrap_or(""),evidence["pointer"].as_str().unwrap_or("")));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    fn company() -> Company { Company {name:"Fixture B.V.".into(),kvk_number:"01234567".into()} }
    fn source(data: Value) -> SavedSource {
        SavedSource {result_ref:Uuid::new_v4(),retrieved_at:"2026-10-25T09:00:00Z".into(),kvk_number:Some(company().kvk_number),
            document:json!({"source":{"api_slug":PROVIDER,"name":"KVK"},"coverage":"complete","data":data})}
    }
    fn profile() -> Value {
        json!({"kvkNummer":"01234567","naam":"Fixture B.V.","handelsnamen":[{"naam":"Fixture"},{"naam":"Fixture Bikes"}],
            "statutaireNaam":"Fixture B.V.","sbiActiviteiten":[{"sbiCode":"6201"}],"materieleRegistratie":{"datumAanvang":"20200101"},
            "_embedded":{"eigenaar":{"rechtsvorm":"Besloten Vennootschap"},"hoofdvestiging":{"adressen":[{"indAfgeschermd":"Nee","volledigAdres":"Public 1"}]}}})
    }
    #[tokio::test]
    #[ignore = "reads the authorized current catalog; no task, model, provider call or purchase"]
    async fn live_catalog_three_source_monitoring_recipe_reaches_one_complete_quote() {
        dotenvy::dotenv().ok();
        let pool=sqlx::postgres::PgPoolOptions::new().max_connections(1).after_connect(|connection,_|Box::pin(async move {
            sqlx::query("SET default_transaction_read_only=on").execute(&mut *connection).await?;Ok(())
        })).connect(&std::env::var("DATABASE_URL").expect("authorized DATABASE_URL")).await.unwrap();
        let raw:Value=sqlx::query_scalar(crate::catalog::CATALOG_SQL).fetch_one(&pool).await.unwrap();
        pool.close().await;
        let catalog=crate::catalog::Catalog::build(serde_json::from_value(raw).unwrap());
        let company=Company{name:"Heineken N.V.".into(),kvk_number:"33011433".into()};
        let ids=vec![PROVIDER.into(),SANCTIONS.into(),GLEIF.into()];
        let (mut intent,gaps)=prepare_recipe(&company,&ids,Some("000010159975"),&catalog);
        assert!(gaps.is_empty(),"{gaps:?}");
        assert_eq!(intent.goals.len(),6);
        let mut task=crate::state::Task::default();
        crate::engine::bind_subjects(&mut task,&catalog,&mut intent).unwrap();
        let plan=match crate::resolver::resolve(&catalog,&task,&intent) {
            crate::resolver::Resolution::Plan(plan)=>plan,
            other=>panic!("full monitoring recipe did not reach a quote: {other:?}"),
        };
        assert_eq!(plan.steps.len(),6);
        assert!(crate::parser::plan_coverage::uncovered_inputs(&task,&catalog,&intent,&plan).is_empty());
        assert!(crate::parser::plan_coverage::uncovered_outputs(&task,&catalog,&intent,&plan).is_empty());
        let operations: BTreeSet<_>=plan.steps.iter().map(|step|(step.service.api_slug.as_str(),step.service.contract.capability_slug.as_str())).collect();
        assert_eq!(operations,BTreeSet::from([(PROVIDER,"company.registry-profile"),(PROVIDER,"company.registered-names"),(PROVIDER,"company.financial-statements"),(PROVIDER,"company.branch-profile"),(SANCTIONS,"company.sanctions-screen"),(GLEIF,"company.legal-entity-search")]));
        let search=plan.steps.iter().find(|step|step.service.api_slug==GLEIF).unwrap();
        let inputs=search.service.inputs(&task,search.entity_ref,&catalog.vocabulary).unwrap();
        assert_eq!(inputs["filter[entity.legalName]"],"Heineken N.V.");
        assert_eq!(inputs["filter[entity.legalAddress.country]"],"NL");
        let mut prior_intent=intent.clone();prior_intent.subjects[0].values.remove("company.name");
        let mut prior_task=crate::state::Task::default();prior_intent.subjects[0].entity_ref=None;
        crate::engine::bind_subjects(&mut prior_task,&catalog,&mut prior_intent).unwrap();
        assert!(matches!(crate::resolver::resolve(&catalog,&prior_task,&prior_intent),crate::resolver::Resolution::Missing{kind,..} if kind=="company.name"),"fixture must reproduce the reported missing-name blocker");
        println!("Current catalog: 6 exact source operations, input/output coverage complete, total {} micro USD; no task or purchase.",plan.cap_atomic);
    }
    fn snap(s: &[SavedSource], prior: Option<&Value>) -> Value {
        let mut out=snapshot(&company(),s,prior,"2026-10-25T10:00:00Z");
        select(&mut out,&["name","trade_names","activities","legal_name","legal_form","addresses","registration_end"].map(str::to_owned));out
    }
    #[test]
    fn first_scheduled_unchanged_profile_matches_the_purchased_baseline() {
        let baseline = snap(&[source(profile())],None);
        let repeat = snap(&[source(profile())],Some(&baseline));
        assert_eq!(comparison_hash(&baseline),comparison_hash(&repeat));
        assert_eq!(repeat["new_signal_count"],0);
        assert_eq!(repeat["status"],"completed");
        assert_eq!(repeat["coverage"].as_array().unwrap().len(),1);
        assert!(repeat["field_coverage"].as_array().unwrap().iter().all(|f|f["status"]=="checked"));
    }
    #[test]
    fn changed_name_carries_exact_before_after_and_original_evidence() {
        let baseline = snap(&[source(profile())],None);
        let mut data=profile();data["naam"]=json!("Fixture Nieuw B.V.");
        let changed_source=source(data);
        let changed = snap(&[changed_source.clone()],Some(&baseline));
        assert_ne!(comparison_hash(&baseline),comparison_hash(&changed));
        assert_eq!(changed["new_signals"][0]["previous"],"Fixture B.V.");
        assert_eq!(changed["new_signals"][0]["current"],"Fixture Nieuw B.V.");
        assert_eq!(changed["new_signals"][0]["evidence"][0]["result_ref"],changed_source.result_ref.to_string());
        let repeat=snap(&[changed_source],Some(&changed));
        assert_eq!(repeat["new_signal_count"],0);
        assert_eq!(comparison_hash(&changed),comparison_hash(&repeat));
    }
    #[test]
    fn missing_fields_and_source_failure_preserve_last_values_but_show_unknown() {
        let baseline=snap(&[source(profile())],None);
        let missing=snap(&[source(json!({"kvkNummer":"01234567","naam":"Fixture B.V."}))],Some(&baseline));
        assert_eq!(comparison_hash(&baseline),comparison_hash(&missing));
        assert_eq!(missing["status"],"partial");
        assert_eq!(missing["field_coverage"].as_array().unwrap().iter().find(|f|f["field"]=="addresses").unwrap()["status"],"unknown");
        let failure=snap(&[],Some(&missing));
        assert_eq!(comparison_hash(&baseline),comparison_hash(&failure));
        assert_eq!(failure["coverage"][0]["status"],"unknown");
        assert_eq!(failure["source_count"],0);
    }
    #[test]
    fn identity_mismatch_never_replaces_the_baseline() {
        let baseline=snap(&[source(profile())],None);
        let mut data=profile();data["kvkNummer"]=json!("99999999");data["naam"]=json!("Unrelated B.V.");
        let wrong=snap(&[source(data)],Some(&baseline));
        assert_eq!(wrong["coverage"][0]["status"],"unknown");
        assert_eq!(wrong["new_signal_count"],0);
        assert_eq!(comparison_hash(&baseline),comparison_hash(&wrong));
    }
    #[test]
    fn array_order_metadata_and_unmonitored_employee_count_are_not_changes() {
        let baseline=snap(&[source(profile())],None);
        let mut data=profile();data["handelsnamen"].as_array_mut().unwrap().reverse();
        data["links"]=json!([{"href":"https://example.test/new-metadata"}]);data["totaalWerkzamePersonen"]=json!(999);
        let repeat=snap(&[source(data)],Some(&baseline));
        assert_eq!(comparison_hash(&baseline),comparison_hash(&repeat));
        assert_eq!(repeat["new_signal_count"],0);
    }
    #[test]
    fn shielded_address_is_unknown_not_a_business_change_or_disclosure() {
        let baseline=snap(&[source(profile())],None);
        let mut data=profile();data["_embedded"]["hoofdvestiging"]["adressen"]=json!([{"indAfgeschermd":"Ja","volledigAdres":"Secret 2"}]);
        let shielded=snap(&[source(data)],Some(&baseline));
        assert_eq!(comparison_hash(&baseline),comparison_hash(&shielded));
        assert_eq!(shielded["new_signal_count"],0);
        assert!(!shielded["observations"].to_string().contains("Secret"));
    }
    #[test]
    fn only_the_fixed_kvk_profile_is_requested() {
        let recipe=crate::company_checks::registry_recipe(&company());
        assert_eq!(recipe.goals.len(),1);
        assert_eq!(recipe.goals[0].capability,"company.registry-profile");
        assert_eq!(recipe.goals[0].constraints["source"],PROVIDER);
        assert_eq!(recipe.subjects[0].values["company_registry.kvknummer"],"01234567");
    }
    fn recurring_source(mut s: SavedSource) -> Value {
        s.document["retrieved_at"]=json!(s.retrieved_at);
        json!({"result_ref":s.result_ref,"document":s.document})
    }
    #[test]
    fn newly_available_field_saves_a_baseline_without_a_blank_alert_then_reports_changes() {
        let mut initial=profile();initial.as_object_mut().unwrap().remove("statutaireNaam");
        let baseline=snap(&[source(initial)],None);
        let scheduled="2026-10-25T08:00:00Z".parse().unwrap();
        let available=result(&company(),&baseline,None,vec![recurring_source(source(profile()))],scheduled,"Monitor","approved");
        assert_eq!(available["comparison_hash"],comparison_hash(&baseline));
        assert_eq!(available["monitoring"]["new_signal_count"],0);
        assert_eq!(available["monitoring"]["checked_at"],"2026-10-25T09:00:00Z");
        assert_eq!(available["monitoring"]["observations"]["01234567:kvk-dutch-business-register:legal_name"],"Fixture B.V.");
        let mut changed=profile();changed["statutaireNaam"]=json!("New statutory name");
        let changed=result(&company(),&baseline,Some(&available),vec![recurring_source(source(changed))],scheduled,"Monitor","approved");
        assert_ne!(changed["comparison_hash"],available["comparison_hash"]);
        assert_eq!(changed["monitoring"]["new_signal_count"],1);
        let reverted=result(&company(),&baseline,Some(&changed),vec![recurring_source(source(profile()))],scheduled,"Monitor","approved");
        assert_ne!(reverted["comparison_hash"],changed["comparison_hash"]);
        assert_eq!(reverted["monitoring"]["new_signal_count"],1);
        let repeat=result(&company(),&baseline,Some(&reverted),vec![recurring_source(source(profile()))],scheduled,"Monitor","approved");
        assert_eq!(repeat["comparison_hash"],reverted["comparison_hash"]);
        assert_eq!(repeat["monitoring"]["new_signal_count"],0);
    }
    #[test]
    fn selected_fields_do_not_send_unselected_profile_changes() {
        let mut baseline=snap(&[source(profile())],None);select(&mut baseline,&["name".into()]);
        let mut data=profile();data["sbiActiviteiten"]=json!([{"sbiCode":"9999"}]);
        let checked=result(&company(),&baseline,None,vec![recurring_source(source(data))],chrono::Utc::now(),"Monitor","approved");
        assert_eq!(checked["monitoring"]["new_signal_count"],0);
        assert_eq!(checked["comparison_hash"],comparison_hash(&baseline));
        assert_eq!(checked["monitoring"]["field_coverage"].as_array().unwrap().len(),1);
    }
    fn sanctions(rows:Value,total:usize)->SavedSource {
        SavedSource{result_ref:Uuid::new_v4(),retrieved_at:"2026-10-25T09:05:00Z".into(),kvk_number:Some("01234567".into()),
            document:json!({"source":{"api_slug":SANCTIONS,"capability":"company.sanctions-screen"},"coverage":"complete","data":{"responses":{"q1":{"status":200,"results":rows,"total":{"value":total,"relation":"eq"}}}}})}
    }
    #[test]
    fn sanctions_changes_are_potential_matches_with_evidence_never_clearance() {
        let ids=vec![PROVIDER.into(),SANCTIONS.into()];
        let mut baseline=snapshot_for_sources(&company(),&[source(profile()),sanctions(json!([]),0)],None,"2026-10-25T09:05:00Z",&ids);
        select(&mut baseline,&["name".into(),"sanctions_matches".into()]);
        assert_eq!(baseline["coverage"][1]["status"],"checked");
        let rows=json!([{"id":"example-id","caption":"Potential name overlap","match":true,"score":0.9,"properties":{"topics":["sanction"]},"datasets":["fixture-list"]}]);
        let scheduled=chrono::Utc::now();
        let hit=result(&company(),&baseline,None,vec![recurring_source(source(profile())),recurring_source(sanctions(rows.clone(),1))],scheduled,"Monitor","approved");
        assert_eq!(hit["monitoring"]["new_signal_count"],1);
        assert_eq!(hit["monitoring"]["coverage"][1]["status"],"checked");
        assert_eq!(hit["monitoring"]["new_signals"][0]["kind"],"sanctions_change");
        assert_eq!(hit["monitoring"]["new_signals"][0]["review_required"],true);
        assert_eq!(hit["monitoring"]["new_signals"][0]["evidence"][0]["pointer"],"/data/responses");
        let same=result(&company(),&baseline,Some(&hit),vec![recurring_source(source(profile())),recurring_source(sanctions(rows,1))],scheduled,"Monitor","approved");
        assert_eq!(same["monitoring"]["new_signal_count"],0);assert_eq!(same["comparison_hash"],hit["comparison_hash"]);
        let removed=result(&company(),&baseline,Some(&same),vec![recurring_source(source(profile())),recurring_source(sanctions(json!([]),0))],scheduled,"Monitor","approved");
        assert_eq!(removed["monitoring"]["new_signal_count"],1);
        assert_eq!(removed["monitoring"]["coverage"][1]["status"],"checked");
        assert!(removed["monitoring"]["new_signals"][0]["message"].as_str().unwrap().contains("not a confirmed hit or clearance"));
        let gap=result(&company(),&baseline,Some(&same),vec![recurring_source(source(profile())),recurring_source(sanctions(json!([]),10))],scheduled,"Monitor","approved");
        assert_eq!(gap["monitoring"]["coverage"][1]["status"],"unknown");
        assert_eq!(gap["monitoring"]["new_signal_count"],0);assert_eq!(gap["comparison_hash"],same["comparison_hash"]);
    }
    #[test]
    fn positive_screening_match_without_classification_retains_prior_observation_as_unknown() {
        let ids=vec![PROVIDER.into(),SANCTIONS.into()];
        let known=json!({"id":"example-id","schema":"Company","match":true,"properties":{"topics":["sanction"]},"datasets":["fixture-list"]});
        let mut baseline=snapshot_for_sources(&company(),&[source(profile()),sanctions(json!([known.clone()]),1)],None,"2026-10-25T09:05:00Z",&ids);
        select(&mut baseline,&["name".into(),"sanctions_matches".into()]);
        assert_eq!(baseline["observations"]["01234567:opensanctions:sanctions_matches"].as_array().unwrap().len(),1);
        for topics in [None,Some(Value::Null),Some(json!([])),Some(json!("sanction")),Some(json!(["sanction",1])),Some(json!([" "]))] {
            let mut candidate=known.clone();
            candidate["properties"]=json!({});
            if let Some(topics)=topics {candidate["properties"]["topics"]=topics;}
            let source=sanctions(json!([candidate.clone()]),1);
            let first=snapshot_for_sources(&company(),&[self::source(profile()),source.clone()],None,"2026-10-26T09:05:00Z",&ids);
            assert!(first["observations"].get("01234567:opensanctions:sanctions_matches").is_none());
            let checked=result(&company(),&baseline,None,vec![recurring_source(self::source(profile())),recurring_source(source)],chrono::Utc::now(),"Monitor","approved");
            let monitor=&checked["monitoring"];
            assert_eq!(monitor["coverage"][1]["status"],"unknown");
            assert!(monitor["coverage"][1]["message"].as_str().unwrap().contains("classification is unavailable"));
            assert_eq!(monitor["field_coverage"][1]["status"],"unknown");
            assert_eq!(monitor["observations"],baseline["observations"]);
            assert_eq!(monitor["new_signal_count"],0);
            assert_eq!(checked["comparison_hash"],comparison_hash(&baseline));
            assert_eq!(monitor["sources"][1]["document"]["data"]["responses"]["q1"]["results"][0],candidate);
        }
    }
    fn extra(field:&str,data:Value)->SavedSource {
        let mut s=source(data);s.document["source"]["capability"]=json!(field_operation(field));
        if field=="branch_profile" {s.document["source"]["establishment_number"]=json!("000012345678");}s
    }
    #[test]
    fn branch_identity_shielding_and_missing_nested_data_do_not_create_false_changes() {
        let original=json!({"kvkNummer":"01234567","vestigingsnummer":"000012345678","totaalWerkzamePersonen":5});
        let mut baseline=snapshot(&company(),&[source(profile()),extra("branch_profile",original.clone())],None,"2026-10-25T09:00:00Z");
        select(&mut baseline,&["branch_profile".into()]);
        for data in [json!({"kvkNummer":"01234567","vestigingsnummer":"000012345678"}),
            json!({"kvkNummer":"01234567","vestigingsnummer":"999999999999","totaalWerkzamePersonen":5}),
            json!({"kvkNummer":"99999999","vestigingsnummer":"000012345678","totaalWerkzamePersonen":5}),
            json!({"kvkNummer":"01234567","vestigingsnummer":"000012345678","totaalWerkzamePersonen":5,"adressen":[{"indAfgeschermd":"Ja","volledigAdres":"Secret"}]})] {
            let mut check=snapshot(&company(),&[source(profile()),extra("branch_profile",data)],Some(&baseline),"2026-10-26T09:00:00Z");
            select(&mut check,&["branch_profile".into()]);
            assert_eq!(check["field_coverage"][0]["status"],"unknown");assert_eq!(check["new_signal_count"],0);
            assert_eq!(check["observations"],baseline["observations"]);assert!(!check["sources"].to_string().contains("Secret"));
        }
    }
    #[test]
    fn unavailable_accounts_are_not_an_empty_successful_filing() {
        for data in [json!({}),json!({"error":"unavailable"}),json!({"opendataFields":[]})] {
            assert!(!usable_extra(&extra("financial_statements",data),"financial_statements",&company()));
        }
        assert!(usable_extra(&extra("financial_statements",json!({"opendataFields":[{"key":"FinancialYear","value":"2024"},{"key":"BalanceSheet","opendataFields":[{"key":"Assets","value":"100"}]}]})),"financial_statements",&company()));
    }
    fn gleif(data:Value)->SavedSource {
        SavedSource{result_ref:Uuid::new_v4(),retrieved_at:"2026-10-25T09:05:00Z".into(),kvk_number:Some("01234567".into()),document:json!({"source":{"api_slug":GLEIF,"capability":"company.legal-entity-search"},"data":{"data":data}})}
    }
    fn lei_record(number:&str,name:&str)->Value {json!({"id":"LEI-FIXTURE","attributes":{"lei":"LEI-FIXTURE","entity":{"registeredAt":{"id":"RA000463"},"registeredAs":number,"legalName":{"name":name},"status":"ACTIVE","legalAddress":{"country":"NL","city":"Amsterdam"}},"registration":{"status":"ISSUED"}}})}
    #[test]
    fn gleif_requires_a_unique_exact_authority_registration_and_keeps_real_evidence_pointer() {
        let ids=vec![PROVIDER.into(),GLEIF.into()];
        let mut baseline=snapshot_for_sources(&company(),&[source(profile()),gleif(json!([lei_record("01234567","Original")]))],None,"2026-10-25T09:05:00Z",&ids);
        select(&mut baseline,&["gleif_legal_name".into()]);
        let field=fields(&baseline).into_iter().find(|f|f["key"]=="gleif_legal_name").unwrap();
        assert_eq!(field["evidence"][0]["pointer"],"/data/data/0/attributes/entity/legalName/name");
        let changed=result(&company(),&baseline,None,vec![recurring_source(source(profile())),recurring_source(gleif(json!([lei_record("01234567","Changed")])) )],chrono::Utc::now(),"Monitor","hash");
        assert_eq!(changed["monitoring"]["new_signal_count"],1);
        for rows in [json!([lei_record("99999999","Original")]),json!([lei_record("01234567","One"),lei_record("01234567","Two")])] {
            let unknown=snapshot_for_sources(&company(),&[source(profile()),gleif(rows)],Some(&baseline),"2026-10-26T09:00:00Z",&ids);
            assert_eq!(unknown["coverage"].as_array().unwrap().iter().find(|c|c["category"]=="gleif").unwrap()["status"],"unknown");
        }
        let mut wrong=lei_record("01234567","Same name");wrong["attributes"]["entity"]["registeredAt"]["id"]=json!("RA000602");
        assert!(gleif_record(&gleif(json!([wrong])),&company()).is_none());
    }
}
