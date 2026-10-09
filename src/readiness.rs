//! One assessment for public cards, source browsing and the intent model.
//! Presence is not correctness; sample checks are not live upstream tests.
use crate::catalog::Catalog;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Metadata {
    pub provider_slug: Option<String>,
    pub description: String,
    pub category: String,
    pub tags: Vec<String>,
    pub capability_descriptions: Value,
    pub endpoints: Vec<crate::catalog::ListedEndpoint>,
    pub identity_documented: bool,
    pub endpoint_count: usize,
    pub described: usize,
    pub tagged: usize,
    pub categorized: usize,
    pub documented_inputs: usize,
    pub inferred: usize,
    pub unknown_provenance: usize,
    pub recent_successes: usize,
    pub recent_failures: usize,
    pub recent_unresolved: usize,
    pub coverage_notice: Option<String>,
}

pub fn assess(catalog: &Catalog, slugs: &[&str]) -> Value {
    let rows: Vec<_> = catalog.known_sources.iter().filter(|s| slugs.contains(&s.slug.as_str())).collect();
    let services: Vec<_> = catalog.services.iter().filter(|s| slugs.contains(&s.api_slug.as_str())).collect();
    let total: usize = rows.iter().map(|s| s.metadata.endpoint_count).sum();
    let covered = services.iter().map(|s| s.endpoint_id).collect::<BTreeSet<_>>().len();
    let sampled = services.iter().filter(|s| s.sample_payload.is_some() && !s.is_document_retrieval()).map(|s| s.endpoint_id).collect::<BTreeSet<_>>().len();
    let documents = services.iter().filter(|s|s.is_document_retrieval()).map(|s|s.endpoint_id).collect::<BTreeSet<_>>().len();
    let blocks: Vec<_> = rows.iter().filter_map(|s| s.block_reason.as_ref().map(|r| format!("{}: {}",s.name,r))).collect();
    let identity = !rows.is_empty() && rows.iter().all(|s| s.metadata.identity_documented
        && !["x402 endpoints", "x402 seller", "unknown", "unnamed", "api provider", "data provider"].contains(&s.name.trim().to_lowercase().as_str()));
    let sum = |f: fn(&Metadata) -> usize| rows.iter().map(|s| f(&s.metadata)).sum::<usize>();
    let described = sum(|m| m.described); let tagged = sum(|m| m.tagged);
    let categorized = sum(|m| m.categorized); let inputs = sum(|m| m.documented_inputs);
    let inferred = sum(|m| m.inferred); let unknown = sum(|m| m.unknown_provenance);
    let successes=sum(|m| m.recent_successes); let failures=sum(|m| m.recent_failures); let unresolved=sum(|m| m.recent_unresolved);
    let execution_concern = unresolved > 0 || (failures > 0 && successes == 0);
    let complete = total > 0 && described == total && tagged == total && categorized == total && inputs == total && inferred == 0 && unknown == 0;
    // A mixed provider remains partially supported when some listings are blocked.
    let status = if !rows.is_empty() && blocks.len() == rows.len() { "blocked" }
        else if covered == 0 { "discovery-only" }
        else if blocks.is_empty() && !execution_concern && identity && complete && covered == total && sampled == total { "planner-ready" }
        else { "partially-supported" };
    let mut reasons = blocks.clone();
    if !identity { reasons.push("Source identity or official documentation is incomplete.".into()); }
    if covered < total { reasons.push(format!("{covered} of {total} registered endpoints have runtime-accepted execution contracts.")); }
    if !complete { reasons.push("Endpoint metadata is incomplete, inferred, or lacks provenance.".into()); }
    if covered == 0 && blocks.is_empty() { reasons.push("No executable capability is available to the v2 planner.".into()); }
    if execution_concern { reasons.push("Recent v2 executions are unresolved or have failures without a recorded success.".into()); }
    if documents>0 {reasons.push(format!("{documents} endpoints retrieve opaque source documents using documented inputs. Their adapter fixtures are not provider samples or proof of live success; no normalized facts are promised."));}
    json!({"status":status,"available_in_v2":covered>0,"reasons":reasons,
        "identity":{"documented":identity},
        "metadata":{"complete":complete,"described_endpoints":described,"tagged_endpoints":tagged,"categorized_endpoints":categorized,"documented_input_endpoints":inputs,"inferred_endpoints":inferred,"unknown_provenance_endpoints":unknown},
        "contracts":{"registered_endpoints":total,"supported_endpoints":covered,"accepted_contracts":services.len(),"sample_checked_endpoints":sampled,"document_retrieval_endpoints":documents},
        "execution_evidence":{"window_days":30,"state":if successes+failures+unresolved==0 {"unmeasured"} else {"recorded"},"successful_executions":successes,"failed_executions":failures,"unresolved_executions":unresolved,"scope":"Recorded v2 execution outcomes; not a data-accuracy certification or an exhaustive live endpoint test."},
        "restrictions":blocks,"coverage_notices":rows.iter().filter_map(|s| s.metadata.coverage_notice.as_ref()).collect::<Vec<_>>()})
}

pub fn public_report(catalog: &Catalog) -> Value {
    let sources: Vec<_> = catalog.known_sources.iter().map(|s| json!({
        "slug":s.slug,"provider_slug":s.metadata.provider_slug,"name":s.name,
        "readiness":assess(catalog,&[s.slug.as_str()])
    })).collect();
    let handles = catalog.known_sources.iter().filter_map(|s| s.metadata.provider_slug.as_deref()).collect::<BTreeSet<_>>();
    let providers: Vec<_> = handles.into_iter().map(|handle| {
        let slugs: Vec<_> = catalog.known_sources.iter().filter(|s| s.metadata.provider_slug.as_deref()==Some(handle)).map(|s| s.slug.as_str()).collect();
        json!({"username":handle,"readiness":assess(catalog,&slugs)})
    }).collect();
    json!({"audit_version":1,"assessed_at":chrono::Utc::now(),"sources":sources,"providers":providers,
        "notice":"Readiness describes registered operations, not the full upstream API. Metadata and recorded execution outcomes are assessed separately; unmeasured is not proven reliable."})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{KnownSource, Snapshot};
    fn fixture() -> (Catalog, String) {
        let snapshot: Snapshot = serde_json::from_str(include_str!("../tests/fixtures/integration-catalog.json")).unwrap();
        let mut catalog=Catalog::build(snapshot);
        let slug=catalog.services[0].api_slug.clone();
        let total=catalog.services.iter().filter(|s| s.api_slug==slug).map(|s| s.endpoint_id).collect::<BTreeSet<_>>().len();
        catalog.known_sources=vec![KnownSource { slug:slug.clone(), name:"Documented source".into(), block_reason:None,
            metadata:Metadata { identity_documented:true, endpoint_count:total, described:total, tagged:total, categorized:total, documented_inputs:total, ..Default::default() } }];
        (catalog,slug)
    }
    #[test]
    fn full_coverage_is_ready_without_inventing_live_evidence() {
        let (catalog,slug)=fixture();let r=assess(&catalog,&[&slug]);
        assert_eq!(r["status"],"planner-ready");
        assert_eq!(r["execution_evidence"]["state"],"unmeasured");
    }
    #[test]
    fn inference_and_missing_contracts_make_support_partial() {
        let (mut c,s)=fixture();c.known_sources[0].metadata.inferred=1;
        assert_eq!(assess(&c,&[&s])["status"],"partially-supported");
        c.known_sources[0].metadata.inferred=0;c.known_sources[0].metadata.endpoint_count+=1;
        assert_eq!(assess(&c,&[&s])["status"],"partially-supported");
    }
    #[test]
    fn well_tagged_sources_without_contracts_are_discovery_only() {
        let (mut c,s)=fixture();c.services.clear();
        assert_eq!(assess(&c,&[&s])["status"],"discovery-only");
    }
    #[test]
    fn blocked_source_is_unavailable_in_parser_and_public_audit() {
        let (c,s)=fixture();let mut known=c.known_sources.clone();known[0].block_reason=Some("Authorization missing".into());
        let c=Catalog::build(Snapshot {services:c.services, vocabulary:c.vocabulary.into_values().collect(),known_sources:known});
        assert_eq!(assess(&c,&[&s])["status"],"blocked");
        assert_eq!(assess(&c,&[&s])["available_in_v2"],false);
        assert_eq!(c.parser_view()["sources"][0]["readiness"],public_report(&c)["sources"][0]["readiness"]);
    }
    #[test]
    fn mixed_provider_does_not_hide_a_blocked_listing() {
        let (mut c,s)=fixture();let mut blocked=c.known_sources[0].clone();blocked.slug="restricted".into();blocked.block_reason=Some("Restricted".into());c.known_sources.push(blocked);
        let r=assess(&c,&[&s,"restricted"]);assert_eq!(r["status"],"partially-supported");assert_eq!(r["restrictions"].as_array().unwrap().len(),1);
    }
    #[test]
    fn unknown_identity_cannot_be_ready_and_aliases_do_not_inflate_coverage() {
        let (mut c,s)=fixture();let before=assess(&c,&[&s])["contracts"]["supported_endpoints"].clone();c.services.push(c.services[0].clone());
        assert_eq!(assess(&c,&[&s])["contracts"]["supported_endpoints"],before);
        c.known_sources[0].metadata.identity_documented=false;assert_eq!(assess(&c,&[&s])["status"],"partially-supported");
    }
}
