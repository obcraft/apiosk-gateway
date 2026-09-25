//! The fast path of #23: keep a quote's steps when nothing about them
//! changed, so a refresh needs no model call and no re-planning. The
//! resolver itself does not bind literal inputs at quote time (a dependent
//! step's input is only produced once an earlier step runs), so this checks
//! catalog identity, not execution readiness: whether the step still exists
//! with the exact contract, price and endpoint it was quoted with. A missing
//! or genuinely expired input still surfaces normally, at dispatch, exactly
//! as it does for a quote that was never refreshed.
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
    refreshed.cap_atomic = refreshed.steps.iter().map(|s| s.service.total_with(refreshed.fee_bps)).sum();
    Some(refreshed)
}
