use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub fn digest(value: &impl Serialize) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(value).expect("serializable contract"),
    ))
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct Focus {
    pub entity_refs: Vec<Uuid>,
    pub goal_refs: Vec<Uuid>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub schema_version: String,
    pub state_ref: Uuid,
    pub revision: i64,
    pub expires_at: DateTime<Utc>,
    pub focus: Focus,
    pub state_token: String,
}
pub struct Signer(Vec<u8>);
impl Signer {
    pub fn new(key: Vec<u8>) -> anyhow::Result<Self> {
        anyhow::ensure!(
            key.len() >= 32,
            "state signing key must be at least 32 bytes"
        );
        Ok(Self(key))
    }
    fn payload(owner: Uuid, envelope: &Envelope) -> Vec<u8> {
        serde_json::to_vec(&(
            owner,
            &envelope.schema_version,
            envelope.state_ref,
            envelope.revision,
            envelope.expires_at,
            &envelope.focus,
        ))
        .unwrap()
    }
    pub fn issue(&self, owner: Uuid, task: Uuid, revision: i64, focus: Focus) -> Envelope {
        let mut e = Envelope {
            schema_version: "2".into(),
            state_ref: task,
            revision,
            expires_at: Utc::now() + chrono::Duration::minutes(30),
            focus,
            state_token: String::new(),
        };
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).unwrap();
        mac.update(&Self::payload(owner, &e));
        e.state_token = hex::encode(mac.finalize().into_bytes());
        e
    }
    pub fn verify(&self, owner: Uuid, e: &Envelope, allow_expired: bool) -> anyhow::Result<()> {
        anyhow::ensure!(
            e.schema_version == "2" && e.revision >= 0,
            "invalid state version"
        );
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).unwrap();
        mac.update(&Self::payload(owner, e));
        mac.verify_slice(&hex::decode(&e.state_token)?)
            .map_err(|_| anyhow::anyhow!("invalid state signature"))?;
        anyhow::ensure!(
            allow_expired || e.expires_at > Utc::now(),
            "state expired; recover task"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub entity_ref: Uuid,
    pub label: String,
    pub selected: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fact {
    pub entity_ref: Uuid,
    pub semantic_type: String,
    pub value: Value,
    pub provenance: Value,
    pub observed_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub verification: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub question: String,
    pub reply: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Task {
    #[serde(default)]
    pub conversation: Vec<ConversationTurn>,
    pub entities: Vec<Entity>,
    pub facts: Vec<Fact>,
    pub focus: Focus,
    pub intent: Option<Value>,
    pub actions: Vec<Value>,
    pub proposal: Option<Value>,
    pub completed: Vec<String>,
    pub result_refs: Vec<Uuid>,
    pub pending_candidates: Vec<Value>,
    pub original_subject: Option<Uuid>,
    pub cancelled: bool,
    #[serde(default)]
    pub execution_error: Option<Value>,
}
impl Task {
    pub fn record_turn(&mut self, question: &str, reply: Option<String>) {
        self.conversation.push(ConversationTurn {
            question: question.to_owned(),
            reply,
        });
        let remove = self.conversation.len().saturating_sub(6);
        self.conversation.drain(..remove);
    }
    pub fn context_view(&self) -> Value {
        serde_json::json!({"conversation":self.conversation,"entities":self.entities.iter().filter(|e|self.focus.entity_refs.contains(&e.entity_ref)).collect::<Vec<_>>(),"result_refs":self.result_refs.iter().rev().take(8).collect::<Vec<_>>()})
    }
    pub fn bounded(&self) -> bool {
        self.conversation.len() <= 6
            && self.entities.len() <= 100
            && self.facts.len() <= 1000
            && self.actions.len() <= 24
            && self.result_refs.len() <= 100
            && serde_json::to_vec(self).is_ok_and(|b| b.len() <= 1024 * 1024)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_state_is_account_and_focus_bound() {
        let s = Signer::new(vec![9; 32]).unwrap();
        let owner = Uuid::new_v4();
        let mut e = s.issue(owner, Uuid::new_v4(), 0, Focus::default());
        assert!(s.verify(owner, &e, false).is_ok());
        assert!(s.verify(Uuid::new_v4(), &e, false).is_err());
        e.focus.entity_refs.push(Uuid::new_v4());
        assert!(s.verify(owner, &e, false).is_err());
    }
    #[test]
    fn conversation_retains_six_recent_turns_and_reads_old_tasks() {
        let mut task = Task::default();
        for n in 0..10 {
            task.record_turn(&n.to_string(), Some("reply".into()));
        }
        assert_eq!(task.conversation.len(), 6);
        assert_eq!(task.conversation[0].question, "4");
        let mut legacy = serde_json::to_value(Task::default()).unwrap();
        legacy.as_object_mut().unwrap().remove("conversation");
        assert!(serde_json::from_value::<Task>(legacy)
            .unwrap()
            .conversation
            .is_empty());
    }
    #[test]
    fn context_is_bounded_by_focus_and_recent_results() {
        let task = Task {
            result_refs: (0..100).map(|_| Uuid::new_v4()).collect(),
            ..Task::default()
        };
        assert_eq!(
            task.context_view()["result_refs"].as_array().unwrap().len(),
            8
        );
    }
}
