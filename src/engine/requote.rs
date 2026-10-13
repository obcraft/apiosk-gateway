//! The fast path of #23: keep a quote's steps when nothing about them
//! changed, so a refresh needs no model call and no re-planning. The
//! resolver itself does not bind literal inputs at quote time (a dependent
//! step's input is only produced once an earlier step runs), so this checks
//! catalog identity: whether the step still exists with the exact contract,
//! price and endpoint it was quoted with. The caller renews explicit inputs
//! from the saved intent on both paths; source evidence keeps its own TTL.
use crate::{catalog::Catalog, resolver::Plan};

/// `None` when any step's service disappeared or changed shape since the
/// quote. The caller then re-binds the stored intent and resolves fresh,
/// per the design's fallback.
pub(crate) fn same_steps(catalog: &Catalog, plan: &Plan) -> Option<Plan> {
    let unchanged = plan.steps.iter().all(|step| {
        catalog.services.iter().any(|current| current.id == step.service.id && current.version_hash() == step.service.version_hash())
    });
    if !unchanged {
        return None;
    }
    let mut refreshed = plan.clone();
    refreshed.cap_atomic = refreshed.steps.iter().map(|s| s.service.total_with(refreshed.fee_bps)).sum::<i64>() + refreshed.compute.as_ref().map_or(0, crate::compute::Quote::buyer_cap);
    Some(refreshed)
}
