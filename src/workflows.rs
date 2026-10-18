//! Workflows: ready-made flows that run inside the Ask chat (and connected
//! chatbots). A run is a normal gateway task, so price, approval, execution and
//! results use the same guarded path as any other request.
use crate::{invoice_document::FIELDS, supplier_check};
use serde_json::{json, Value};

/// Temporary pause of the named Workflows overview, independent of recurring
/// orders. Individual slugs are released through `enabled`.
pub const ENABLED: bool = false;
pub const DISABLED_MESSAGE: &str = "Workflows are temporarily disabled.";

/// The one workflow that runs while the feature is paused: the Pre-KYB check
/// (`docs/pre-kyb-check.md`). The App mirrors this with `isWorkflowEnabled`.
pub fn enabled(slug: &str) -> bool {
    ENABLED || slug == crate::pre_kyb::SLUG
}

pub fn start_rejection(request_id: uuid::Uuid, slug: &str) -> Option<crate::protocol::Response> {
    (!enabled(slug)).then(|| crate::protocol::Response::error(request_id, crate::protocol::Status::Unsupported, "workflows_disabled", DISABLED_MESSAGE))
}

/// Only the enabled definitions of a `{"workflows": [...]}` listing.
pub fn enabled_only(definitions: Vec<Value>) -> Vec<Value> {
    definitions.into_iter().filter(|w| w["slug"].as_str().is_some_and(enabled)).collect()
}

pub const SUPPLIER_CHECK: &str = "supplier-payment-check";

/// The quoted price range of a supplier check at the workflow markup: a minimal
/// invoice (KVK number, name, IBAN) and a complete one (name search, VAT,
/// website). Converted with the account's display rate; charges stay in USD.
pub fn supplier_price_range(catalog: &crate::catalog::Catalog, display: &Value) -> Option<(String, String, String)> {
    let markup = catalog_core::fees::WORKFLOW_MARKUP_BPS as i64;
    let price = |capability: &str, source: &str| catalog.by_capability.get(capability)
        .and_then(|ids| ids.iter().map(|i| &catalog.services[*i]).filter(|s| s.api_slug == source).map(|s| s.total_with(markup)).min());
    let iban = price("iban.validate", "iban-check").or_else(|| price("company.sirenic-iban-verifier-iban", "sirenic")).unwrap_or(0);
    let profile = price("company.registry-profile", "kvk-dutch-business-register")?;
    let sanctions = price("company.sanctions-screen", "opensanctions").unwrap_or(0);
    let min = profile + sanctions + iban;
    let max = min + price("company_registry.search", "kvk-dutch-business-register").unwrap_or(0) + price("vat.eu.validate", "vies").unwrap_or(0)
        + price(crate::supplier_check::DOMAIN_REGISTRATION, "domain-intelligence").unwrap_or(0) + price(crate::supplier_check::DOMAIN_MAIL_DNS, "domain-intelligence").unwrap_or(0);
    let currency = display["currency"].as_str().unwrap_or("USD").to_owned();
    let rate: f64 = display["rate"].as_str().and_then(|r| r.parse().ok()).filter(|r: &f64| *r > 0.0).unwrap_or(1.0);
    // An indicative range to the cent; the exact maximum is quoted before approval.
    let amount = |atomic: i64| format!("{:.2}", ((atomic as f64 / 1e6) * rate * 100.0).round() / 100.0);
    Some((currency, amount(min), amount(max)))
}

pub fn definitions(catalog: &crate::catalog::Catalog, display: &Value) -> Value {
    definitions_for_client(catalog, display, true)
}

/// Existing browser bundles only understand invoice inputs and numeric price
/// ranges. The new App opts in to field forms while older tabs keep working.
pub fn definitions_for_client(catalog: &crate::catalog::Catalog, display: &Value, forms: bool) -> Value {
    let label = |key: &str| match key {
        "supplier_name" => "Supplier name", "kvk_number" => "KVK number", "company_number_uk" => "UK company number", "vat_number" => "VAT number",
        "iban" => "IBAN", "website" => "Website", "email" => "E-mail", "street" => "Street and number", "postcode" => "Postcode", "city" => "City",
        "country" => "Country", "invoice_number" => "Invoice number", "invoice_date" => "Invoice date", "total_amount" => "Total amount", _ => "Currency",
    };
    let checks = [("company_exists", "Company exists"), ("registry_name_match", "Name matches the register"), ("name_vat_match", "VAT number belongs to this company"),
        ("address_match", "Invoice address matches registration"), ("iban_valid", "IBAN is valid"), ("account_holder", "Account holder matches"),
        ("recent_changes", "Not recently registered or changed"), ("domain_age", "Domain looks established"), ("sanctions_pep", "No sanctions or PEP hit"),
        ("invoice_history", "Consistent with earlier invoices")];
    let bank_source = if catalog.services.iter().any(|s| s.api_slug=="iban-check" && s.contract.capability_slug=="iban.validate") { "iban-check" } else { "sirenic" };
    let sources: Vec<Value> = [("kvk-dutch-business-register", "KVK"), ("companies-house", "Companies House"), ("vies", "VIES"), ("opensanctions", "OpenSanctions"), ("domain-intelligence", "RDAP & DNS"), (bank_source, "IBAN check")]
        .iter().filter(|(slug, _)| catalog.services.iter().any(|s| s.api_slug == *slug)).map(|(slug, name)| json!({"slug":slug,"name":name})).collect();
    let group = |slugs: &[&str]| sources.iter().filter(|source| slugs.contains(&source["slug"].as_str().unwrap_or_default())).cloned().collect::<Vec<_>>();
    let steps = json!([
        {"id":"invoice","label":"Read the invoice details","sources":[],"condition":"From your uploaded invoice"},
        {"id":"register","label":"Check company identity and registration","sources":group(&["kvk-dutch-business-register","companies-house"]),"condition":"Use the register for the supplier's country; search only when its identifier is missing"},
        {"id":"vat","label":"Validate VAT and available identity details","sources":group(&["vies"]),"condition":"When a VAT number is provided"},
        {"id":"bank","label":"Validate the IBAN format and bank details","sources":group(&[bank_source]),"condition":"When an IBAN is provided; this does not establish account ownership"},
        {"id":"sanctions","label":"Screen the supplier for potential sanctions matches","sources":group(&["opensanctions"])},
        {"id":"domain","label":"Check domain registration and mail DNS","sources":group(&["domain-intelligence"]),"condition":"When a website or email domain is provided"},
        {"id":"history","label":"Compare with earlier invoices","sources":[],"condition":"Uses your saved supplier invoice history"}
    ]);
    let checks: Vec<Value> = if forms {
        ["Company identity and registration", "Invoice name and address consistency", "VAT details when provided", "IBAN validity and available bank evidence", "Potential sanctions matches", "Domain signals when provided", "Changes from earlier invoices"].iter().enumerate().map(|(i,label)|json!({"id":format!("check-{i}"),"label":label})).collect()
    } else {checks.iter().map(|(id,label)|json!({"id":id,"label":label})).collect()};
    let mut workflows = vec![];
    if supplier_check::available(catalog) {
        workflows.push(json!({
            "slug": SUPPLIER_CHECK, "name": "Supplier payment check", "name_nl": "Kan ik deze leverancier veilig betalen?", "category": "finance",
            "description": "Upload an invoice. Apiosk checks the business register, VAT number, sanctions, domain age, bank account and earlier invoices, and tells you whether to hold the payment.",
            "input": {"kind":"document","purpose":"invoice","accept":".pdf,application/pdf,image/png,image/jpeg","max_bytes":crate::invoice_document::MAX_FILE_BYTES},
            "fields": FIELDS.iter().map(|k| json!({"key":k,"label":label(k),"required":false})).collect::<Vec<_>>(),
            "checks": checks,
            "steps": steps,
            "sources": sources,
            "price": match supplier_price_range(catalog, display) {
                Some((currency, min, max)) => json!({"currency":currency,"min":min,"max":max,"fee_percent":20,"note":"Sum of the source calls this invoice needs, plus a 20% Apiosk workflow fee. The exact maximum is quoted before you approve; failed source calls are refunded."}),
                None => Value::Null,
            },
        }));
    }
    if forms { workflows.extend(crate::workflow_catalog::definitions(catalog)); }
    json!({"workflows": workflows})
}

pub fn run_status(status: &crate::protocol::Status, decision: Option<&str>) -> &'static str {
    use crate::protocol::Status::*;
    if decision.is_some() { return "completed" }
    match status {
        Ready | RequiresApproval => "quoted",
        NeedsInput => "needs_input",
        NeedsSelection => "needs_selection",
        Unsupported => "unsupported",
        Running => "running",
        Cancelled => "cancelled",
        Succeeded | Partial => "completed",
        Failed | StateConflict => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn price_range_uses_the_workflow_markup_and_display_rate() {
        let mut snapshot: crate::catalog::Snapshot = serde_json::from_str(include_str!("../tests/fixtures/supplier-check/catalog.json")).unwrap();
        let iban: crate::catalog::Snapshot = serde_json::from_str(include_str!("../tests/fixtures/iban-check/services.json")).unwrap();
        snapshot.services.extend(iban.services);
        snapshot.vocabulary.extend(iban.vocabulary);
        let catalog = crate::catalog::Catalog::build(snapshot);
        assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
        let (currency, min, max) = supplier_price_range(&catalog, &json!({"currency":"EUR","rate":"0.92"})).unwrap();
        assert_eq!(currency, "EUR");
        // KVK profile 0.066162 + OpenSanctions 0.094518 + IBAN 0.054348 list, +20%, x0.92.
        assert_eq!(min, "0.24");
        // + KVK search 0.018903 + VIES 0.085066 + RDAP 0.085066 + DNS 0.085066.
        assert_eq!(max, "0.54");
    }
}

#[cfg(test)]
mod pause_tests {
    #[test]
    fn paused_workflow_start_has_no_task_or_purchase_to_continue() {
        let request_id = uuid::Uuid::new_v4();
        let response = super::start_rejection(request_id, "customer-onboarding").expect("workflows are paused");
        assert_eq!(response.request_id, request_id);
        assert_eq!(response.status, crate::protocol::Status::Unsupported);
        assert_eq!(response.errors[0]["code"], "workflows_disabled");
        assert!(response.state.is_none());
        assert!(response.proposal.is_none());
        assert!(!super::enabled(super::SUPPLIER_CHECK));
    }
    #[test]
    fn the_pre_kyb_check_is_not_paused() {
        assert!(super::enabled("pre-kyb-screening"));
        assert!(super::start_rejection(uuid::Uuid::new_v4(), "pre-kyb-screening").is_none());
        let listed = super::enabled_only(vec![serde_json::json!({"slug":"pre-kyb-screening"}), serde_json::json!({"slug":"customer-onboarding"})]);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["slug"], "pre-kyb-screening");
    }
}
