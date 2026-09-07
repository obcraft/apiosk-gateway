use apiosk_gateway_v2::catalog::{Catalog, Snapshot};

#[test]
#[ignore = "requires a read-only Supabase catalog snapshot via CATALOG_SNAPSHOT"]
fn live_snapshot_validates_when_supplied() {
    let path = std::env::var("CATALOG_SNAPSHOT").expect("CATALOG_SNAPSHOT is required");
    let snapshot: Snapshot = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let before = snapshot.services.len();
    let catalog = Catalog::build(snapshot);
    println!(
        "live catalog: {before} queried, {} accepted, {} quarantined",
        catalog.services.len(),
        catalog.rejected.len()
    );
    for (id, why) in &catalog.rejected {
        println!("quarantined {id}: {why}")
    }
    assert!(!catalog.services.is_empty());
    assert!(catalog
        .by_capability
        .contains_key("company.financial-statements"));
}
