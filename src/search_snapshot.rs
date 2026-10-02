//! Server-issued discovery evidence, shared across replicas by the existing
//! signing key. This token grants no execution or payment authority.
use crate::{auth::Principal, capability_intake::{Intake, Match, ParsedRequest}, state::{digest, Signer}};
use anyhow::{ensure, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_BYTES: usize = 128 * 1024;
const MAX_TOKEN_BYTES: usize = (MAX_BYTES * 4).div_ceil(3) + 65;
const TTL_SECONDS: i64 = 15 * 60;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Snapshot {
    owner: uuid::Uuid,
    workspace: Option<uuid::Uuid>,
    parsed_digest: String,
    expires: i64,
    catalog_version: String,
    matches: Vec<Match>,
    sources_searched: usize,
}
impl Snapshot {
    pub(crate) fn issue(signer: &Signer, person: &Principal, parsed: &ParsedRequest, response: &Value) -> Result<String> {
        ensure!(person.is_person(), "search snapshot requires an App person");
        let snapshot = Self {
            owner: person.owner, workspace: person.workspace, parsed_digest: digest(parsed),
            expires: chrono::Utc::now().timestamp() + TTL_SECONDS,
            catalog_version: response["catalog_version"].as_str().filter(|v| !v.is_empty()).ok_or_else(|| anyhow::anyhow!("search catalog missing"))?.to_owned(),
            matches: serde_json::from_value(response["matches"].clone())?,
            sources_searched: response["apiosk_sources_searched"].as_u64().unwrap_or(0) as usize,
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
        ensure!(person.is_person() && token.len() <= MAX_TOKEN_BYTES, "invalid search snapshot");
        let (payload, signature) = token.split_once('.').ok_or_else(|| anyhow::anyhow!("invalid search snapshot"))?;
        ensure!(signature.len() == 64, "invalid search signature");
        let payload = URL_SAFE_NO_PAD.decode(payload)?;
        ensure!(payload.len() <= MAX_BYTES, "search snapshot too large");
        signer.verify_search(&payload, signature)?;
        let snapshot: Self = serde_json::from_slice(&payload)?;
        ensure!(snapshot.owner == person.owner && snapshot.workspace == person.workspace
            && snapshot.parsed_digest == digest(parsed), "search belongs to another request");
        Ok(snapshot)
    }
    pub(crate) fn current(&self, catalog_version: &str) -> bool {
        self.expires > chrono::Utc::now().timestamp() && self.catalog_version == catalog_version
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
