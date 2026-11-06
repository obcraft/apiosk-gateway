//! The backward search that turns an intent into a deduplicated, priced DAG.
//!
//! ```text
//!   for every wanted fact that is not already known:
//!     find the contracts that produce it, under this intent's constraints
//!     take the best one
//!     its own requirements become wanted facts
//!   until nothing is left wanted, or a limit is hit
//! ```
//!
//! Bounded on purpose, and the bounds are in [`Limits`] rather than scattered:
//! eight paid nodes, dependency depth four, three concurrent calls. These are
//! starting settings to be validated against real jobs, not tuned constants,
//! and a plan echoes them back so it can explain its own shape.
//!
//! ---------------------------------------------------------------------------
//! What this refuses to do
//! ---------------------------------------------------------------------------
//!
//! * **Spend.** No HTTP client, no resolver, no probe. Every input is
//!   catalogue data the caller already loaded.
//! * **Fire a hidden resolver.** A resolver is a node or it does not happen.
//!   Live today, `/v1/do` can run a paid KVK lookup on its own when a binding
//!   looks unready, uncapped unless the caller happened to send a price field.
//!   On the job path that is off: a contract whose requirements are not all
//!   bound is not schedulable, and the plan says so before anyone approves it.
//! * **Claim an optimum.** It ranks candidates on what is actually known —
//!   price, then a stable tiebreak. Latency and quality are not measured yet
//!   and are therefore not weighted; a weight on an unmeasured quantity is a
//!   guess wearing a number.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::quote::{self, QuoteLine};
use super::{
    Binding, Candidate, Coverage, CoverageStatus, Intent, OutputFormat, Plan, PlanNode, Subject,
};

/// The bounds a plan is compiled under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Most paid calls one plan may contain.
    pub max_paid_nodes: usize,
    /// Longest chain of dependencies.
    pub max_depth: usize,
    /// How many nodes the worker may have in flight at once. Carried in the
    /// plan because it bounds the job's blast radius, not just its speed.
    pub max_concurrency: usize,
}

impl Default for Limits {
    /// The starting settings from the plan, adjusted against real jobs: with
    /// the vocabulary at 39 words and 140 contracts, a question that pairs a
    /// registry profile with the company's own site and the web needed nine
    /// calls on 2026-09-06 and was refused at eight.
    fn default() -> Self {
        Self {
            max_paid_nodes: 12,
            max_depth: 4,
            max_concurrency: 3,
        }
    }
}

/// Why an intent has no plan.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PlanError {
    #[error("no source can produce `{fact_type}`{}", reason_suffix(.reason))]
    Unreachable {
        fact_type: String,
        reason: Option<String>,
    },
    #[error("`{fact_type}` depends on itself")]
    Cycle { fact_type: String },
    #[error("the plan needs {found} paid calls; the limit is {limit}")]
    TooManyNodes { found: usize, limit: usize },
    #[error("the plan is {found} steps deep; the limit is {limit}")]
    TooDeep { found: usize, limit: usize },
    #[error("the plan costs {found} atomic USDC; the ceiling is {limit}")]
    TooExpensive { found: i128, limit: i128 },
    #[error("the dependency search exceeded its bounded candidate budget")]
    SearchLimit,
    #[error("the intent names no required output")]
    NothingWanted,
    #[error("the intent names no subject")]
    NoSubject,
    #[error(
        "`{fact_type}` is wanted for subject role `{role}`, which the intent does not describe"
    )]
    UnknownRole { fact_type: String, role: String },
}

fn reason_suffix(reason: &Option<String>) -> String {
    match reason {
        Some(text) => format!(" ({text})"),
        None => String::new(),
    }
}

/// Identity of a unit of work.
///
/// Two nodes merge only when running one would produce exactly what running
/// the other would. Not "same output name" — a lookup for Mollie and a lookup
/// for Adyen both produce `company.registration.nl.kvk`, and merging those
/// would research the wrong company. The bound inputs are in the key precisely
/// so that cannot happen.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct NodeKey {
    contract_id: String,
    subject_role: String,
    /// Bindings rendered canonically: `fact_type=known:<value>` or
    /// `fact_type=node:<id>`.
    bindings: Vec<String>,
}

impl NodeKey {
    /// A short stable id. Same intent, same catalogue, same id — which is what
    /// lets an App and an MCP client show the same plan hash.
    fn id(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.contract_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.subject_role.as_bytes());
        for binding in &self.bindings {
            hasher.update(b"\0");
            hasher.update(binding.as_bytes());
        }
        format!("n_{}", hex::encode(&hasher.finalize()[..8]))
    }
}

/// Compile an intent into a plan. Reads catalogue data; calls nothing.
pub fn compile(
    intent: &Intent,
    candidates: &[Candidate],
    limits: Limits,
    rate_micro: Option<i64>,
) -> Result<Plan, PlanError> {
    if intent.subjects.is_empty() {
        return Err(PlanError::NoSubject);
    }
    if intent.required_outputs.is_empty() {
        return Err(PlanError::NothingWanted);
    }

    let mut state = Compiler {
        intent,
        candidates,
        limits,
        remaining_candidates: 128,
        nodes: BTreeMap::new(),
        order: Vec::new(),
    };

    let mut coverage: Vec<Coverage> = Vec::new();

    // Every wanted fact, for every subject. THAT IS WHAT A COMPARISON IS: two
    // companies and one list of facts, not two questions. Subjects on the
    // outside keeps a plan card grouped by company, and a one-subject intent
    // walks exactly the order it always did.
    // The first required fact nothing could produce. A wanted fact that no
    // source of the right kind yields is recorded as missing coverage, the
    // same as an optional one: the reader lists every fact a step's
    // capability can yield, and one of them being out of reach must not
    // throw away the calls that do answer the question. It is fatal only when
    // it leaves nothing to buy — then the plan would be a price for nothing,
    // and the error names what was missing.
    let mut first_unreachable: Option<PlanError> = None;

    for subject in &intent.subjects {
        let role = subject.role.clone();
        for fact_type in &intent.required_outputs {
            match state.satisfy(fact_type, &role, 1, &mut Vec::new()) {
                Ok(produced) => coverage.push(state.coverage_for(fact_type, &role, true, produced)),
                Err(error @ PlanError::Unreachable { .. }) => {
                    coverage.push(Coverage {
                        fact_type: fact_type.clone(),
                        subject_role: role.clone(),
                        required: true,
                        node_id: None,
                        status: CoverageStatus::Unreachable,
                        reason: unreachable_reason(&error),
                    });
                    first_unreachable.get_or_insert(error);
                }
                Err(other) => return Err(other),
            }
        }

        for fact_type in &intent.optional_outputs {
            // An optional output that cannot be reached is not a failure. It is
            // recorded as missing coverage, which is what a plan card shows under
            // "what this will not tell you".
            match state.satisfy(fact_type, &role, 1, &mut Vec::new()) {
                Ok(produced) => {
                    coverage.push(state.coverage_for(fact_type, &role, false, produced))
                }
                Err(error @ PlanError::Unreachable { .. })
                | Err(error @ PlanError::Cycle { .. }) => {
                    coverage.push(Coverage {
                        fact_type: fact_type.clone(),
                        subject_role: role.clone(),
                        required: false,
                        node_id: None,
                        status: CoverageStatus::Unreachable,
                        reason: unreachable_reason(&error),
                    });
                }
                Err(other) => return Err(other),
            }
        }
    }

    if let Some(error) = first_unreachable {
        let anything_planned = coverage
            .iter()
            .any(|c| c.required && c.status != CoverageStatus::Unreachable);
        if !anything_planned {
            return Err(error);
        }
    }

    let nodes = state.finish()?;

    let lines: Vec<QuoteLine> = nodes
        .iter()
        .map(|node| {
            let candidate = candidates
                .iter()
                .find(|candidate| candidate.contract_id == node.contract_id)
                .expect("every plan node came from a candidate");
            QuoteLine {
                node_id: node.id.clone(),
                capability_slug: node.capability_slug.clone(),
                source_cost_atomic: candidate.list_atomic,
                apiosk_fee_atomic: node.buyer_total_atomic - candidate.list_atomic,
                buyer_total_atomic: node.buyer_total_atomic,
            }
        })
        .collect();
    let eur = rate_micro.and_then(|rate| {
        let subtotal: i128 = lines.iter().map(|l| l.buyer_total_atomic).sum();
        quote::euro_cents(subtotal, rate).map(|cents| quote::EuroSnapshot {
            cents,
            rate_micro: rate,
            taken_at: chrono::Utc::now(),
        })
    });
    let quote = quote::total(lines, eur);

    if let Some(ceiling) = intent.max_buyer_total_atomic {
        if quote.buyer_total_atomic > ceiling {
            return Err(PlanError::TooExpensive {
                found: quote.buyer_total_atomic,
                limit: ceiling,
            });
        }
    }

    let plan = Plan {
        nodes,
        coverage,
        quote,
        format: intent.format,
        limits,
    };

    let depth = plan.critical_path();
    if depth > limits.max_depth {
        return Err(PlanError::TooDeep {
            found: depth,
            limit: limits.max_depth,
        });
    }
    Ok(plan)
}

/// The kind a fact type or capability slug belongs to: the namespace before
/// the first dot, with a `_qualifier` stripped. `company.country`,
/// `company_registry.search` and `company_intel.check` are all `company`;
/// `web_tools.url` and `web.search` are both `web`. The subject kinds in
/// `fact_subject_kinds` are derived the same way, from the capability
/// namespace.
/// The refusal a coverage line carries, taken from the error that produced it.
fn unreachable_reason(error: &PlanError) -> Option<String> {
    match error {
        PlanError::Unreachable { reason, .. } => reason.clone(),
        PlanError::Cycle { fact_type } => Some(format!("{fact_type} would need itself")),
        _ => None,
    }
}

pub(crate) fn kind_of(slug: &str) -> String {
    let namespace = slug.split('.').next().unwrap_or(slug);
    namespace.split('_').next().unwrap_or(namespace).to_string()
}

struct Compiler<'a> {
    intent: &'a Intent,
    candidates: &'a [Candidate],
    remaining_candidates: usize,
    limits: Limits,
    nodes: BTreeMap<String, PlanNode>,
    /// Insertion order, which is dependency order: a node is only inserted
    /// once everything it needs already is.
    order: Vec<String>,
}

/// What satisfying a wanted fact produced.
enum Produced {
    /// The caller already had it.
    Known,
    /// This node id produces it.
    Node(String),
}

impl<'a> Compiler<'a> {
    fn subject(&self, role: &str) -> Option<&Subject> {
        self.intent.subjects.iter().find(|s| s.role == role)
    }

    /// Make `fact_type` available for `role`, adding nodes as needed.
    ///
    /// `visiting` is the chain of fact types currently being resolved. A fact
    /// type appearing in its own chain is a cycle, and a cycle is refused
    /// rather than unrolled — an unrolled cycle is an unbounded bill.
    /// What the plan already has for `fact_type` on `role`, without buying
    /// anything: a fact the buyer supplied, or one a node already in the
    /// plan produces. `None` means it would take a new call.
    fn at_hand(&self, fact_type: &str, role: &str) -> Option<Produced> {
        let subject = self.subject(role)?;
        if subject.known.contains_key(fact_type) {
            return Some(Produced::Known);
        }
        self.order
            .iter()
            .find(|id| {
                self.nodes.get(*id).is_some_and(|node| {
                    node.subject_role == role && node.produces.iter().any(|f| f == fact_type)
                })
            })
            .map(|id| Produced::Node(id.clone()))
    }

    fn satisfy(
        &mut self,
        fact_type: &str,
        role: &str,
        depth: usize,
        visiting: &mut Vec<String>,
    ) -> Result<Produced, PlanError> {
        let Some(subject) = self.subject(role) else {
            return Err(PlanError::UnknownRole {
                fact_type: fact_type.to_string(),
                role: role.to_string(),
            });
        };
        if subject.known.contains_key(fact_type) {
            return Ok(Produced::Known);
        }
        if visiting.iter().any(|seen| seen == fact_type) {
            return Err(PlanError::Cycle {
                fact_type: fact_type.to_string(),
            });
        }
        if depth > self.limits.max_depth {
            return Err(PlanError::TooDeep {
                found: depth,
                limit: self.limits.max_depth,
            });
        }

        // A fact this subject's plan already buys is not bought again. One
        // profile lookup yields the name, the number and the profile; asking
        // for each of those as its own wanted output must not add a node per
        // fact, and the cheapest source for one of them is not cheaper than a
        // call that is already in the price. Dependency order holds: the node
        // is already inserted, so nothing depending on it can come before it.
        if let Some(id) = self.order.iter().find(|id| {
            self.nodes.get(*id).is_some_and(|node| {
                node.subject_role == role && node.produces.iter().any(|f| f == fact_type)
            })
        }) {
            return Ok(Produced::Node(id.clone()));
        }

        let candidates = self.best_candidates(fact_type, role)?;
        let mut first_error = None;
        for chosen in candidates {
            if self.remaining_candidates == 0 {
                return Err(PlanError::SearchLimit);
            }
            self.remaining_candidates -= 1;
            let saved_nodes = self.nodes.clone();
            let saved_order = self.order.clone();
            visiting.push(fact_type.to_string());
            let result = self.plan_node(&chosen, role, depth, visiting);
            visiting.pop();
            match result {
                Ok(id) => return Ok(Produced::Node(id)),
                Err(error) => {
                    self.nodes = saved_nodes;
                    self.order = saved_order;
                    if error == PlanError::SearchLimit {
                        return Err(error);
                    }
                    first_error.get_or_insert(error);
                }
            }
        }
        Err(first_error.expect("at least one candidate was tried"))
    }

    /// The kinds the subject in `role` is known by: `company` for a subject
    /// that arrived as a name or a KVK number. A fact of one of these kinds is
    /// a fact ABOUT THE SUBJECT, and is bought only from a source of that kind.
    fn subject_kinds(&self, role: &str) -> BTreeSet<String> {
        self.subject(role)
            .map(|s| s.known.keys().map(|k| kind_of(k)).collect())
            .unwrap_or_default()
    }

    /// Filter, then rank.
    ///
    /// Filtering first is not a performance choice: a cheaper candidate that
    /// cannot legally or technically run is not a cheaper candidate, and
    /// ranking before filtering is how a price ends up quoted for a call that
    /// was never going to happen.
    ///
    /// A fact about the subject itself — `company.country` for a company — is
    /// bought only from a source of the subject's kind. The vocabulary matches
    /// on key names, so an IP-geolocation body with a `country` key produces
    /// `company.country` as far as a contract can tell; on 2026-09-06 the
    /// cheapest such source was chosen for a two-company comparison, twice,
    /// and the buyer paid for two IP lookups that said nothing about either
    /// company. Among the sources that remain, one of the subject's own kind
    /// ranks before one of another kind; price decides among equals.
    fn best_candidates(&self, fact_type: &str, role: &str) -> Result<Vec<Candidate>, PlanError> {
        let mut refusal: Option<String> = None;
        let mut usable: Vec<&Candidate> = Vec::new();
        let fact_kind = kind_of(fact_type);
        let about_the_subject = self.subject_kinds(role).contains(&fact_kind);

        for candidate in self.candidates {
            if !candidate
                .contract
                .output_map
                .produces
                .contains_key(fact_type)
            {
                continue;
            }
            let source_kind = kind_of(&candidate.contract.capability_slug);
            if about_the_subject && source_kind != fact_kind {
                refusal.get_or_insert_with(|| {
                    format!("the only sources describe {source_kind}, not {fact_kind}")
                });
                continue;
            }
            // A source that takes nothing cannot be about the subject. Live,
            // a harvested GLEIF contract with no inputs was planned for a
            // company's country, called unfiltered, and returned the first
            // page of the world's LEI records as "nothing on record".
            // Not only for facts about the subject: a residents count is not
            // about a building, yet an input-less neighbourhood listing is
            // still a page of strangers, not an answer.
            if candidate.contract.requires.iter().all(|r| r.optional) {
                refusal.get_or_insert_with(|| {
                    format!("the only source takes nothing that identifies a {fact_kind}")
                });
                continue;
            }
            if !candidate.active {
                refusal.get_or_insert_with(|| "the only source is not active".to_string());
                continue;
            }
            if let (Some(wanted), Some(has)) = (&self.intent.jurisdiction, &candidate.jurisdiction)
            {
                if wanted != has {
                    refusal.get_or_insert_with(|| {
                        format!("the only source covers {has}, not {wanted}")
                    });
                    continue;
                }
            }
            if candidate.list_atomic <= 0 || candidate.list_atomic > i64::MAX as i128 / 2 {
                // A non-positive list price is refused downstream by the shared
                // execution path, so planning against it would quote work that
                // cannot run. Live today, every GLEIF capability offer points
                // at exactly such a row.
                refusal
                    .get_or_insert_with(|| "the only source has no verifiable price".to_string());
                continue;
            }
            usable.push(candidate);
        }

        // Cheapest first, then a stable tiebreak on contract id. Latency and
        // quality are deliberately absent: neither is measured yet, and a
        // weight on an unmeasured quantity is a guess wearing a number.
        usable.sort_by(|a, b| {
            let a_same = kind_of(&a.contract.capability_slug) == fact_kind;
            let b_same = kind_of(&b.contract.capability_slug) == fact_kind;
            b_same
                .cmp(&a_same)
                .then_with(|| {
                    quote::node_buyer_total(a.list_atomic, a.class)
                        .cmp(&quote::node_buyer_total(b.list_atomic, b.class))
                })
                .then_with(|| a.contract_id.cmp(&b.contract_id))
        });

        if usable.is_empty() {
            Err(PlanError::Unreachable {
                fact_type: fact_type.to_string(),
                reason: refusal,
            })
        } else {
            Ok(usable.into_iter().cloned().collect())
        }
    }

    fn plan_node(
        &mut self,
        candidate: &Candidate,
        role: &str,
        depth: usize,
        visiting: &mut Vec<String>,
    ) -> Result<String, PlanError> {
        let mut bindings: BTreeMap<String, Binding> = BTreeMap::new();
        let mut depends_on: BTreeSet<String> = BTreeSet::new();

        for requirement in &candidate.contract.requires {
            if requirement.optional
                && !candidate
                    .contract
                    .input_map
                    .contains_key(&requirement.fact_type)
            {
                continue;
            }
            // An optional input narrows a call; it never justifies buying
            // another one. A KVK search takes a postcode if the buyer has
            // it, but a plan that buys a building registry lookup just to
            // find a postcode to narrow a name search has doubled its price
            // for nothing the buyer asked for. So an optional input is bound
            // only from what is already known or already in the plan.
            let produced = if requirement.optional {
                match self.at_hand(&requirement.fact_type, role) {
                    Some(produced) => produced,
                    None => continue,
                }
            } else {
                self.satisfy(&requirement.fact_type, role, depth + 1, visiting)?
            };
            match produced {
                Produced::Known => {
                    let value = self
                        .subject(role)
                        .and_then(|s| s.known.get(&requirement.fact_type))
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    bindings.insert(requirement.fact_type.clone(), Binding::Known { value });
                }
                Produced::Node(node_id) => {
                    depends_on.insert(node_id.clone());
                    bindings.insert(requirement.fact_type.clone(), Binding::Node { node_id });
                }
            }
        }

        let key = NodeKey {
            contract_id: candidate.contract_id.clone(),
            subject_role: role.to_string(),
            bindings: bindings
                .iter()
                .map(|(fact_type, binding)| match binding {
                    Binding::Known { value } => format!("{fact_type}=known:{value}"),
                    Binding::Node { node_id } => format!("{fact_type}=node:{node_id}"),
                })
                .collect(),
        };
        let id = key.id();

        // Already planned: the same contract, the same subject, the same bound
        // inputs. One KVK lookup feeding both the profile and the accounts is
        // this line, and it is why the lookup appears once in the DAG and once
        // in the price.
        if self.nodes.contains_key(&id) {
            return Ok(id);
        }

        if self.nodes.len() + 1 > self.limits.max_paid_nodes {
            return Err(PlanError::TooManyNodes {
                found: self.nodes.len() + 1,
                limit: self.limits.max_paid_nodes,
            });
        }

        let mut produces: Vec<String> = candidate
            .contract
            .output_map
            .produces
            .keys()
            .cloned()
            .collect();
        produces.sort();

        let node = PlanNode {
            endpoint_method: candidate.endpoint_method.clone(),
            contract: Some(candidate.contract.clone()),
            id: id.clone(),
            contract_id: candidate.contract_id.clone(),
            endpoint_id: candidate.endpoint_id.clone(),
            capability_slug: candidate.contract.capability_slug.clone(),
            api_slug: candidate.api_slug.clone(),
            endpoint_path: candidate.endpoint_path.clone(),
            subject_role: role.to_string(),
            produces,
            depends_on: depends_on.into_iter().collect(),
            bindings,
            buyer_total_atomic: quote::node_buyer_total(candidate.list_atomic, candidate.class),
            may_ask: candidate.contract.cardinality == crate::facts::contract::Cardinality::Many,
        };
        self.nodes.insert(id.clone(), node);
        self.order.push(id.clone());
        Ok(id)
    }

    fn coverage_for(
        &self,
        fact_type: &str,
        role: &str,
        required: bool,
        produced: Produced,
    ) -> Coverage {
        match produced {
            Produced::Known => Coverage {
                fact_type: fact_type.to_string(),
                subject_role: role.to_string(),
                required,
                node_id: None,
                status: CoverageStatus::AlreadyKnown,
                reason: None,
            },
            Produced::Node(node_id) => Coverage {
                fact_type: fact_type.to_string(),
                subject_role: role.to_string(),
                required,
                node_id: Some(node_id),
                status: CoverageStatus::Planned,
                reason: None,
            },
        }
    }

    fn finish(self) -> Result<Vec<PlanNode>, PlanError> {
        let mut nodes = self.nodes;
        // `order` is already dependency order: `plan_node` inserts a node only
        // after every node it depends on has been inserted.
        Ok(self
            .order
            .iter()
            .filter_map(|id| nodes.remove(id))
            .collect())
    }
}

#[cfg(test)]
/// Convenience for callers that only have the default limits and no FX rate.
pub fn compile_default(intent: &Intent, candidates: &[Candidate]) -> Result<Plan, PlanError> {
    compile(intent, candidates, Limits::default(), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::contract::ExecutionContract;
    use crate::fees::ProviderClass;
    use serde_json::json;

    // The three contracts of the first chain, from the same fixtures migration
    // 112 seeds, paired with the real live prices and endpoint ids read out of
    // `api_endpoints`. Nothing here is a plausible-looking invention.
    const ZOEKEN: &str = include_str!("../../fixtures/contracts/kvk-zoeken.json");
    const BASISPROFIEL: &str = include_str!("../../fixtures/contracts/kvk-basisprofiel.json");
    const JAARREKENINGEN: &str = include_str!("../../fixtures/contracts/kvk-jaarrekeningen.json");

    /// KVK `/api/v2/zoeken`, list 0.018903 USDC.
    const ZOEKEN_LIST: i128 = 18_903;
    /// KVK `/api/v1/basisprofielen` and the jaarrekeningen route, both 0.066162.
    const PROFIEL_LIST: i128 = 66_162;

    fn contract(text: &str) -> ExecutionContract {
        serde_json::from_str(text).expect("a contract fixture must parse")
    }

    fn candidate(text: &str, id: &str, path: &str, list_atomic: i128) -> Candidate {
        Candidate {
            endpoint_method: "GET".into(),
            contract_id: id.to_string(),
            endpoint_id: format!("e_{id}"),
            api_slug: "kvk-dutch-business-register".to_string(),
            endpoint_path: path.to_string(),
            contract: contract(text),
            list_atomic,
            class: ProviderClass::Managed,
            active: true,
            jurisdiction: Some("NL".to_string()),
        }
    }

    fn catalogue() -> Vec<Candidate> {
        vec![
            candidate(ZOEKEN, "c_zoeken", "/api/v2/zoeken", ZOEKEN_LIST),
            candidate(
                BASISPROFIEL,
                "c_profiel",
                "/api/v1/basisprofielen",
                PROFIEL_LIST,
            ),
            candidate(
                JAARREKENINGEN,
                "c_jaar",
                "/api/v1/hvds/jaarrekeningen/kvknummer",
                PROFIEL_LIST,
            ),
        ]
    }

    fn from_a_name() -> Intent {
        Intent {
            subjects: vec![Subject {
                role: "subject".to_string(),
                known: [("company.name".to_string(), json!("Mollie B.V."))]
                    .into_iter()
                    .collect(),
            }],
            required_outputs: vec![
                "company.profile".to_string(),
                "company.financial_statements".to_string(),
            ],
            optional_outputs: vec![],
            format: OutputFormat::Json,
            max_buyer_total_atomic: None,
            jurisdiction: Some("NL".to_string()),
        }
    }

    // --- the acceptance case ------------------------------------------------

    #[test]
    fn one_lookup_serves_two_dependent_outputs_and_is_charged_once() {
        let plan = compile_default(&from_a_name(), &catalogue()).unwrap();

        // Three nodes: one search, the profile, the accounts. Not four.
        assert_eq!(plan.paid_nodes(), 3);
        let searches = plan
            .nodes
            .iter()
            .filter(|n| n.capability_slug == "company_registry.search")
            .count();
        assert_eq!(searches, 1, "the identity lookup is one node, not two");

        // Both dependants point at that one node.
        let search = plan
            .nodes
            .iter()
            .find(|n| n.capability_slug == "company_registry.search")
            .unwrap();
        for slug in ["company.registry-profile", "company.financial-statements"] {
            let node = plan
                .nodes
                .iter()
                .find(|n| n.capability_slug == slug)
                .unwrap();
            assert_eq!(node.depends_on, vec![search.id.clone()]);
        }

        // And it is in the price exactly once.
        assert_eq!(plan.quote.lines.len(), 3);
        let search_lines = plan
            .quote
            .lines
            .iter()
            .filter(|l| l.capability_slug == "company_registry.search")
            .count();
        assert_eq!(search_lines, 1);

        let expected = quote::node_buyer_total(ZOEKEN_LIST, ProviderClass::Managed)
            + quote::node_buyer_total(PROFIEL_LIST, ProviderClass::Managed) * 2;
        assert_eq!(plan.quote.buyer_total_atomic, expected);
    }

    #[test]
    fn a_fact_the_plan_already_buys_is_not_bought_again_from_a_cheaper_source() {
        // The profile lookup also yields the website. A cheaper website-only
        // source exists, but the profile call is already in the price, so the
        // website comes from it and the plan stays at two paid nodes, not three.
        let mut profiel: serde_json::Value = serde_json::from_str(BASISPROFIEL).unwrap();
        profiel["output_map"]["produces"]["company.website"] = json!({ "path": "$.website" });
        let mut cheap: serde_json::Value = serde_json::from_str(ZOEKEN).unwrap();
        cheap["capability_slug"] = json!("company.enrich");
        cheap["output_map"]["produces"] = json!({ "company.website": { "path": "$.w" } });
        cheap["requires"] = json!([{
            "fact_type": "company.registration.nl.kvk",
            "subject_role": "subject",
            "optional": false
        }]);
        let catalogue = vec![
            candidate(ZOEKEN, "c_zoeken", "/api/v2/zoeken", ZOEKEN_LIST),
            candidate(
                &profiel.to_string(),
                "c_profiel",
                "/api/v1/basisprofielen",
                PROFIEL_LIST,
            ),
            candidate(&cheap.to_string(), "c_cheap", "/enrich", 1_000),
        ];
        let mut intent = from_a_name();
        intent.required_outputs =
            vec!["company.profile".to_string(), "company.website".to_string()];

        let plan = compile_default(&intent, &catalogue).unwrap();
        assert_eq!(
            plan.paid_nodes(),
            2,
            "search and profile; no third call for the website"
        );
        assert!(plan
            .nodes
            .iter()
            .all(|n| n.capability_slug != "company.enrich"));
        let website = plan
            .coverage
            .iter()
            .find(|c| c.fact_type == "company.website")
            .unwrap();
        let profile = plan
            .nodes
            .iter()
            .find(|n| n.capability_slug == "company.registry-profile")
            .unwrap();
        assert_eq!(website.node_id.as_deref(), Some(profile.id.as_str()));
    }

    #[test]
    fn the_dependants_run_in_parallel_so_the_critical_path_is_two() {
        let plan = compile_default(&from_a_name(), &catalogue()).unwrap();
        assert_eq!(plan.critical_path(), 2);
    }

    #[test]
    fn a_known_kvk_number_removes_the_lookup_from_the_dag_and_from_the_price() {
        let mut intent = from_a_name();
        intent.subjects[0]
            .known
            .insert("company.registration.nl.kvk".to_string(), json!("30204462"));
        let plan = compile_default(&intent, &catalogue()).unwrap();

        assert_eq!(plan.paid_nodes(), 2);
        assert!(plan
            .nodes
            .iter()
            .all(|n| n.capability_slug != "company_registry.search"));
        assert!(plan.nodes.iter().all(|n| n.depends_on.is_empty()));

        // The saving is exactly the lookup, and nothing else moved.
        let expected = quote::node_buyer_total(PROFIEL_LIST, ProviderClass::Managed) * 2;
        assert_eq!(plan.quote.buyer_total_atomic, expected);
        let with_lookup = compile_default(&from_a_name(), &catalogue())
            .unwrap()
            .quote
            .buyer_total_atomic;
        assert_eq!(
            with_lookup - plan.quote.buyer_total_atomic,
            quote::node_buyer_total(ZOEKEN_LIST, ProviderClass::Managed)
        );

        // Both nodes now bind the number the caller gave, not a node output.
        for node in &plan.nodes {
            assert_eq!(
                node.bindings.get("company.registration.nl.kvk"),
                Some(&Binding::Known {
                    value: json!("30204462")
                })
            );
        }
        assert_eq!(plan.critical_path(), 1);
    }

    #[test]
    fn the_plan_says_where_it_may_stop_to_ask() {
        let plan = compile_default(&from_a_name(), &catalogue()).unwrap();
        let search = plan
            .nodes
            .iter()
            .find(|n| n.capability_slug == "company_registry.search")
            .unwrap();
        // A search can return several companies, so the plan discloses that
        // this step may pause. Approving a plan includes approving that.
        assert!(search.may_ask);
        assert!(plan
            .nodes
            .iter()
            .filter(|n| n.capability_slug != "company_registry.search")
            .all(|n| !n.may_ask));
    }

    #[test]
    fn coverage_names_what_came_from_where() {
        let mut intent = from_a_name();
        intent
            .subjects
            .get_mut(0)
            .unwrap()
            .known
            .insert("company.profile".to_string(), json!({ "legal_name": "x" }));
        let plan = compile_default(&intent, &catalogue()).unwrap();
        let profile = plan
            .coverage
            .iter()
            .find(|c| c.fact_type == "company.profile")
            .unwrap();
        assert_eq!(profile.status, CoverageStatus::AlreadyKnown);
        assert!(profile.node_id.is_none());

        let filing = plan
            .coverage
            .iter()
            .find(|c| c.fact_type == "company.financial_statements")
            .unwrap();
        assert_eq!(filing.status, CoverageStatus::Planned);
        assert!(filing.node_id.is_some());
    }

    // --- what is refused, and with what reason ------------------------------

    #[test]
    fn an_output_nothing_produces_is_refused_by_name() {
        let mut intent = from_a_name();
        intent.required_outputs = vec!["company.insolvency".to_string()];
        let error = compile_default(&intent, &catalogue()).unwrap_err();
        assert!(
            matches!(&error, PlanError::Unreachable { fact_type, .. } if fact_type == "company.insolvency")
        );
        // The message names the fact, so a person is told what is missing
        // rather than that "the plan failed".
        assert!(error.to_string().contains("company.insolvency"));
    }

    #[test]
    fn an_inactive_endpoint_is_not_a_candidate_and_the_refusal_says_why() {
        // Live today, `company.registry-profile` also has an offer on the HVD
        // route whose `active` is false, with an active resolver still
        // pointing at it. An inactive row must never be planned against.
        let mut catalogue = catalogue();
        catalogue[1].active = false;
        let plan = compile_default(&from_a_name(), &catalogue).unwrap();
        let profile = unreachable(&plan, "company.profile");
        assert!(profile.contains("not active"), "{profile}");
        // The accounts, which do not need the profile, are still bought.
        assert_eq!(plan.paid_nodes(), 2);
    }

    /// The reason a plan gives for not covering `fact`. Panics if it is covered.
    fn unreachable(plan: &Plan, fact: &str) -> String {
        let line = plan.coverage.iter().find(|c| c.fact_type == fact).unwrap();
        assert_eq!(
            line.status,
            CoverageStatus::Unreachable,
            "{fact} was planned"
        );
        assert!(line.required, "{fact} was wanted");
        line.reason.clone().unwrap_or_default()
    }

    #[test]
    fn a_source_with_no_verifiable_price_is_not_planned_against() {
        // GLEIF's `/api/v1/lei-records` lists at zero and the shared execution
        // path refuses a non-positive price. Quoting it would price work that
        // cannot run.
        let mut catalogue = catalogue();
        catalogue[1].list_atomic = 0;
        let plan = compile_default(&from_a_name(), &catalogue).unwrap();
        let profile = unreachable(&plan, "company.profile");
        assert!(profile.contains("no verifiable price"), "{profile}");
    }

    #[test]
    fn a_source_for_the_wrong_jurisdiction_is_refused_by_name() {
        let mut catalogue = catalogue();
        catalogue[1].jurisdiction = Some("BE".to_string());
        let plan = compile_default(&from_a_name(), &catalogue).unwrap();
        let profile = unreachable(&plan, "company.profile");
        assert!(profile.contains("covers BE, not NL"), "{profile}");
    }

    #[test]
    fn a_fact_about_the_subject_is_not_bought_from_a_source_of_another_kind() {
        // `company.country` is also what an IP-geolocation body's `country`
        // key matches. For a company, that source is not a source: the fact
        // is left uncovered, with the reason, rather than bought for nothing.
        let mut geo: serde_json::Value = serde_json::from_str(ZOEKEN).unwrap();
        geo["capability_slug"] = json!("ip.geolocate");
        geo["output_map"]["produces"] = json!({ "company.country": { "path": "$.country" } });
        geo["requires"] = json!([]);
        let mut catalogue = catalogue();
        catalogue.push(candidate(&geo.to_string(), "c_geo", "/api/geo", 1_000));
        let mut intent = from_a_name();
        intent.required_outputs.push("company.country".to_string());

        let plan = compile_default(&intent, &catalogue).unwrap();
        assert!(plan
            .nodes
            .iter()
            .all(|n| n.capability_slug != "ip.geolocate"));
        let country = unreachable(&plan, "company.country");
        assert!(country.contains("describe ip, not company"), "{country}");
        assert_eq!(plan.paid_nodes(), 3, "the company plan itself is untouched");
    }

    #[test]
    fn when_nothing_wanted_can_be_bought_the_plan_is_refused_with_the_first_reason() {
        let mut intent = from_a_name();
        intent.required_outputs = vec![
            "company.insolvency".to_string(),
            "company.beneficial_owners".to_string(),
        ];
        let error = compile_default(&intent, &catalogue()).unwrap_err();
        assert!(
            matches!(&error, PlanError::Unreachable { fact_type, .. } if fact_type == "company.insolvency")
        );
    }

    #[test]
    fn a_plan_over_the_ceiling_is_refused_with_its_price() {
        let mut intent = from_a_name();
        intent.max_buyer_total_atomic = Some(1_000);
        let error = compile_default(&intent, &catalogue()).unwrap_err();
        let PlanError::TooExpensive { found, limit } = error else {
            panic!("expected a price refusal, got {error}");
        };
        assert_eq!(limit, 1_000);
        // Refused WITH the number, so the caller can raise the ceiling or drop
        // an output rather than guess.
        assert!(found > 1_000);
    }

    #[test]
    fn a_plan_exactly_on_the_ceiling_is_allowed() {
        let plan = compile_default(&from_a_name(), &catalogue()).unwrap();
        let mut intent = from_a_name();
        intent.max_buyer_total_atomic = Some(plan.quote.buyer_total_atomic);
        assert!(compile_default(&intent, &catalogue()).is_ok());
    }

    #[test]
    fn a_cycle_is_refused_rather_than_unrolled() {
        // A contract that needs its own output. Unrolling it is an unbounded
        // bill, so it is refused before anything is priced.
        let mut looping = contract(BASISPROFIEL);
        looping.requires = vec![crate::facts::contract::Requirement {
            fact_type: "company.profile".to_string(),
            subject_role: "subject".to_string(),
            optional: false,
        }];
        looping.input_map = [(
            "company.profile".to_string(),
            crate::facts::contract::InputBinding {
                location: None,
                name: "profile".to_string(),
                wrap: None,
                default: None,
            },
        )]
        .into_iter()
        .collect();
        let catalogue = vec![Candidate {
            endpoint_method: "GET".into(),
            contract: looping,
            ..candidate(BASISPROFIEL, "c_loop", "/loop", PROFIEL_LIST)
        }];
        let mut intent = from_a_name();
        intent.required_outputs = vec!["company.profile".to_string()];
        assert!(matches!(
            compile_default(&intent, &catalogue),
            Err(PlanError::Cycle { .. })
        ));
    }

    #[test]
    fn too_many_paid_calls_is_refused_with_both_numbers() {
        let mut limits = Limits::default();
        limits.max_paid_nodes = 2;
        let error = compile(&from_a_name(), &catalogue(), limits, None).unwrap_err();
        assert!(matches!(
            error,
            PlanError::TooManyNodes { found: 3, limit: 2 }
        ));
    }

    #[test]
    fn too_deep_is_refused() {
        let mut limits = Limits::default();
        limits.max_depth = 1;
        let error = compile(&from_a_name(), &catalogue(), limits, None).unwrap_err();
        assert!(matches!(error, PlanError::TooDeep { limit: 1, .. }));
    }

    #[test]
    fn an_intent_with_nothing_wanted_or_nobody_to_ask_about_is_refused() {
        let mut empty = from_a_name();
        empty.required_outputs.clear();
        assert!(matches!(
            compile_default(&empty, &catalogue()),
            Err(PlanError::NothingWanted)
        ));

        let mut nobody = from_a_name();
        nobody.subjects.clear();
        assert!(matches!(
            compile_default(&nobody, &catalogue()),
            Err(PlanError::NoSubject)
        ));
    }

    #[test]
    fn an_unreachable_optional_output_is_recorded_and_does_not_sink_the_plan() {
        let mut intent = from_a_name();
        intent.optional_outputs = vec!["building.attributes".to_string()];
        let plan = compile_default(&intent, &catalogue()).unwrap();
        let missing = plan
            .coverage
            .iter()
            .find(|c| c.fact_type == "building.attributes")
            .unwrap();
        assert_eq!(missing.status, CoverageStatus::Unreachable);
        assert!(!missing.required);
        assert_eq!(plan.paid_nodes(), 3, "no node was added for it");
    }

    // --- deduplication is on the work, not on the name ----------------------

    #[test]
    fn two_different_companies_are_two_lookups_and_never_merged() {
        let intent = Intent {
            subjects: vec![
                Subject {
                    role: "subject".to_string(),
                    known: [("company.name".to_string(), json!("Mollie B.V."))]
                        .into_iter()
                        .collect(),
                },
                Subject {
                    role: "counterparty".to_string(),
                    known: [("company.name".to_string(), json!("Adyen N.V."))]
                        .into_iter()
                        .collect(),
                },
            ],
            required_outputs: vec!["company.profile".to_string()],
            optional_outputs: vec![],
            format: OutputFormat::Json,
            max_buyer_total_atomic: None,
            jurisdiction: Some("NL".to_string()),
        };

        // A chain each: the lookup and the profile, twice over, and the wanted
        // fact covered once per company.
        let plan = compile_default(&intent, &catalogue()).unwrap();
        assert_eq!(plan.paid_nodes(), 4);
        let ids: BTreeSet<&str> = plan.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids.len(), 4, "two companies must not share a node id");
        for role in ["subject", "counterparty"] {
            assert_eq!(
                plan.nodes.iter().filter(|n| n.subject_role == role).count(),
                2
            );
            let covered = plan
                .coverage
                .iter()
                .find(|c| c.subject_role == role && c.fact_type == "company.profile")
                .expect("each company is covered in its own right");
            assert_eq!(covered.status, CoverageStatus::Planned);
            let node = covered.node_id.as_deref().expect("a node produces it");
            assert!(plan
                .nodes
                .iter()
                .any(|n| n.id == node && n.subject_role == role));
        }

        // Every node depends only on its own company's lookup: a binding that
        // crossed roles would research one company and label it the other.
        for node in &plan.nodes {
            for dependency in &node.depends_on {
                let upstream = plan.nodes.iter().find(|n| &n.id == dependency).unwrap();
                assert_eq!(upstream.subject_role, node.subject_role);
            }
        }
    }

    #[test]
    fn a_two_company_comparison_is_one_plan_and_fits_the_budget() {
        // The example question off the Ask page: two payment companies, their
        // profiles and their filed accounts. It used to stop and ask which
        // company to research first, which is not an answer to it.
        let mut intent = from_a_name();
        intent.subjects = ["Adyen N.V.", "Mollie B.V."]
            .iter()
            .enumerate()
            .map(|(n, name)| Subject {
                role: if n == 0 {
                    "subject".to_string()
                } else {
                    format!("subject_{}", n + 1)
                },
                known: [("company.name".to_string(), json!(name))]
                    .into_iter()
                    .collect(),
            })
            .collect();
        let plan = compile_default(&intent, &catalogue()).unwrap();

        // A lookup, a profile and an accounts call for each company: six paid
        // calls, and the identity lookup shared within a company but never
        // across two.
        assert_eq!(plan.paid_nodes(), 6);
        assert_eq!(plan.critical_path(), 2);
        for role in ["subject", "subject_2"] {
            let mine: Vec<&PlanNode> = plan
                .nodes
                .iter()
                .filter(|n| n.subject_role == role)
                .collect();
            assert_eq!(mine.len(), 3);
            // One rootless node — the identity lookup — and the other two hang
            // off it. A second lookup for the same company would be paid twice.
            let roots: Vec<&&PlanNode> = mine.iter().filter(|n| n.depends_on.is_empty()).collect();
            assert_eq!(roots.len(), 1, "one identity lookup per company");
            assert!(mine
                .iter()
                .filter(|n| !n.depends_on.is_empty())
                .all(|n| n.depends_on == vec![roots[0].id.clone()]));
        }
        // Both companies are covered for both facts, and nothing is short.
        assert_eq!(plan.coverage.len(), 4);
        assert!(plan
            .coverage
            .iter()
            .all(|c| c.status == CoverageStatus::Planned && c.required));
        assert_eq!(
            plan.quote.buyer_total_atomic,
            2 * from_one_company_total(&catalogue())
        );
    }

    /// What the same two facts cost for a single company.
    fn from_one_company_total(catalogue: &[Candidate]) -> i128 {
        compile_default(&from_a_name(), catalogue)
            .unwrap()
            .quote
            .buyer_total_atomic
    }

    #[test]
    fn more_companies_than_the_budget_holds_is_refused_with_the_count() {
        // No policy ceiling on how many companies a question may name: the
        // paid-node budget is the only limit, and it refuses with arithmetic
        // rather than asking which company to drop.
        let mut intent = from_a_name();
        intent.subjects = (0..8)
            .map(|n| Subject {
                role: if n == 0 {
                    "subject".to_string()
                } else {
                    format!("subject_{}", n + 1)
                },
                known: [("company.name".to_string(), json!(format!("Company {n}")))]
                    .into_iter()
                    .collect(),
            })
            .collect();
        assert!(matches!(
            compile_default(&intent, &catalogue()),
            Err(PlanError::TooManyNodes {
                found: 13,
                limit: 12
            })
        ));
    }

    #[test]
    fn an_optional_input_is_never_bought_only_bound_when_at_hand() {
        // The search takes a postcode to narrow a name search. A source that
        // sells postcodes exists. The plan must not buy it: an optional input
        // narrows a call, it does not justify one. Live, this was two PDOK
        // calls and thirty thousand atomic on top of a three-node KVK plan.
        let mut zoeken: serde_json::Value = serde_json::from_str(ZOEKEN).unwrap();
        zoeken["requires"].as_array_mut().unwrap().push(json!({
            "fact_type": "address.postal",
            "subject_role": "subject",
            "optional": true
        }));
        zoeken["input_map"]["address.postal"] = json!({ "in": "query", "name": "postcode" });
        let mut postal: serde_json::Value = serde_json::from_str(ZOEKEN).unwrap();
        postal["capability_slug"] = json!("address.lookup");
        postal["output_map"]["produces"] = json!({ "address.postal": { "path": "$.pc" } });
        let catalogue = vec![
            candidate(
                &zoeken.to_string(),
                "c_zoeken",
                "/api/v2/zoeken",
                ZOEKEN_LIST,
            ),
            candidate(
                BASISPROFIEL,
                "c_profiel",
                "/api/v1/basisprofielen",
                PROFIEL_LIST,
            ),
            candidate(
                JAARREKENINGEN,
                "c_jaar",
                "/api/v1/hvds/jaarrekeningen/kvknummer",
                PROFIEL_LIST,
            ),
            candidate(&postal.to_string(), "c_postal", "/postal", 1_000),
        ];

        let plan = compile_default(&from_a_name(), &catalogue).unwrap();
        assert_eq!(
            plan.paid_nodes(),
            3,
            "the postcode source must not be bought"
        );
        assert!(plan
            .nodes
            .iter()
            .all(|n| n.capability_slug != "address.lookup"));
        let search = plan
            .nodes
            .iter()
            .find(|n| n.capability_slug == "company_registry.search")
            .unwrap();
        assert!(!search.bindings.contains_key("address.postal"));

        // Supplied by the buyer, it IS bound, and still costs nothing extra.
        let mut intent = from_a_name();
        intent.subjects[0]
            .known
            .insert("address.postal".to_string(), json!("1016 EE"));
        let plan = compile_default(&intent, &catalogue).unwrap();
        assert_eq!(plan.paid_nodes(), 3);
        let search = plan
            .nodes
            .iter()
            .find(|n| n.capability_slug == "company_registry.search")
            .unwrap();
        assert!(matches!(
            search.bindings.get("address.postal"),
            Some(Binding::Known { .. })
        ));
    }

    #[test]
    fn a_source_that_takes_nothing_is_not_a_source_about_the_subject() {
        // An input-less contract that "produces" the country. It cannot be
        // filtered to this company, so it is refused by name rather than
        // called unfiltered and charged.
        let mut unbound: serde_json::Value = serde_json::from_str(ZOEKEN).unwrap();
        unbound["capability_slug"] = json!("company.lei-verification");
        unbound["requires"] = json!([]);
        unbound["input_map"] = json!({});
        unbound["output_map"]["produces"] = json!({ "company.country": { "path": "$.c" } });
        let mut catalogue = catalogue();
        catalogue.push(candidate(
            &unbound.to_string(),
            "c_unbound",
            "/lei-records",
            1_000,
        ));
        let mut intent = from_a_name();
        intent.required_outputs = vec!["company.country".to_string()];
        let error = compile_default(&intent, &catalogue).unwrap_err();
        let PlanError::Unreachable { fact_type, reason } = error else {
            panic!("expected a refusal by name, got {error}");
        };
        assert_eq!(fact_type, "company.country");
        let reason = reason.unwrap_or_default();
        assert!(reason.contains("takes nothing"), "{reason}");
    }

    #[test]
    fn the_same_intent_compiles_to_the_same_node_ids_every_time() {
        // What lets the App and an MCP client show the same plan and the same
        // amount without either of them planning.
        let first = compile_default(&from_a_name(), &catalogue()).unwrap();
        let second = compile_default(&from_a_name(), &catalogue()).unwrap();
        assert_eq!(first.nodes, second.nodes);
        assert_eq!(
            first.quote.buyer_total_atomic,
            second.quote.buyer_total_atomic
        );
    }

    // --- price -------------------------------------------------------------

    #[test]
    fn the_euro_figure_is_snapshotted_with_its_rate() {
        let plan = compile(
            &from_a_name(),
            &catalogue(),
            Limits::default(),
            Some(920_000),
        )
        .unwrap();
        let eur = plan.quote.eur.as_ref().unwrap();
        assert_eq!(eur.rate_micro, 920_000);
        assert_eq!(
            eur.cents,
            quote::euro_cents(plan.quote.buyer_total_atomic, 920_000).unwrap()
        );
    }

    #[test]
    fn without_a_rate_there_is_no_euro_figure_rather_than_a_guessed_one() {
        let plan = compile_default(&from_a_name(), &catalogue()).unwrap();
        assert!(plan.quote.eur.is_none());
    }

    #[test]
    fn the_plan_echoes_the_limits_it_was_compiled_under() {
        let plan = compile_default(&from_a_name(), &catalogue()).unwrap();
        assert_eq!(plan.limits, Limits::default());
        assert_eq!(plan.limits.max_paid_nodes, 12);
        assert_eq!(plan.limits.max_depth, 4);
        assert_eq!(plan.limits.max_concurrency, 3);
    }

    #[test]
    fn a_plan_round_trips_through_json() {
        let plan = compile_default(&from_a_name(), &catalogue()).unwrap();
        let text = serde_json::to_string(&plan).unwrap();
        let back: Plan = serde_json::from_str(&text).unwrap();
        assert_eq!(plan, back);
    }
}
