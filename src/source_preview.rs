//! Readable projections of large pages; raw data remains available through paging.
use serde_json::{json,Value};
pub fn project(provider:&str,data:&Value)->Option<Value> {
    let preview=match provider {
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
