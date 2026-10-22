//! Server-issued discovery evidence, shared across replicas by the existing
//! signing key. This token grants no execution or payment authority.
use crate::{auth::Principal, capability_intake::{Intake, Match, ParsedRequest}, state::{digest, Signer}};
use anyhow::{ensure, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::ask_v2::source_options::SourceOption;

const MAX_BYTES: usize = 128 * 1024;
const MAX_TOKEN_BYTES: usize = (MAX_BYTES * 4).div_ceil(3) + 65;
const TTL_SECONDS: i64 = 15 * 60;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Snapshot {
    owner: uuid::Uuid,
    workspace: Option<uuid::Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    connection: Option<uuid::Uuid>,
    parsed_digest: String,
    expires: i64,
    catalog_version: String,
    matches: Vec<Match>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    source_options: Vec<SourceOption>,
    sources_searched: usize,
    /// The validated output of the question's one model request. This grants
    /// no purchase authority; resolution and pricing still run when adopted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    planning_reply: Option<crate::parser::ChatReply>,
}
impl Snapshot {
    #[cfg(test)]
    pub(crate) fn issue(signer: &Signer, person: &Principal, parsed: &ParsedRequest, response: &Value) -> Result<String> {
        Self::issue_planned(signer, person, parsed, response, None)
    }
    pub(crate) fn issue_planned(signer: &Signer, person: &Principal, parsed: &ParsedRequest, response: &Value, planning_reply: Option<crate::parser::ChatReply>) -> Result<String> {
        ensure!(person.is_person(), "search snapshot requires an App person");
        Self::issue_scoped(signer, person, parsed, response, planning_reply)
    }
    pub(crate) fn issue_connected(signer: &Signer, principal: &Principal, parsed: &ParsedRequest, response: &Value) -> Result<String> {
        ensure!(principal.connection.is_some(), "connected search requires a connection");
        Self::issue_scoped(signer, principal, parsed, response, None)
    }
    fn issue_scoped(signer: &Signer, person: &Principal, parsed: &ParsedRequest, response: &Value, planning_reply: Option<crate::parser::ChatReply>) -> Result<String> {
        let snapshot = Self {
            owner: person.owner, workspace: person.workspace, connection: person.connection, parsed_digest: digest(parsed),
            expires: chrono::Utc::now().timestamp() + TTL_SECONDS,
            // External directory searches have no execution catalog version.
            // They are signed too, but can only contribute discovery metadata.
            catalog_version: response["catalog_version"].as_str().ok_or_else(|| anyhow::anyhow!("search catalog missing"))?.to_owned(),
            matches: serde_json::from_value(response["matches"].clone())?,
            source_options: serde_json::from_value(response.get("source_options").cloned().unwrap_or_else(|| serde_json::json!([])))?,
            sources_searched: response["apiosk_sources_searched"].as_u64().unwrap_or(0) as usize,
            planning_reply,
        };
        snapshot.seal(signer)
    }
    fn seal(&self, signer: &Signer) -> Result<String> {
        let payload = serde_json::to_vec(self)?;
        ensure!(payload.len() <= MAX_BYTES, "search snapshot too large");
        Ok(format!("{}.{}", URL_SAFE_NO_PAD.encode(&payload), signer.search_signature(&payload)))
    }
    /// Verify the signature and caller before consulting a saved receipt.
    /// Freshness is checked separately: old completed requests remain recoverable.
    pub(crate) fn open(signer: &Signer, person: &Principal, parsed: &ParsedRequest, token: &str) -> Result<Self> {
        ensure!(person.is_person(), "invalid search snapshot");
        Self::open_scoped(signer, person, parsed, token)
    }
    pub(crate) fn open_connected(signer: &Signer, principal: &Principal, parsed: &ParsedRequest, token: &str) -> Result<Self> {
        ensure!(principal.connection.is_some(), "invalid connected search snapshot");
        Self::open_scoped(signer, principal, parsed, token)
    }
    fn open_scoped(signer: &Signer, person: &Principal, parsed: &ParsedRequest, token: &str) -> Result<Self> {
        ensure!(token.len() <= MAX_TOKEN_BYTES, "invalid search snapshot");
        let (payload, signature) = token.split_once('.').ok_or_else(|| anyhow::anyhow!("invalid search snapshot"))?;
        ensure!(signature.len() == 64, "invalid search signature");
        let payload = URL_SAFE_NO_PAD.decode(payload)?;
        ensure!(payload.len() <= MAX_BYTES, "search snapshot too large");
        signer.verify_search(&payload, signature)?;
        let snapshot: Self = serde_json::from_slice(&payload)?;
        ensure!(snapshot.owner == person.owner && snapshot.workspace == person.workspace && snapshot.connection == person.connection
            && snapshot.parsed_digest == digest(parsed), "search belongs to another request");
        Ok(snapshot)
    }
    pub(crate) fn current(&self, catalog_version: &str) -> bool {
        self.expires > chrono::Utc::now().timestamp() && self.catalog_version == catalog_version
    }
    pub(crate) fn source_options(&self) -> &[SourceOption] { &self.source_options }
    pub(crate) fn planning_reply(&self) -> Option<crate::parser::ChatReply> { self.planning_reply.clone() }
    pub(crate) fn search_response(&self) -> Value {
        serde_json::json!({"matches":self.matches,"source_options":self.source_options,
            "catalog_version":self.catalog_version,"apiosk_sources_searched":self.sources_searched,
            "timing":{"search_ms":0}})
    }
    pub(crate) fn with_discovery(mut self, discovery: Self) -> Result<Self> {
        ensure!(self.owner == discovery.owner && self.workspace == discovery.workspace && self.connection == discovery.connection
            && self.parsed_digest == discovery.parsed_digest, "discovery belongs to another request");
        ensure!(!self.catalog_version.is_empty() && discovery.catalog_version.is_empty()
            && discovery.planning_reply.is_none()
            && discovery.source_options.is_empty()
            && discovery.matches.iter().all(|found| found.operations.is_empty() && found.apiosk.is_empty()
                && found.coinbase.iter().all(|candidate| candidate.origin == "coinbase"
                    && matches!(candidate.availability.as_str(), "discovery_only" | "supported")
                    && candidate.capability.as_deref().is_none_or(|cap| cap == "external.request")
                    && candidate.endpoint_id.is_none())), "invalid external discovery snapshot");
        self.expires = self.expires.min(discovery.expires);
        // Bazaar queries are generic per capability; the local snapshot owns
        // every subject occurrence and all executable operation selections.
        for found in &mut self.matches {
            if let Some(external) = discovery.matches.iter().find(|external| external.slug == found.slug) {
                found.coinbase = external.coinbase.clone();
                found.coinbase_status = external.coinbase_status.clone();
            }
        }
        Ok(self)
    }
    pub(crate) fn intake(self, parsed: ParsedRequest) -> Intake {
        Intake {
            parsed_request: parsed,
            selected_capabilities: self.matches.iter().flat_map(|found| found.operations.iter().cloned()).collect(),
            matches: self.matches, apiosk_sources_searched: self.sources_searched, catalog_version: self.catalog_version,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    #[test]
    fn planned_reply_survives_signed_search_without_conferring_execution_authority() {
        let (signer,person,parsed,response)=fixture();
        let reply=crate::parser::ChatReply {message:"Which company should I search?".into(),discover:false,
            intent:None,decline_reason:Some("missing_subject".into()),output_bindings:vec![]};
        let token=Snapshot::issue_planned(&signer,&person,&parsed,&response,Some(reply)).unwrap();
        let snapshot=Snapshot::open(&signer,&person,&parsed,&token).unwrap();
        assert_eq!(snapshot.planning_reply().unwrap().message,"Which company should I search?");
        let expected: Vec<Match> = serde_json::from_value(response["matches"].clone()).unwrap();
        assert_eq!(snapshot.search_response()["matches"],serde_json::json!(expected));
        let mut other=parsed.clone();
        other.meta.text_sha256="another question".into();
        assert!(Snapshot::open(&signer,&person,&other,&token).is_err());
        let mut tampered=token.into_bytes();
        tampered[8]=if tampered[8]==b'A' {b'B'} else {b'A'};
        assert!(Snapshot::open(&signer,&person,&parsed,std::str::from_utf8(&tampered).unwrap()).is_err());
    }
    #[test]
    fn connected_search_is_bound_to_connection_question_and_workspace() {
        let (signer,mut person,parsed,response)=fixture();
        person.connection=Some(Uuid::new_v4());
        let token=Snapshot::issue_connected(&signer,&person,&parsed,&response).unwrap();
        assert!(Snapshot::open_connected(&signer,&person,&parsed,&token).unwrap().current("catalog-a"));
        assert!(Snapshot::open(&signer,&person,&parsed,&token).is_err());
        let mut other=person.clone();other.connection=Some(Uuid::new_v4());
        assert!(Snapshot::open_connected(&signer,&other,&parsed,&token).is_err());
        other=person.clone();other.workspace=None;
        assert!(Snapshot::open_connected(&signer,&other,&parsed,&token).is_err());
        other=person.clone();other.connection=None;
        assert!(Snapshot::open(&signer,&other,&parsed,&token).is_err());
        let mut changed=parsed.clone();changed.meta.text_sha256="different question".into();
        assert!(Snapshot::open_connected(&signer,&person,&changed,&token).is_err());
    }
    fn fixture() -> (Signer, Principal, ParsedRequest, Value) {
        let signer = Signer::new(vec![7; 32]).unwrap();
        let person = Principal { owner:Uuid::new_v4(),workspace:Some(Uuid::new_v4()),connection:None };
        let parsed = crate::capability_intake::normalize(&json!({"language":"en","subjects":[],"capabilities":[{
            "slug":"company.profile","name":"Company profile","description":"Company registration","domain":"company","inputs":[],"outputs":[],"subject_id":null,"source_hint":null,"confidence":0.9}],"deliverable":{"format":"chat","operations":[]}}).to_string(),"Find a company profile","fixture").unwrap();
        let response = json!({"catalog_version":"catalog-a","apiosk_sources_searched":1,"matches":[{
            "slug":"company.profile","query":"Company profile","operations":["company.registry"],
            "apiosk":[],"coinbase":[],"coinbase_status":"ready","answer_only":false}]});
        (signer,person,parsed,response)
    }
    #[test]
    fn snapshot_is_portable_exact_scoped_and_expires_without_changing_saved_matches() {
        let (signer,person,parsed,response) = fixture();
        let token = Snapshot::issue(&signer,&person,&parsed,&response).unwrap();
        let other_replica = Signer::new(vec![7;32]).unwrap();
        let mut snapshot = Snapshot::open(&other_replica,&person,&parsed,&token).unwrap();
        assert!(snapshot.current("catalog-a"));
        assert!(!snapshot.current("catalog-b"));
        snapshot.expires = chrono::Utc::now().timestamp()-1;
        let expired = snapshot.seal(&signer).unwrap();
        let saved = Snapshot::open(&signer,&person,&parsed,&expired).unwrap();
        assert!(!saved.current("catalog-a"));
        let intake = saved.intake(parsed.clone());
        assert_eq!(intake.matches[0].operations.iter().next().unwrap(),"company.registry");
        assert_eq!(serde_json::to_value(&intake.parsed_request).unwrap(),serde_json::to_value(&parsed).unwrap());
        let mut other = person.clone(); other.owner = Uuid::new_v4();
        assert!(Snapshot::open(&signer,&other,&parsed,&token).is_err());
        other = person.clone(); other.workspace = None;
        assert!(Snapshot::open(&signer,&other,&parsed,&token).is_err());
        other = person.clone(); other.connection = Some(Uuid::new_v4());
        assert!(Snapshot::open(&signer,&other,&parsed,&token).is_err());
        let mut changed = parsed.clone(); changed.capabilities[0].outputs.push("unseen_output".into());
        assert!(Snapshot::open(&signer,&person,&changed,&token).is_err());
    }
    #[test]
    fn external_discovery_survives_planning_without_becoming_an_execution_contract() {
        let (signer, person, parsed, response) = fixture();
        let local = Snapshot::issue(&signer, &person, &parsed, &response).unwrap();
        let mut external = response.clone();
        external["catalog_version"] = json!("");
        external["matches"][0]["operations"] = json!([]);
        external["matches"][0]["coinbase_status"] = json!("complete");
        external["matches"][0]["coinbase"] = json!([{
            "origin":"coinbase","name":"Matching x402 service","description":"Directory description",
            "source_slug":null,"capability":"external.request","endpoint_id":null,
            "url":"https://example.test/service","method":"GET","price":"0.040000 USD",
            "network":"eip155:8453","calls_30d":null,"availability":"supported"
        }]);
        let token = Snapshot::issue(&signer, &person, &parsed, &external).unwrap();
        let read = |token: &str| Snapshot::open(&signer, &person, &parsed, token).unwrap();
        let combined = read(&local).with_discovery(read(&token)).unwrap();
        assert!(combined.current("catalog-a"));
        let intake = combined.intake(parsed.clone());
        assert_eq!(intake.matches[0].coinbase.len(), 1);
        assert_eq!(intake.matches[0].coinbase_status, "complete");
        assert_eq!(intake.selected_capabilities, ["company.registry".into()].into_iter().collect());
        assert!(read(&local).with_discovery(read(&local)).is_err());
        let mut expired = read(&token);
        expired.expires = 0;
        assert!(!read(&local).with_discovery(expired).unwrap().current("catalog-a"));
        let mut injected = read(&token);
        injected.matches[0].operations.insert("external.request".into());
        assert!(read(&local).with_discovery(injected).is_err());
        let mut other = read(&token);
        other.owner = Uuid::new_v4();
        assert!(read(&local).with_discovery(other).is_err());
    }
    #[test]
    fn optional_sources_are_signed_to_the_same_owner_and_parsed_request() {
        let (signer, person, parsed, mut response) = fixture();
        response["source_options"] = serde_json::json!([{
            "source":{"origin":"apiosk","name":"Reviewed source","description":"Published company record",
                "source_slug":"reviewed-source","capability":"company.registry","endpoint_id":Uuid::new_v4().to_string(),
                "url":"https://source.example/","method":"GET","price":"0.050000 USD","network":null,
                "calls_30d":null,"availability":"supported"},
            "subject_id":"s1","requirement":parsed.capabilities[0],"reason":"Matches your question"
        }]);
        let token = Snapshot::issue(&signer,&person,&parsed,&response).unwrap();
        let snapshot = Snapshot::open(&signer,&person,&parsed,&token).unwrap();
        assert_eq!(snapshot.source_options().len(),1);
        assert_eq!(snapshot.source_options()[0].source.source_slug.as_deref(),Some("reviewed-source"));
        let mut changed=parsed.clone(); changed.capabilities[0].outputs.push("extra_field".into());
        assert!(Snapshot::open(&signer,&person,&changed,&token).is_err());

        let (_,_,_,mut external) = fixture();
        external["catalog_version"] = serde_json::json!("");
        external["matches"][0]["operations"] = serde_json::json!([]);
        external["source_options"] = response["source_options"].clone();
        let external = Snapshot::issue(&signer,&person,&parsed,&external).unwrap();
        assert!(snapshot.with_discovery(Snapshot::open(&signer,&person,&parsed,&external).unwrap()).is_err(),
            "external discovery cannot add executable optional sources");
    }
    #[test]
    fn unsigned_changed_and_oversized_snapshots_are_refused() {
        let (signer,person,parsed,response) = fixture();
        let token = Snapshot::issue(&signer,&person,&parsed,&response).unwrap();
        let mut bytes = token.into_bytes(); bytes[20] = if bytes[20] == b'A' { b'B' } else { b'A' };
        assert!(Snapshot::open(&signer,&person,&parsed,std::str::from_utf8(&bytes).unwrap()).is_err());
        assert!(Snapshot::open(&Signer::new(vec![8;32]).unwrap(),&person,&parsed,std::str::from_utf8(&bytes).unwrap()).is_err());
        assert!(Snapshot::open(&signer,&person,&parsed,&"A".repeat(MAX_TOKEN_BYTES+1)).is_err());
        let mut large = response; large["matches"][0]["query"] = json!("x".repeat(MAX_BYTES));
        assert!(Snapshot::issue(&signer,&person,&parsed,&large).is_err());
    }
}
