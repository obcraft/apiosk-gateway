//! Which node may run next, and what the job's status actually is.
//!
//! Pure functions over a snapshot of node rows. No database, no clock beyond
//! the one passed in, no side effects — because every rule in here is a
//! release gate, and a release gate that can only be checked by running a
//! distributed system is not checked.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Where one node stands. Mirrors the `status` check constraint of
/// `research_job_nodes` (migration 114).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Queued,
    Running,
    /// Stopped to ask something. Its dependants wait; they are not blocked.
    NeedsInput,
    Paused,
    Succeeded,
    Failed,
    /// A dependency failed, so this can never run. Not the same as failed: it
    /// was never attempted, and nothing was ever spent on it.
    Blocked,
    /// Deliberately not run — an optional branch the plan dropped.
    Skipped,
    Cancelled,
}

impl NodeStatus {
    /// Terminal: nothing this node does will change from here.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Blocked | Self::Skipped | Self::Cancelled
        )
    }

    /// Its dependants can never run.
    fn poisons_dependants(self) -> bool {
        matches!(self, Self::Failed | Self::Blocked | Self::Cancelled)
    }
}

/// One node, as the scheduler sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeState {
    pub key: String,
    pub depends_on: Vec<String>,
    pub status: NodeStatus,
    /// Set while a worker holds this node. A lease in the past is not a lease.
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub attempts: i32,
    pub max_attempts: i32,
    /// Whether a required output of the job depends on this node succeeding.
    /// Drives the difference between `partial` and `succeeded`.
    pub required: bool,
}

/// Why a node cannot be dispatched right now. Returned rather than silently
/// skipped so a job's own record can say what it is waiting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Withheld {
    /// A dependency has not finished yet.
    WaitingOnDependency,
    /// A dependency failed; this branch is dead.
    BranchFailed,
    /// The plan's concurrency limit is already met.
    AtConcurrencyLimit,
    /// It has used all its attempts.
    OutOfAttempts,
}

/// What the scheduler decided about one node this tick.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub key: String,
    pub withheld: Option<Withheld>,
}

/// The nodes that may be dispatched now, in plan order.
///
/// A node runs only when EVERY fact it needs is validated and available, which
/// here means every dependency has succeeded. A dependency that is merely
/// finished is not enough: a failed lookup produced no KVK number, and a
/// profile call bound to nothing would spend money to ask about a company
/// nobody identified.
pub fn dispatchable(
    nodes: &[NodeState],
    now: DateTime<Utc>,
    max_concurrency: usize,
) -> Vec<String> {
    let by_key: BTreeMap<&str, &NodeState> =
        nodes.iter().map(|node| (node.key.as_str(), node)).collect();

    let running = nodes
        .iter()
        .filter(|node| node.status == NodeStatus::Running && holds_lease(node, now))
        .count();
    let mut room = max_concurrency.saturating_sub(running);

    let mut out = Vec::new();
    for node in nodes {
        if room == 0 {
            break;
        }
        // A running node whose lease has lapsed is claimable again. That is the
        // whole recovery story for a worker that died mid-node: the lease runs
        // out and someone else picks it up.
        let claimable = node.status == NodeStatus::Queued
            || (node.status == NodeStatus::Running && !holds_lease(node, now));
        if !claimable {
            continue;
        }
        if node.attempts >= node.max_attempts {
            continue;
        }
        if !dependencies_are_ready(node, &by_key) {
            continue;
        }
        out.push(node.key.clone());
        room -= 1;
    }
    out
}

/// The same walk, but reporting why each node was withheld. For a job's own
/// progress record: "waiting on the KVK lookup" is an answer; a spinner is not.
pub fn explain(nodes: &[NodeState], now: DateTime<Utc>, max_concurrency: usize) -> Vec<Decision> {
    let by_key: BTreeMap<&str, &NodeState> =
        nodes.iter().map(|node| (node.key.as_str(), node)).collect();
    let running = nodes
        .iter()
        .filter(|node| node.status == NodeStatus::Running && holds_lease(node, now))
        .count();
    let mut room = max_concurrency.saturating_sub(running);

    nodes
        .iter()
        .filter(|node| {
            node.status == NodeStatus::Queued
                || (node.status == NodeStatus::Running && !holds_lease(node, now))
        })
        .map(|node| {
            let withheld = if branch_is_dead(node, &by_key) {
                Some(Withheld::BranchFailed)
            } else if node.attempts >= node.max_attempts {
                Some(Withheld::OutOfAttempts)
            } else if !dependencies_are_ready(node, &by_key) {
                Some(Withheld::WaitingOnDependency)
            } else if room == 0 {
                Some(Withheld::AtConcurrencyLimit)
            } else {
                room -= 1;
                None
            };
            Decision {
                key: node.key.clone(),
                withheld,
            }
        })
        .collect()
}

fn holds_lease(node: &NodeState, now: DateTime<Utc>) -> bool {
    node.lease_expires_at.is_some_and(|until| until > now)
}

fn dependencies_are_ready(node: &NodeState, by_key: &BTreeMap<&str, &NodeState>) -> bool {
    node.depends_on.iter().all(|key| {
        by_key
            .get(key.as_str())
            .is_some_and(|dependency| dependency.status == NodeStatus::Succeeded)
    })
}

fn branch_is_dead(node: &NodeState, by_key: &BTreeMap<&str, &NodeState>) -> bool {
    node.depends_on.iter().any(|key| {
        by_key
            .get(key.as_str())
            .is_none_or(|dependency| dependency.status.poisons_dependants())
    })
}

/// Nodes that must be marked `blocked`: a dependency failed, so they can never
/// run and must not sit in `queued` forever pretending they might.
///
/// Returns keys rather than mutating, so the caller writes one statement and
/// the transition is visible in the job's event stream.
pub fn newly_blocked(nodes: &[NodeState]) -> Vec<String> {
    let by_key: BTreeMap<&str, &NodeState> =
        nodes.iter().map(|node| (node.key.as_str(), node)).collect();
    nodes
        .iter()
        .filter(|node| !node.status.is_terminal() && node.status != NodeStatus::Running)
        .filter(|node| branch_is_dead(node, &by_key))
        .map(|node| node.key.clone())
        .collect()
}

/// Where the job stands. Mirrors the `status` constraint of `research_jobs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    NeedsInput,
    Paused,
    Succeeded,
    Partial,
    Failed,
    Cancelled,
}

/// Derive the job's status from its nodes.
///
/// The rule that matters is the last one: a job is `succeeded` only when every
/// REQUIRED node succeeded. Today, one successful run marks every step of an
/// ask plan answered even when other calls failed; for research that is a false
/// claim rather than an approximation. `partial` is a real terminal state here,
/// and it means exactly "some required outputs were proven and some were not".
pub fn job_status(nodes: &[NodeState], cancelled: bool) -> JobStatus {
    if cancelled {
        return JobStatus::Cancelled;
    }
    if nodes.is_empty() {
        return JobStatus::Queued;
    }
    // Asking beats running: a job that is waiting on a person is not making
    // progress, and showing it as running invites everyone to wait for nothing.
    if nodes.iter().any(|n| n.status == NodeStatus::NeedsInput) {
        return JobStatus::NeedsInput;
    }
    if nodes.iter().any(|n| n.status == NodeStatus::Paused) {
        return JobStatus::Paused;
    }
    if nodes.iter().any(|n| n.status == NodeStatus::Running) {
        return JobStatus::Running;
    }
    if !nodes.iter().all(|n| n.status.is_terminal()) {
        return JobStatus::Queued;
    }

    let required: Vec<&NodeState> = nodes.iter().filter(|n| n.required).collect();
    if required.is_empty() {
        // Nothing was required, and everything is finished. Whether any
        // optional branch worked is a coverage question, not a status one.
        return JobStatus::Succeeded;
    }
    let proven = required
        .iter()
        .filter(|n| n.status == NodeStatus::Succeeded)
        .count();
    match proven {
        0 => JobStatus::Failed,
        n if n == required.len() => JobStatus::Succeeded,
        _ => JobStatus::Partial,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 5, 12, 0, 0).unwrap()
    }

    fn node(key: &str, status: NodeStatus, depends_on: &[&str]) -> NodeState {
        NodeState {
            key: key.to_string(),
            depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
            status,
            lease_expires_at: None,
            attempts: 0,
            max_attempts: 3,
            required: true,
        }
    }

    /// The first chain: a search, then a profile and a filing that both need it.
    fn chain(search: NodeStatus, profile: NodeStatus, filing: NodeStatus) -> Vec<NodeState> {
        vec![
            node("search", search, &[]),
            node("profile", profile, &["search"]),
            node("filing", filing, &["search"]),
        ]
    }

    // --- dispatch -----------------------------------------------------------

    #[test]
    fn only_the_root_runs_first() {
        let nodes = chain(NodeStatus::Queued, NodeStatus::Queued, NodeStatus::Queued);
        assert_eq!(dispatchable(&nodes, now(), 3), vec!["search"]);
    }

    #[test]
    fn both_branches_run_together_once_the_lookup_has_succeeded() {
        let nodes = chain(
            NodeStatus::Succeeded,
            NodeStatus::Queued,
            NodeStatus::Queued,
        );
        assert_eq!(dispatchable(&nodes, now(), 3), vec!["profile", "filing"]);
    }

    #[test]
    fn a_dependency_that_merely_finished_is_not_enough() {
        // A failed lookup produced no KVK number. Binding a profile call to
        // nothing would spend money asking about a company nobody identified.
        for finished in [
            NodeStatus::Failed,
            NodeStatus::Cancelled,
            NodeStatus::Blocked,
        ] {
            let nodes = chain(finished, NodeStatus::Queued, NodeStatus::Queued);
            assert!(
                dispatchable(&nodes, now(), 3).is_empty(),
                "{finished:?} must not release its dependants"
            );
        }
    }

    #[test]
    fn a_node_that_is_asking_holds_its_dependants_without_killing_them() {
        let nodes = chain(
            NodeStatus::NeedsInput,
            NodeStatus::Queued,
            NodeStatus::Queued,
        );
        assert!(dispatchable(&nodes, now(), 3).is_empty());
        // And they are NOT blocked: an answer is still coming.
        assert!(newly_blocked(&nodes).is_empty());
    }

    #[test]
    fn the_concurrency_limit_is_respected() {
        let nodes = chain(
            NodeStatus::Succeeded,
            NodeStatus::Queued,
            NodeStatus::Queued,
        );
        assert_eq!(dispatchable(&nodes, now(), 1), vec!["profile"]);
        assert!(dispatchable(&nodes, now(), 0).is_empty());
    }

    #[test]
    fn a_node_already_running_under_a_live_lease_counts_against_the_limit() {
        let mut nodes = chain(
            NodeStatus::Succeeded,
            NodeStatus::Running,
            NodeStatus::Queued,
        );
        nodes[1].lease_expires_at = Some(now() + chrono::Duration::seconds(30));
        assert!(dispatchable(&nodes, now(), 1).is_empty());
        assert_eq!(dispatchable(&nodes, now(), 2), vec!["filing"]);
    }

    #[test]
    fn a_lapsed_lease_makes_a_running_node_claimable_again() {
        // The whole recovery story for a worker that died mid-node.
        let mut nodes = chain(
            NodeStatus::Succeeded,
            NodeStatus::Running,
            NodeStatus::Queued,
        );
        nodes[1].lease_expires_at = Some(now() - chrono::Duration::seconds(1));
        assert_eq!(dispatchable(&nodes, now(), 3), vec!["profile", "filing"]);
    }

    #[test]
    fn two_workers_never_see_the_same_leased_node() {
        // Both call this at the same moment; the lease is live, so neither is
        // offered it. The database's conditional claim is the second guard;
        // this is the first.
        let mut nodes = chain(
            NodeStatus::Succeeded,
            NodeStatus::Running,
            NodeStatus::Skipped,
        );
        nodes[1].lease_expires_at = Some(now() + chrono::Duration::seconds(30));
        assert!(!dispatchable(&nodes, now(), 3).contains(&"profile".to_string()));
        assert!(!dispatchable(&nodes, now(), 3).contains(&"profile".to_string()));
    }

    #[test]
    fn a_node_out_of_attempts_is_not_dispatched_again() {
        let mut nodes = chain(NodeStatus::Queued, NodeStatus::Queued, NodeStatus::Queued);
        nodes[0].attempts = 3;
        nodes[0].max_attempts = 3;
        assert!(dispatchable(&nodes, now(), 3).is_empty());
    }

    // --- why something is waiting ------------------------------------------

    #[test]
    fn the_scheduler_can_say_what_each_node_is_waiting_on() {
        let nodes = chain(NodeStatus::Running, NodeStatus::Queued, NodeStatus::Queued);
        let reasons = explain(&nodes, now(), 3);
        assert_eq!(reasons.len(), 3);
        assert_eq!(
            reasons
                .iter()
                .find(|d| d.key == "profile")
                .unwrap()
                .withheld,
            Some(Withheld::WaitingOnDependency)
        );
    }

    #[test]
    fn a_dead_branch_says_so_rather_than_saying_waiting() {
        let nodes = chain(NodeStatus::Failed, NodeStatus::Queued, NodeStatus::Queued);
        let reasons = explain(&nodes, now(), 3);
        assert!(reasons
            .iter()
            .filter(|d| d.key != "search")
            .all(|d| d.withheld == Some(Withheld::BranchFailed)));
    }

    #[test]
    fn hitting_the_limit_is_reported_as_the_limit_and_not_as_waiting() {
        let nodes = chain(
            NodeStatus::Succeeded,
            NodeStatus::Queued,
            NodeStatus::Queued,
        );
        let reasons = explain(&nodes, now(), 1);
        let withheld: Vec<Option<Withheld>> = reasons.iter().map(|d| d.withheld).collect();
        assert_eq!(withheld, vec![None, Some(Withheld::AtConcurrencyLimit)]);
    }

    // --- blocking -----------------------------------------------------------

    #[test]
    fn a_failed_essential_node_blocks_its_branch_and_nothing_else() {
        //        search
        //        /    \
        //   profile   filing        and an independent address lookup
        let mut nodes = chain(NodeStatus::Failed, NodeStatus::Queued, NodeStatus::Queued);
        nodes.push(node("address", NodeStatus::Queued, &[]));

        let blocked = newly_blocked(&nodes);
        assert_eq!(blocked, vec!["profile", "filing"]);
        assert!(!blocked.contains(&"address".to_string()));
        // The independent branch still runs.
        assert_eq!(dispatchable(&nodes, now(), 3), vec!["address"]);
    }

    #[test]
    fn blocking_does_not_reach_past_a_node_that_is_still_alive() {
        let nodes = chain(NodeStatus::Running, NodeStatus::Queued, NodeStatus::Queued);
        assert!(newly_blocked(&nodes).is_empty());
    }

    #[test]
    fn a_node_whose_dependency_does_not_exist_is_blocked_not_run() {
        // A DAG that lost a node is a DAG nobody approved. Running the orphan
        // would bind it to a fact that was never produced.
        let nodes = vec![node("profile", NodeStatus::Queued, &["missing"])];
        assert!(dispatchable(&nodes, now(), 3).is_empty());
        assert_eq!(newly_blocked(&nodes), vec!["profile"]);
    }

    // --- the job's own status ----------------------------------------------

    #[test]
    fn all_required_outputs_proven_is_the_only_way_to_succeed() {
        let nodes = chain(
            NodeStatus::Succeeded,
            NodeStatus::Succeeded,
            NodeStatus::Succeeded,
        );
        assert_eq!(job_status(&nodes, false), JobStatus::Succeeded);
    }

    #[test]
    fn one_proven_output_out_of_two_is_partial_and_never_succeeded() {
        // The exact failure the plan calls out: one successful run must not
        // mark every step answered.
        let nodes = chain(
            NodeStatus::Succeeded,
            NodeStatus::Succeeded,
            NodeStatus::Failed,
        );
        assert_eq!(job_status(&nodes, false), JobStatus::Partial);
    }

    #[test]
    fn a_blocked_branch_counts_as_unproven_not_as_absent() {
        let nodes = chain(
            NodeStatus::Succeeded,
            NodeStatus::Succeeded,
            NodeStatus::Blocked,
        );
        assert_eq!(job_status(&nodes, false), JobStatus::Partial);
    }

    #[test]
    fn nothing_proven_is_failed() {
        let nodes = chain(NodeStatus::Failed, NodeStatus::Blocked, NodeStatus::Blocked);
        assert_eq!(job_status(&nodes, false), JobStatus::Failed);
    }

    #[test]
    fn an_optional_branch_that_failed_does_not_make_the_job_partial() {
        let mut nodes = chain(
            NodeStatus::Succeeded,
            NodeStatus::Succeeded,
            NodeStatus::Succeeded,
        );
        let mut optional = node("building", NodeStatus::Failed, &[]);
        optional.required = false;
        nodes.push(optional);
        assert_eq!(job_status(&nodes, false), JobStatus::Succeeded);
    }

    #[test]
    fn asking_beats_running_in_the_reported_status() {
        // A job waiting on a person is not making progress, and showing it as
        // running invites everyone to wait for nothing.
        let mut nodes = chain(
            NodeStatus::NeedsInput,
            NodeStatus::Queued,
            NodeStatus::Queued,
        );
        nodes.push(node("address", NodeStatus::Running, &[]));
        assert_eq!(job_status(&nodes, false), JobStatus::NeedsInput);
    }

    #[test]
    fn cancellation_wins_over_everything() {
        let nodes = chain(
            NodeStatus::Succeeded,
            NodeStatus::Running,
            NodeStatus::Queued,
        );
        assert_eq!(job_status(&nodes, true), JobStatus::Cancelled);
    }

    #[test]
    fn a_job_with_work_left_is_queued_not_finished() {
        let nodes = chain(
            NodeStatus::Succeeded,
            NodeStatus::Queued,
            NodeStatus::Queued,
        );
        assert_eq!(job_status(&nodes, false), JobStatus::Queued);
    }
}
