//! Production catalog + real intent model for "Kan ik deze leverancier veilig
//! betalen?". Proves question -> preset -> concrete checks -> bound inputs ->
//! one price, for both the MCP parser and the Ask chat. Read-only: no provider
//! purchase, no task or customer state. Domain intelligence services are taken
//! from the staged fixture until that source is published.
use apiosk_gateway_v2::{
    catalog::{Catalog, Snapshot, CATALOG_SQL},
    parser::{Intent, Parser},
    resolver::{resolve, Resolution},
    state::{Fact, Task},
    supplier_check,
};
use serde_json::{json, Value};

fn fact(entity: uuid::Uuid, kind: &str, value: Value) -> Fact {
    Fact { entity_ref: entity, semantic_type: kind.into(), value, provenance: json!({"kind":"user_input"}), observed_at: chrono::Utc::now(), expires_at: chrono::Utc::now() + chrono::Duration::hours(1), verification: "user_asserted".into() }
}

fn plan(catalog: &Catalog, question: &str, mut intent: Intent, expected: &[&str]) -> anyhow::Result<()> {
    let mut task = Task::default();
    for subject in &mut intent.subjects {
        let id = uuid::Uuid::new_v4();
        subject.entity_ref = Some(id);
        for (k, v) in &subject.values { task.facts.push(fact(id, k, v.clone())) }
    }
    let expansion = supplier_check::expand(question, &mut intent, catalog).ok_or_else(|| anyhow::anyhow!("no supplier preset for {question:?}: {intent:?}"))?;
    for (k, v) in &expansion.facts {
        if !task.facts.iter().any(|f| f.semantic_type == *k && f.value == *v) { task.facts.push(fact(expansion.invoice.entity_ref, k, v.clone())) }
    }
    let Resolution::Plan(plan) = resolve(catalog, &task, &intent) else {
        anyhow::bail!("no executable plan for {question:?}: {:?}", resolve(catalog, &task, &intent))
    };
    let caps: Vec<String> = plan.steps.iter().map(|s| format!("{}:{}", s.service.api_slug, s.service.contract.capability_slug)).collect();
    for want in expected {
        anyhow::ensure!(caps.iter().any(|c| c.ends_with(want) || (*want == "company.sirenic-iban-verifier-iban" && c.ends_with("iban.validate"))), "{want} missing for {question:?}: {caps:?}");
    }
    for step in &plan.steps {
        let inputs = step.service.inputs(&task, step.entity_ref, &catalog.vocabulary).map(|v| v.to_string()).unwrap_or_else(|_| "(bound from an earlier step)".into());
        println!("   - {:<28} {:<42} inputs={inputs}", step.service.api_slug, step.service.contract.capability_slug);
    }
    println!("   invoice={}", serde_json::to_string(&expansion.invoice)?);
    println!("   max total: USD {:.4} (~EUR {:.2}) for {} calls\n", plan.cap_atomic as f64 / 1e6, plan.cap_atomic as f64 / 1e6 * 0.92, plan.steps.len());
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1).after_connect(|conn, _| Box::pin(async move {
        sqlx::query("SET search_path TO public, gateway_v2, kyb, providers, catalog, actions, billing, agents, wallets, research, integrations, observability, support, content, operations, identity, extensions").execute(&mut *conn).await?;
        sqlx::query("SET default_transaction_read_only = on").execute(&mut *conn).await?;
        Ok(())
    })).connect(&std::env::var("DATABASE_URL")?).await?;
    let value: Value = sqlx::query_scalar(CATALOG_SQL).fetch_one(&pool).await?;
    let mut snapshot: Snapshot = serde_json::from_value(value)?;
    let staged: Snapshot = serde_json::from_str(include_str!("../tests/fixtures/domain-intelligence/services.json"))?;
    if !snapshot.services.iter().any(|s| s.api_slug == "domain-intelligence") {
        snapshot.services.extend(staged.services);
        for v in staged.vocabulary { if !snapshot.vocabulary.iter().any(|x| x.slug == v.slug) { snapshot.vocabulary.push(v) } }
    }
    if let Ok(out) = std::env::var("SUPPLIER_FIXTURE_OUT") {
        // Real production contracts for the local durable-flow test.
        let wanted = [("kvk-dutch-business-register", "company.registry-profile"), ("kvk-dutch-business-register", "company_registry.search"), ("vies", "vat.eu.validate"), ("opensanctions", "company.sanctions-screen"), ("sirenic", "company.sirenic-iban-verifier-iban"), ("domain-intelligence", "domain.registration"), ("domain-intelligence", "domain.mail-dns")];
        let services: Vec<_> = snapshot.services.iter().filter(|s| wanted.contains(&(s.api_slug.as_str(), s.contract.capability_slug.as_str()))).cloned().collect();
        let facts: std::collections::BTreeSet<String> = services.iter().flat_map(|s| s.contract.requires.iter().map(|r| r.fact_type.clone()).chain(s.contract.output_map.produces.keys().cloned())).collect();
        let vocabulary: Vec<_> = snapshot.vocabulary.iter().filter(|v| facts.contains(&v.slug)).cloned().collect();
        std::fs::write(&out, serde_json::to_string_pretty(&json!({"services":services,"vocabulary":vocabulary}))?)?;
        println!("wrote {} services and {} fact types to {out}", services.len(), vocabulary.len());
        return Ok(());
    }
    let catalog = Catalog::build(snapshot);
    let domain_services = catalog.services.iter().filter(|s| s.api_slug == "domain-intelligence").count();
    anyhow::ensure!(domain_services == 2, "domain intelligence contracts rejected: {:?}", catalog.rejected);
    anyhow::ensure!(supplier_check::available(&catalog), "supplier preset unavailable: {:?}", catalog.rejected);
    let parser = Parser::new(reqwest::Client::new(), std::env::var("INTENT_API_URL")?, std::env::var("INTENT_API_KEY")?, std::env::var("INTENT_MODEL")?);
    let cases: [(&str, &[&str]); 4] = [
        ("Kan ik deze leverancier veilig betalen? Factuur INV-2026-0042 van Mollie B.V., KVK 30204462, btw-nummer NL815839091B01, IBAN NL91 ABNA 0417 1643 00, website mollie.com, adres Keizersgracht 126, 1015 CW Amsterdam.",
            &["company.registry-profile", "vat.eu.validate", "company.sanctions-screen", "domain.registration", "domain.mail-dns", "company.sirenic-iban-verifier-iban"]),
        ("Kan ik Coolblue B.V. veilig betalen? IBAN NL91ABNA0417164300, website coolblue.nl",
            &["company_registry.search", "company.registry-profile", "company.sanctions-screen", "domain.registration"]),
        ("Can I safely pay this supplier? Invoice 55-1 from TESCO PLC, company number 00445790, IBAN GB82 WEST 1234 5698 7654 32, website tesco.com, address Tesco House, Shire Park, AL7 1GA Welwyn Garden City.",
            &["companies-house:company.registry-profile", "company.officers", "company.filing-history", "company.sanctions-screen", "domain.registration"]),
        ("Klopt deze factuur van Bouwbedrijf De Vries BV? KvK-nummer 12345678, betalen naar IBAN NL02 ABNA 0123 4567 89, mail devriesbouw@gmail.com",
            &["company.registry-profile", "company.sanctions-screen"]),
    ];
    for (question, expected) in cases {
        println!("MCP parse: {question}");
        let intent = parser.parse(uuid::Uuid::new_v4(), question, &Task::default(), &catalog).await?;
        anyhow::ensure!(intent.goals.iter().any(|g| g.capability == supplier_check::PRESET), "parser did not choose the preset: {intent:?}");
        plan(&catalog, question, intent, expected)?;
        println!("Ask chat: {question}");
        let reply = parser.chat(question, &Task::default(), &catalog).await?;
        anyhow::ensure!(reply.discover, "chat did not discover: {}", reply.message);
        println!("   reply: {}", reply.message);
        plan(&catalog, question, reply.intent.ok_or_else(|| anyhow::anyhow!("no chat intent"))?, expected)?;
    }
    // Routing: varied invoice wording must reach the preset after model + code routing.
    let mut routed = 0;
    let phrasings = [
        "Controleer deze factuur van Bouwmaat Nederland B.V., KVK 34129463, IBAN NL91 ABNA 0417 1643 00",
        "Is dit een nepfactuur? Afzender Coolblue B.V., btw NL810433941B01, rekening NL91ABNA0417164300",
        "Onze leverancier Mollie B.V. (KVK 30204462) heeft een nieuw rekeningnummer NL02 ABNA 0123 4567 89, klopt dat?",
        "Our vendor TESCO PLC, company number 00445790, sent new bank details GB82 WEST 1234 5698 7654 32. Is this invoice real?",
        "check this invoice from Mollie B.V. KVK 30204462 before I pay it",
        "Mag ik deze betaling doen aan Bol.com B.V., KVK 32146913, website bol.com?",
    ];
    for question in phrasings {
        let mut via = vec![];
        let parsed = parser.parse(uuid::Uuid::new_v4(), question, &Task::default(), &catalog).await.ok();
        let mut mcp = parsed.clone().or_else(|| supplier_check::fallback_intent(question, &catalog));
        if let Some(intent) = mcp.as_mut() { supplier_check::route(question, intent, &catalog); }
        if mcp.as_ref().is_some_and(|i| i.goals.iter().any(|g| g.capability == supplier_check::PRESET)) { via.push("mcp") }
        let chat = parser.chat(question, &Task::default(), &catalog).await.ok();
        let mut ask = chat.as_ref().filter(|r| r.discover).and_then(|r| r.intent.clone()).or_else(|| supplier_check::fallback_intent(question, &catalog));
        if let Some(intent) = ask.as_mut() { supplier_check::route(question, intent, &catalog); }
        if ask.as_ref().is_some_and(|i| i.goals.iter().any(|g| g.capability == supplier_check::PRESET)) { via.push("ask") }
        println!("routing {:?}: model_preset(mcp)={} model_preset(ask)={} -> {via:?}", question,
            parsed.as_ref().is_some_and(|i| i.goals.iter().any(|g| g.capability == supplier_check::PRESET)),
            chat.as_ref().and_then(|r| r.intent.as_ref()).is_some_and(|i| i.goals.iter().any(|g| g.capability == supplier_check::PRESET)));
        if via.len() == 2 { routed += 1 }
    }
    anyhow::ensure!(routed == phrasings.len(), "{routed}/{} phrasings routed on both surfaces", phrasings.len());
    let vague = parser.chat("Kan ik deze leverancier veilig betalen?", &Task::default(), &catalog).await?;
    anyhow::ensure!(!vague.discover, "a question without supplier details must ask for the invoice: {:?}", vague.intent);
    println!("Ask chat without details -> {}", vague.message);
    Ok(())
}
