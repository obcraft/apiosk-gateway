//! From a goal to a priced, executable DAG — without spending anything.
//!
//! ```text
//!   intent          what is wanted, and what is already known
//!     │
//!     │ compile     backward search from the wanted facts to the facts we have
//!     ▼
//!   plan            a deduplicated DAG of nodes, topologically ordered
//!     │
//!     │ quote       one buyer-total, summed from per-node buyer-totals
//!     ▼
//!   plan + amount   read-only. Nothing has been called and nothing is owed.
//! ```
//!
//! THE PLANNER SPENDS NOTHING. Not one provider call, not one resolver, not
//! one speculative fetch. Everything it needs to decide is already in the
//! catalogue: which contracts exist, what they require and produce, and what
//! each endpoint's list price is. That is the property the release gate names
//! first, and it holds by construction here — this module has no HTTP client
//! and no way to acquire one.
//!
//! ---------------------------------------------------------------------------
//! What "shared node" means, and why it is not "same output name"
//! ---------------------------------------------------------------------------
//!
//! A company profile and a company's filed accounts both need a KVK number.
//! Both therefore depend on the same identity lookup, and a plan that lists
//! that lookup twice charges for it twice. Merging them is the single most
//! visible thing this compiler does.
//!
//! But two nodes are the same node only when running one of them would produce
//! exactly what running the other would. That is not "they produce a fact with
//! the same name". It is the whole of: contract version, subject role, and the
//! bound inputs. Two lookups for two different companies both produce
//! `company.registration.nl.kvk`, and merging those would silently research the
//! wrong business. [`compile::NodeKey`] is that tuple, and nothing merges
//! except on it.

pub mod compile;
pub mod quote;
pub mod reading;
pub mod token;
pub mod validate;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::facts::contract::ExecutionContract;

/// What the buyer asked for, in the vocabulary the compiler plans in.
///
/// Produced from a question by the reader in `crate::ask`, or supplied
/// directly by an API caller. Either way the compiler validates it: an intent
/// is a description of a goal, never an execution decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    /// The things the question is about. As many as the question names: a
    /// comparison is ONE question about two companies, not two questions.
    pub subjects: Vec<Subject>,
    /// Facts the answer must contain. A plan that cannot reach one of these is
    /// refused rather than quietly delivered short.
    pub required_outputs: Vec<String>,
    /// Facts worth having. An unreachable one is dropped with a reason, and
    /// the plan still stands.
    #[serde(default)]
    pub optional_outputs: Vec<String>,
    #[serde(default)]
    pub format: OutputFormat,
    /// Hard ceiling on the buyer-total, in atomic USDC. A plan above it is
    /// refused with its price, not silently trimmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_buyer_total_atomic: Option<i128>,
    /// Restricts candidates to contracts valid in this jurisdiction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jurisdiction: Option<String>,
}

/// One thing the question is about, and whatever is already known about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subject {
    /// Matches `Requirement::subject_role` on a contract. `subject` unless the
    /// question genuinely holds two roles (an acquirer and a target).
    #[serde(default = "default_role")]
    pub role: String,
    /// Facts the caller already has. THE REASON A KNOWN KVK NUMBER MAKES THE
    /// LOOKUP DISAPPEAR: a fact that is already known is not a fact that needs
    /// producing, so no node produces it and no node is priced for it.
    #[serde(default)]
    pub known: BTreeMap<String, Value>,
}

/// One fact this conversation already paid for.
#[derive(Debug, Clone, PartialEq)]
pub struct HeldFact {
    pub fact_type: String,
    /// The identity of the thing the fact is about — the validated value, not
    /// a position. `None` while a subject was still a candidate.
    pub subject_key: Option<String>,
    pub subject_role: String,
    pub value: Value,
}

/// Fold what this thread already knows into the intent it just produced.
///
/// A thread that paid for Coolblue's KVK number on turn one must not pay for it
/// again on turn two. The machinery for that already exists — a fact in
/// [`Subject::known`] produces no node and is priced at nothing — and until now
/// nothing filled it: the reader sees one question and none of the conversation
/// around it, so every follow-up planned the lookup again from zero.
///
/// THE HARD PART IS NOT REUSE, IT IS IDENTITY. Held facts are keyed by the
/// subject's validated identity (`24330087`); a follow-up naming "Coolblue
/// B.V." carries a name, not that number, so the two cannot be joined on the
/// key. Joining them on ROLE alone would be a disaster: two turns about two
/// different companies both say `subject`, and Coolblue's registration number
/// would be silently answered for Mollie. Paying twice is a cost. Answering
/// about the wrong company is a lie.
///
/// So a held subject is attached only when it and the intent's subject AGREE ON
/// A FACT THEY BOTH STATE — same type, same value. That is evidence they are
/// the same entity rather than an assumption. Where no such agreement exists,
/// nothing is injected and the plan is priced exactly as it is today.
///
/// What the intent itself says always wins: a value the reader put on the
/// subject is never overwritten by a remembered one.
pub fn merge_held_facts(intent: &mut Intent, held: &[HeldFact]) {
    for subject in &mut intent.subjects {
        // Candidates: held facts about one identified subject, in this role.
        let mut by_identity: BTreeMap<&str, Vec<&HeldFact>> = BTreeMap::new();
        for fact in held {
            if fact.subject_role != subject.role {
                continue;
            }
            let Some(key) = fact.subject_key.as_deref() else {
                continue;
            };
            by_identity.entry(key).or_default().push(fact);
        }

        for (_identity, facts) in by_identity {
            let agrees = facts.iter().any(|fact| {
                subject
                    .known
                    .get(&fact.fact_type)
                    .is_some_and(|stated| stated == &fact.value)
            });
            if !agrees {
                continue;
            }
            for fact in facts {
                subject
                    .known
                    .entry(fact.fact_type.clone())
                    .or_insert_with(|| fact.value.clone());
            }
        }
    }
}

fn default_role() -> String {
    "subject".to_string()
}

fn is_default_role(role: &str) -> bool {
    role == "subject"
}

/// What the answer is delivered as. Carried in the plan because a costlier
/// format has to be inside the amount the buyer approves, never added after.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    #[default]
    Json,
    Pdf,
}

/// One executable offer, as the compiler sees it: the contract plus the
/// catalogue facts that decide whether and at what price it may be used.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub contract_id: String,
    pub endpoint_id: String,
    /// For the plan text a person reads. Never a price.
    pub api_slug: String,
    pub endpoint_path: String,
    pub endpoint_method: String,
    pub contract: ExecutionContract,
    /// The provider's asking price in atomic USDC, straight off the endpoint.
    pub list_atomic: i128,
    pub class: crate::fees::ProviderClass,
    /// `api_endpoints.active`. An inactive endpoint is not a candidate, and
    /// this is checked here rather than trusted from `capability_offers` —
    /// live today, two offers and one active resolver still point at a KVK
    /// endpoint whose `active` is false.
    pub active: bool,
    /// `NULL` means "not jurisdiction-bound", which matches every intent.
    pub jurisdiction: Option<String>,
}

/// One step of an approved plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanNode {
    /// Immutable input/output mappings approved with this node. Legacy plans must be recompiled.
    #[serde(default)]
    pub contract: Option<ExecutionContract>,
    /// Stable within a plan and derived from the dedup key, so the same work
    /// carries the same id across a recompile of the same intent.
    pub id: String,
    pub contract_id: String,
    pub endpoint_id: String,
    pub capability_slug: String,
    pub api_slug: String,
    pub endpoint_path: String,
    #[serde(default)]
    pub endpoint_method: String,
    pub subject_role: String,
    /// Fact types this node yields, sorted.
    pub produces: Vec<String>,
    /// Node ids that must finish first, sorted.
    pub depends_on: Vec<String>,
    /// Where each required fact comes from.
    pub bindings: BTreeMap<String, Binding>,
    /// This node's own buyer-total in atomic USDC: list plus the 15%, rounded
    /// once, here. Summing these is the plan's price; nothing multiplies a
    /// second markup onto the sum.
    pub buyer_total_atomic: i128,
    /// True when the response can describe several subjects, so the job may
    /// pause here and ask. Visible in the plan, because "this may stop to ask
    /// you something" is part of what a buyer is approving.
    pub may_ask: bool,
}

/// Where one of a node's required facts comes from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum Binding {
    /// The caller already supplied it. Costs nothing and adds no node.
    Known { value: Value },
    /// Another node in this plan produces it.
    Node { node_id: String },
}

/// Whether a wanted fact is reachable, and by what.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Coverage {
    pub fact_type: String,
    /// Which subject this covers. Omitted for the first subject, so a
    /// one-company plan serialises — and therefore hashes — exactly as it did
    /// before a plan could hold two.
    #[serde(default = "default_role", skip_serializing_if = "is_default_role")]
    pub subject_role: String,
    pub required: bool,
    /// The node that will produce it, or `None` when it is already known or
    /// unreachable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub status: CoverageStatus,
    /// Why an unreachable fact is unreachable, in the compiler's words: "the
    /// only sources describe ip, not company". Absent otherwise, so a plan
    /// without one serialises — and hashes — exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    /// The caller supplied it. No node, no cost.
    AlreadyKnown,
    /// A node in the plan produces it.
    Planned,
    /// Nothing in the catalogue produces it under this intent's constraints.
    /// Fatal for a required output; recorded and dropped for an optional one.
    Unreachable,
}

/// A compiled, priced, read-only plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    /// Topologically ordered: every node appears after everything it needs.
    pub nodes: Vec<PlanNode>,
    pub coverage: Vec<Coverage>,
    pub quote: quote::Quote,
    pub format: OutputFormat,
    /// The limits this plan was compiled under, echoed back so a plan explains
    /// its own shape.
    pub limits: compile::Limits,
}

impl Plan {
    /// Nodes that will actually be charged for. Every node in a plan is a paid
    /// call today; the accessor exists so that stays explicit if a free node
    /// type is ever added.
    pub fn paid_nodes(&self) -> usize {
        self.nodes.len()
    }

    /// What the App and an MCP client compare to agree they are looking at the
    /// same plan.
    ///
    /// Built from named components in a fixed order rather than from
    /// `serde_json::to_string(self)`. Serialising the whole struct would tie
    /// the hash to field order and to serde's formatting forever: adding a
    /// display-only field would change the hash of a plan whose work and price
    /// had not moved, and every already-approved plan would stop matching.
    /// Only the things that decide what is done and what it costs are in here.
    pub fn hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        let mut feed = |label: &str, value: &str| {
            hasher.update(label.as_bytes());
            hasher.update(b"=");
            hasher.update(value.as_bytes());
            hasher.update(b"\n");
        };
        feed(
            "format",
            match self.format {
                OutputFormat::Json => "json",
                OutputFormat::Pdf => "pdf",
            },
        );
        feed("policy", "clockwork-v1");
        feed(
            "limits",
            &serde_json::to_string(&self.limits).expect("limits serialize"),
        );
        feed(
            "coverage",
            &serde_json::to_string(&self.coverage).expect("coverage serializes"),
        );
        feed("total", &self.quote.buyer_total_atomic.to_string());
        for line in &self.quote.lines {
            feed("source_cost", &line.source_cost_atomic.to_string());
            feed("apiosk_fee", &line.apiosk_fee_atomic.to_string());
        }
        for node in &self.nodes {
            feed("node", &node.id);
            feed(
                "contract_snapshot",
                &serde_json::to_string(&node.contract).expect("contract serializes"),
            );
            feed("api", &node.api_slug);
            feed("path", &node.endpoint_path);
            feed("method", &node.endpoint_method);
            feed(
                "produces",
                &serde_json::to_string(&node.produces).expect("produces serializes"),
            );
            feed("contract", &node.contract_id);
            feed("endpoint", &node.endpoint_id);
            feed("role", &node.subject_role);
            feed("price", &node.buyer_total_atomic.to_string());
            for dependency in &node.depends_on {
                feed("needs", dependency);
            }
            for (fact_type, binding) in &node.bindings {
                feed(
                    "binds",
                    &match binding {
                        Binding::Known { value } => format!("{fact_type}=known:{value}"),
                        Binding::Node { node_id } => format!("{fact_type}=node:{node_id}"),
                    },
                );
            }
        }
        for entry in &self.coverage {
            feed(
                "covers",
                &format!("{}:{:?}:{}", entry.fact_type, entry.status, entry.required),
            );
        }
        hex::encode(hasher.finalize())
    }

    /// The longest chain of dependencies. What decides how long the job takes
    /// when everything that can run in parallel does.
    pub fn critical_path(&self) -> usize {
        let mut depth: BTreeMap<&str, usize> = BTreeMap::new();
        let mut deepest = 0;
        // Nodes are topologically ordered, so a node's dependencies are always
        // already measured by the time it is reached.
        for node in &self.nodes {
            let own = node
                .depends_on
                .iter()
                .filter_map(|id| depth.get(id.as_str()).copied())
                .max()
                .unwrap_or(0)
                + 1;
            depth.insert(node.id.as_str(), own);
            deepest = deepest.max(own);
        }
        deepest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, price: i128, depends_on: Vec<String>) -> PlanNode {
        PlanNode {
            endpoint_method: "GET".into(),
            contract: None,
            id: id.to_string(),
            contract_id: format!("c_{id}"),
            endpoint_id: format!("e_{id}"),
            capability_slug: "x.y".to_string(),
            api_slug: "kvk-dutch-business-register".to_string(),
            endpoint_path: "/p".to_string(),
            subject_role: "subject".to_string(),
            produces: vec!["company.name".to_string()],
            depends_on,
            bindings: BTreeMap::new(),
            buyer_total_atomic: price,
            may_ask: false,
        }
    }

    fn plan(nodes: Vec<PlanNode>) -> Plan {
        let lines = nodes
            .iter()
            .map(|n| quote::QuoteLine {
                node_id: n.id.clone(),
                capability_slug: n.capability_slug.clone(),
                source_cost_atomic: n.buyer_total_atomic,
                apiosk_fee_atomic: 0,
                buyer_total_atomic: n.buyer_total_atomic,
            })
            .collect();
        Plan {
            nodes,
            coverage: vec![],
            quote: quote::total(lines, None),
            format: OutputFormat::Json,
            limits: compile::Limits::default(),
        }
    }

    #[test]
    fn the_critical_path_is_the_longest_chain_not_the_node_count() {
        // a -> b, a -> c: three nodes, two steps, because b and c run together.
        let fanned = plan(vec![
            node("a", 1, vec![]),
            node("b", 1, vec!["a".to_string()]),
            node("c", 1, vec!["a".to_string()]),
        ]);
        assert_eq!(fanned.paid_nodes(), 3);
        assert_eq!(fanned.critical_path(), 2);

        // a -> b -> c: three nodes, three steps.
        let chained = plan(vec![
            node("a", 1, vec![]),
            node("b", 1, vec!["a".to_string()]),
            node("c", 1, vec!["b".to_string()]),
        ]);
        assert_eq!(chained.critical_path(), 3);
    }

    #[test]
    fn an_empty_plan_has_no_path_and_no_cost() {
        let empty = plan(vec![]);
        assert_eq!(empty.critical_path(), 0);
        assert_eq!(empty.quote.buyer_total_atomic, 0);
    }

    #[test]
    fn the_hash_is_stable_and_is_sixty_four_hex_characters() {
        let first = plan(vec![node("a", 1, vec![])]);
        let second = plan(vec![node("a", 1, vec![])]);
        assert_eq!(first.hash(), second.hash());
        assert_eq!(first.hash().len(), 64);
        assert!(first.hash().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn the_hash_moves_when_the_work_or_the_price_moves() {
        let base = plan(vec![node("a", 1, vec![])]);
        let dearer = plan(vec![node("a", 2, vec![])]);
        let extra = plan(vec![node("a", 1, vec![]), node("b", 1, vec![])]);
        let mut other_format = plan(vec![node("a", 1, vec![])]);
        other_format.format = OutputFormat::Pdf;

        assert_ne!(base.hash(), dearer.hash());
        assert_ne!(base.hash(), extra.hash());
        assert_ne!(base.hash(), other_format.hash());
    }

    #[test]
    fn the_hash_does_not_move_for_a_display_only_change() {
        // Display hints may change. The API slug is an execution address
        // used by the worker and therefore is part of the approved hash.
        let base = plan(vec![node("a", 1, vec![])]);
        let mut relabelled = plan(vec![node("a", 1, vec![])]);
        relabelled.nodes[0].may_ask = true;

        relabelled.nodes[0].capability_slug = "renamed.slug".to_string();
        assert_eq!(base.hash(), relabelled.hash());
        relabelled.nodes[0].api_slug = "something-else".to_string();
        assert_ne!(base.hash(), relabelled.hash());
    }
}

#[cfg(test)]
mod held_fact_tests {
    use super::*;
    use serde_json::json;

    fn held(fact_type: &str, key: &str, value: Value) -> HeldFact {
        HeldFact {
            fact_type: fact_type.to_string(),
            subject_key: Some(key.to_string()),
            subject_role: "subject".to_string(),
            value,
        }
    }

    fn intent_knowing(pairs: &[(&str, Value)]) -> Intent {
        Intent {
            subjects: vec![Subject {
                role: "subject".to_string(),
                known: pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect(),
            }],
            required_outputs: vec!["company.financial_statements".to_string()],
            optional_outputs: Vec::new(),
            format: OutputFormat::default(),
            max_buyer_total_atomic: None,
            jurisdiction: None,
        }
    }

    #[test]
    fn a_number_this_thread_already_bought_is_not_bought_again() {
        // Turn one paid for Coolblue's KVK number and printed it. Turn two
        // names Coolblue again. The number must arrive as known, so no node
        // produces it and nothing is priced for it.
        let mut intent = intent_knowing(&[("company.name", json!("Coolblue B.V."))]);
        let facts = vec![
            held("company.name", "24330087", json!("Coolblue B.V.")),
            held("company.registration.nl.kvk", "24330087", json!("24330087")),
        ];

        merge_held_facts(&mut intent, &facts);

        assert_eq!(
            intent.subjects[0].known.get("company.registration.nl.kvk"),
            Some(&json!("24330087"))
        );
    }

    #[test]
    fn one_companys_facts_never_answer_for_another() {
        // The failure that would be far worse than paying twice. Both turns say
        // `subject`; only the agreed fact tells them apart. Coolblue's
        // registration number must not appear under Mollie.
        let mut intent = intent_knowing(&[("company.name", json!("Mollie B.V."))]);
        let facts = vec![
            held("company.name", "24330087", json!("Coolblue B.V.")),
            held("company.registration.nl.kvk", "24330087", json!("24330087")),
        ];

        merge_held_facts(&mut intent, &facts);

        assert!(!intent.subjects[0]
            .known
            .contains_key("company.registration.nl.kvk"));
        assert_eq!(intent.subjects[0].known.len(), 1);
    }

    #[test]
    fn nothing_is_attached_when_the_subject_is_still_anonymous() {
        // No stated fact means no agreement is possible, so there is no
        // evidence of identity and nothing is injected. The plan is priced
        // exactly as it is today — a saving forgone, never a wrong answer.
        let mut intent = intent_knowing(&[]);
        let facts = vec![held(
            "company.registration.nl.kvk",
            "24330087",
            json!("24330087"),
        )];

        merge_held_facts(&mut intent, &facts);

        assert!(intent.subjects[0].known.is_empty());
    }

    #[test]
    fn what_the_reader_stated_is_never_overwritten_by_memory() {
        // The question in front of us outranks the thread behind us.
        let mut intent = intent_knowing(&[
            ("company.name", json!("Coolblue B.V.")),
            ("company.registration.nl.kvk", json!("99999999")),
        ]);
        let facts = vec![
            held("company.name", "24330087", json!("Coolblue B.V.")),
            held("company.registration.nl.kvk", "24330087", json!("24330087")),
        ];

        merge_held_facts(&mut intent, &facts);

        assert_eq!(
            intent.subjects[0].known.get("company.registration.nl.kvk"),
            Some(&json!("99999999"))
        );
    }

    #[test]
    fn a_fact_about_another_role_stays_on_that_role() {
        // A comparison holds two subjects. The acquirer's number is not the
        // target's, and role is a hard boundary before identity is even asked.
        let mut intent = intent_knowing(&[("company.name", json!("Coolblue B.V."))]);
        let facts = vec![
            HeldFact {
                fact_type: "company.name".to_string(),
                subject_key: Some("24330087".to_string()),
                subject_role: "target".to_string(),
                value: json!("Coolblue B.V."),
            },
            HeldFact {
                fact_type: "company.registration.nl.kvk".to_string(),
                subject_key: Some("24330087".to_string()),
                subject_role: "target".to_string(),
                value: json!("24330087"),
            },
        ];

        merge_held_facts(&mut intent, &facts);

        assert!(!intent.subjects[0]
            .known
            .contains_key("company.registration.nl.kvk"));
    }

    #[test]
    fn a_fact_with_no_identity_yet_is_not_attached_to_anything() {
        // `subject_key` is NULL while a subject was still a candidate. There is
        // nothing to join on, so it joins to nothing.
        let mut intent = intent_knowing(&[("company.name", json!("Coolblue B.V."))]);
        let facts = vec![HeldFact {
            fact_type: "company.registration.nl.kvk".to_string(),
            subject_key: None,
            subject_role: "subject".to_string(),
            value: json!("24330087"),
        }];

        merge_held_facts(&mut intent, &facts);

        assert!(!intent.subjects[0]
            .known
            .contains_key("company.registration.nl.kvk"));
    }
}
