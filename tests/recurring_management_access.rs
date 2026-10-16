//! Run in its own test process because rollout configuration is process-wide.
use apiosk_gateway_v2::recurring::{enabled_for, management_enabled_for};
use uuid::Uuid;

#[test]
fn production_owner_can_manage_during_execution_pause_without_granting_spending() {
    let owner = Uuid::new_v4();
    let other = Uuid::new_v4();
    std::env::set_var("BIND_ADDRESS", "0.0.0.0:8082");
    std::env::set_var("V2_EXECUTION_ENABLED", "1");
    std::env::set_var("RECURRING_ORDERS_ENABLED", "0");
    std::env::remove_var("RECURRING_WORKER_SECRET");
    std::env::remove_var("RECURRING_LOCAL_MANAGEMENT_ENABLED");
    std::env::set_var("RECURRING_ALLOWED_OWNER_IDS", owner.to_string());

    assert!(management_enabled_for(owner));
    assert!(!enabled_for(owner));
    assert!(!management_enabled_for(other));
    assert!(!enabled_for(other));

    std::env::set_var("RECURRING_ALLOWED_OWNER_IDS", format!("{owner},invalid"));
    assert!(!management_enabled_for(owner));
    std::env::remove_var("RECURRING_ALLOWED_OWNER_IDS");
    assert!(!management_enabled_for(owner));
}
