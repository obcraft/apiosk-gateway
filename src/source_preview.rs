//! Readable projections of large pages; raw data remains available through paging.
use serde_json::{json,Value};
pub fn project(provider:&str,data:&Value)->Option<Value> {
    let preview=match provider {
        "koop" | "rdw" | "rvo" => {
            let rows=data["items"].as_array()?;
            if rows.len()>50 {return None}
            let mut preview=data.clone();
            let fields:&[&str]=match provider {
                "koop"=>&["id","title","authority","type","published","modified","url"],
                "rdw"=>&["kenteken","merk","handelsbenaming","voertuigsoort","vervaldatum_apk_dt","vervaldatum_apk","datum_eerste_toelating_dt","datum_eerste_toelating"],
                _=>&["id","title","audiences","sectors","modified","application_status","eligibility","url"],
            };
            preview["items"]=Value::Array(rows.iter().map(|r|Value::Object(fields.iter().filter_map(|k|r.get(*k).map(|v|((*k).to_owned(),v.clone()))).collect())).collect());
            preview
        },
        "ted" | "pdok" if data["items"].is_array() => {
            let rows=data["items"].as_array()?;
            if rows.len()>100{return None}
            let mut preview=data.clone();
            if let Some(obj)=preview.as_object_mut(){obj.remove("matched");}
            preview["items"]=Value::Array(rows.iter().map(|r|{let mut row=r.clone();if let Some(obj)=row.as_object_mut(){obj.remove("source_record");obj.remove("links");obj.remove("buyer-name");obj.remove("winner-name");}row}).collect());
            preview
        },
        "data-overheid-nl" => {
            let rows=data.pointer("/result/results")?.as_array()?;
            if rows.len()>100 {return None}
            json!({"success":data["success"],"result":{"count":data["result"]["count"],"results":rows.iter().map(|r|json!({"title":r["title"],"publisher":r["publisher"],"resources":r["resources"].as_array().map(|v|v.iter().map(|r|json!({"url":r["url"]})).collect::<Vec<_>>())})).collect::<Vec<_>>()}})
        },
        "gleif" => {
            let rows=data["data"].as_array()?;
            if rows.len()>100 {return None}
            json!({"meta":data["meta"],"data":rows.iter().map(|r|json!({"id":r["id"],"attributes":{"entity":{"legalName":r.pointer("/attributes/entity/legalName"),"status":r.pointer("/attributes/entity/status"),"legalAddress":{"country":r.pointer("/attributes/entity/legalAddress/country")}},"registration":{"status":r.pointer("/attributes/registration/status")}}})).collect::<Vec<_>>()})
        },
        _=>return None,
    };
    (serde_json::to_vec(&preview).ok()?.len()<=24*1024).then_some(preview)
}

/// A partial search can omit a competing identity; never turn its top hit into
/// authority for another paid lookup. Keep the original response inspectable.
pub fn sirenic_search_incomplete(data: &serde_json::Value) -> bool {
    data["resultats_partiels"] == true
        || data["aucune_correspondance_fiable"] == true
        || ["registres_muets", "etages_abandonnes"].iter().any(|key|
            data[*key].as_array().is_some_and(|items| !items.is_empty()))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_preserves_every_returned_dataset_and_publisher_without_calling_it_measurements(){
        let data=json!({"success":true,"result":{"count":87,"results":[{"title":"Water","publisher":"Province","organization":{"title":"Catalogue"},"resources":[{"url":"https://example.org/a"},{"url":"https://example.org/b"}],"notes":"large irrelevant description"}]}});
        let p=project("data-overheid-nl",&data).unwrap();
        assert_eq!(p["result"]["count"],87);
        assert_eq!(p["result"]["results"][0]["publisher"],"Province");
        assert_eq!(p["result"]["results"][0]["resources"].as_array().unwrap().len(),2);
        assert!(p["result"]["results"][0].get("organization").is_none());
        assert_eq!(data["result"]["results"][0]["notes"],"large irrelevant description");
    }
}
