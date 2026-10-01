use super::*;
use crate::parser::{Goal, Subject};
use std::collections::BTreeMap;

impl Engine {
    /// Price the exact registered endpoint selected in Ask v2. The client can
    /// supply its declared canonical inputs or a supported native lookup seed; directory descriptions
    /// and model capabilities never become execution authority. No model,
    /// authorization, provider request, or balance reservation happens here.
    pub async fn prepare_endpoint(
        &self,
        p: &Principal,
        request_id: Uuid,
        endpoint_id: Uuid,
        capability: Option<&str>,
        input: &BTreeMap<String, Value>,
    ) -> anyhow::Result<Response> {
        if let Some(response) = self.request_claim(
            p, request_id, &digest(&("ask-v2-prepare", p.connection, endpoint_id, capability, input)), "endpoint-prepare",
        ).await? {
            return Ok(response);
        }
        let result = async {
            let catalog = self.catalog.get().await?;
            let service = match Self::endpoint_service(&catalog, endpoint_id, capability) {
                Ok(service) => service,
                Err((code, message)) => return Ok(Response::error(request_id, Status::NeedsInput, code, message)),
            };
            let intent = match endpoint_intent(&catalog, service, input) {
                Ok(intent) => intent,
                Err(message) => return Ok(Response::error(request_id, Status::NeedsInput, "invalid_endpoint_input", &message)),
            };
            let request = DiscoverRequest {
                request_id,
                question: format!("Run {} on {}", service.contract.capability_slug, service.api_slug),
                state: None,
                context_delta: ContextDelta::default(),
            };
            self.intake(p, &request, false, Some((intent, None))).await
        }.await;
        Ok(self.complete(p, request_id, result).await)
    }

    pub(crate) fn endpoint_service<'a>(catalog: &'a Catalog, endpoint_id: Uuid, capability: Option<&str>) -> Result<&'a crate::catalog::Service, (&'static str, &'static str)> {
        let mut matching = catalog.services.iter().filter(|s| s.endpoint_id == endpoint_id
            && capability.is_none_or(|capability| s.contract.capability_slug == capability));
        let first = matching.next().ok_or(("endpoint_unavailable", "This endpoint is no longer available. Choose a source again; nothing was charged."))?;
        if matching.any(|s| s.contract.capability_slug != first.contract.capability_slug) {
            return Err(("ambiguous_endpoint", "This endpoint has several operations. Choose its specific capability before preparing a quote."));
        }
        Ok(first)
    }

    /// Literal names and addresses can seed a native, validated dependency
    /// chain. Geographic links may be absent (e.g. a unit in multiple buildings);
    /// the planner prices the lookup, and execution still requires its evidence.
    pub(crate) fn endpoint_lookup(catalog: &Catalog, service: &crate::catalog::Service) -> Option<(&'static str, Vec<String>)> {
        [("company.name", false), ("geo.input.q", true)].into_iter().find_map(|(seed, conditional)| {
            let fields = Self::endpoint_lookup_fields(catalog, service, seed, conditional);
            (!fields.is_empty()).then_some((seed, fields))
        })
    }

    fn endpoint_lookup_fields(catalog: &Catalog, service: &crate::catalog::Service, seed: &str, conditional: bool) -> Vec<String> {
        if !catalog.vocabulary.contains_key(seed) { return vec![]; }
        let native = |candidate: &&crate::catalog::Service| candidate.api_slug == service.api_slug
            && candidate.endpoint_id != service.endpoint_id && !candidate.unverified;
        let mut known = std::collections::BTreeSet::from([seed.to_owned()]);
        for _ in 0..crate::protocol::MAX_DEPTH {
            let before = known.len();
            let available = known.clone();
            for candidate in catalog.services.iter().filter(native) {
                if candidate.contract.requires.iter().filter(|r| !r.optional).all(|r| available.contains(&r.fact_type)) {
                    known.extend(candidate.contract.output_map.produces.keys()
                        .filter(|field| !candidate.optional_outputs.contains(*field) || (conditional
                            // Match the resolver: a conditional output cannot
                            // displace a native guaranteed producer of that fact.
                            && !catalog.services.iter().filter(native).any(|producer|
                                producer.contract.output_map.produces.contains_key(*field)
                                && !producer.optional_outputs.contains(*field))))
                        .cloned());
                }
            }
            if known.len() == before { break; }
        }
        service.contract.requires.iter().filter(|r| !r.optional && r.fact_type != seed && known.contains(&r.fact_type))
            .map(|r| r.fact_type.clone()).collect()
    }

    /// A structured European request is a fixed multi-goal intent. It enters
    /// the ordinary planner and saved quote flow; no provider is called here.
    pub async fn structured_capability(
        &self,
        p: &Principal,
        request_id: Uuid,
        capability: &str,
        input: &BTreeMap<String, Value>,
        operations: &[(String, String)],
    ) -> anyhow::Result<Response> {
        if let Some(r) = self.request_claim(p, request_id,
            &digest(&("structured-capability", p.connection, capability, input, operations)), "capability").await? {
            return Ok(r);
        }
        let label = input.get("company.name").or_else(|| input.get("company.nl.registered_name"))
            .or_else(|| input.get("eurostat.input.dataset"))
            .and_then(Value::as_str).unwrap_or(capability).chars().take(120).collect();
        let intent = Intent {
            subjects: vec![Subject { entity_ref: None, label, values: input.clone() }],
            goals: operations.iter().map(|(source, operation)| Goal {
                capability: operation.clone(), subject: 0,
                constraints: BTreeMap::from([("source".into(), json!(source))]),
            }).collect(),
            answer_operations: vec![format!("Answer {capability} from the saved source results. Merge records only when stable identifiers match, deduplicate repeated records, and preserve each source's retrieval time, source URL and provenance. Mark unavailable fields as unknown; do not infer an unverified match.")],
            unresolved_references: vec![],
        };
        let request = DiscoverRequest { request_id,
            question: format!("{capability} structured request"), state: None,
            context_delta: ContextDelta::default() };
        let result = self.intake(p, &request, false, Some((intent, None))).await;
        Ok(self.complete(p, request_id, result).await)
    }

    /// A direct source call: the caller names the source, the operation and
    /// the inputs, so no model interprets a question. It is a one-goal intent
    /// pinned to that source, priced by the same planner and run by the same
    /// executor and ledger as an Ask — not a second execution path.
    pub async fn direct(
        &self,
        p: &Principal,
        request_id: Uuid,
        source: &str,
        operation: &str,
        input: &BTreeMap<String, Value>,
    ) -> anyhow::Result<Response> {
        if let Some(r) = self
            .request_claim(p, request_id, &digest(&("direct", p.connection, source, operation, input)), "direct")
            .await?
        {
            return Ok(r);
        }
        let label: String = input
            .values()
            .find_map(Value::as_str)
            .unwrap_or(source)
            .chars()
            .take(120)
            .collect();
        let intent = Intent {
            subjects: vec![Subject { entity_ref: None, label, values: input.clone() }],
            goals: vec![Goal {
                capability: operation.to_owned(),
                subject: 0,
                constraints: BTreeMap::from([("source".to_owned(), json!(source))]),
            }],
            answer_operations: vec![],
            unresolved_references: vec![],
        };
        let req = DiscoverRequest {
            request_id,
            question: format!("Run {operation} on {source}"),
            state: None,
            context_delta: ContextDelta::default(),
        };
        let result = self.intake(p, &req, false, Some((intent, None))).await;
        Ok(self.complete(p, request_id, result).await)
    }
}

fn endpoint_intent(catalog: &Catalog, service: &crate::catalog::Service, input: &BTreeMap<String, Value>) -> Result<Intent, String> {
    let lookup = Engine::endpoint_lookup(catalog, service);
    for (field, value) in input {
        if !service.contract.requires.iter().any(|r| r.fact_type == *field)
            && !lookup.as_ref().is_some_and(|(seed, _)| field == seed) {
            return Err(format!("`{field}` is not an input of this endpoint. Review its input fields; nothing was charged."));
        }
        let fact = catalog.vocabulary.get(field)
            .ok_or_else(|| "This endpoint's input schema is unavailable. Choose a source again.".to_owned())?;
        if catalog_core::facts::schema::validate(&fact.value_schema, value).is_err() {
            return Err(format!("Check the value of `{field}` against its input format. Nothing was charged."));
        }
    }
    let label = input.get("company.name").and_then(Value::as_str).or_else(|| input.values().find_map(Value::as_str)).unwrap_or(&service.name).chars().take(120).collect();
    Ok(Intent {
        subjects: vec![Subject { entity_ref: None, label, values: input.clone() }],
        goals: vec![Goal {
            capability: service.contract.capability_slug.clone(),
            subject: 0,
            constraints: BTreeMap::from([
                ("source".into(), json!(service.api_slug)),
                ("endpoint_id".into(), json!(service.endpoint_id)),
            ]),
        }],
        answer_operations: vec![],
        unresolved_references: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/integration-catalog.json")).unwrap())
    }

    #[test]
    fn selected_endpoint_stays_pinned_even_when_a_cheaper_variant_exists() {
        let mut catalog = catalog();
        let selected = catalog.services.iter().find(|s| s.contract.capability_slug == "company.accounts").unwrap().clone();
        let mut cheaper = selected.clone();
        cheaper.id = Uuid::new_v4();
        cheaper.endpoint_id = Uuid::new_v4();
        cheaper.list_atomic = 0;
        catalog = Catalog::build(crate::catalog::Snapshot { services: vec![cheaper, selected.clone()], vocabulary: catalog.vocabulary.values().cloned().collect(), known_sources: vec![] });
        let mut intent = endpoint_intent(&catalog, &selected, &BTreeMap::from([("company.id".into(), json!("123"))])).unwrap();
        let mut task = Task::default();
        input::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let Resolution::Plan(plan) = resolver::resolve(&catalog, &task, &intent) else { panic!("expected a quote") };
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].service.endpoint_id, selected.endpoint_id);
        assert_eq!(plan.cap_atomic, selected.total());
        assert!(intent.answer_operations.is_empty());
    }

    #[test]
    fn endpoint_inputs_are_exact_schema_checked_and_never_inferred() {
        let catalog = catalog();
        let service = catalog.services.iter().find(|s| s.contract.capability_slug == "company.accounts").unwrap();
        for input in [BTreeMap::from([("unrelated.name".into(), json!("Example"))]), BTreeMap::from([("company.id".into(), json!(123))])] {
            assert!(endpoint_intent(&catalog, service, &input).is_err());
        }
        let mut intent = endpoint_intent(&catalog, service, &BTreeMap::new()).unwrap();
        assert!(intent.subjects[0].values.is_empty());
        let mut task = Task::default();
        input::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        assert!(matches!(resolver::resolve(&catalog, &task, &intent), Resolution::Missing { .. }));
    }

    #[test]
    fn native_name_lookup_is_priced_with_the_pinned_endpoint() {
        let catalog = catalog();
        let service = catalog.services.iter().find(|s| s.contract.capability_slug == "company.accounts").unwrap();
        assert_eq!(Engine::endpoint_lookup(&catalog, service), Some(("company.name", vec!["company.id".into()])));
        let mut intent = endpoint_intent(&catalog, service, &BTreeMap::from([("company.name".into(), json!("Example Ltd"))])).unwrap();
        let mut task = Task::default();
        input::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let Resolution::Plan(plan) = resolver::resolve(&catalog, &task, &intent) else { panic!("expected lookup and accounts") };
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].service.contract.capability_slug, "company.search");
        assert_eq!(plan.steps[1].service.endpoint_id, service.endpoint_id);
        assert_eq!(plan.cap_atomic, 138);
    }

    #[test]
    fn lookup_alternatives_require_guaranteed_native_outputs() {
        for mode in ["foreign", "unverified", "optional", "cycle"] {
            let mut catalog = catalog();
            let selected = catalog.services.iter().find(|s| s.contract.capability_slug == "company.accounts").unwrap().clone();
            for service in catalog.services.iter_mut().filter(|s| s.endpoint_id != selected.endpoint_id) {
                match mode {
                    "foreign" => service.api_slug = "other-provider".into(),
                    "unverified" => service.unverified = true,
                    "optional" => { service.optional_outputs.push("company.id".into()); },
                    "cycle" => for required in &mut service.contract.requires { required.fact_type = "company.id".into(); },
                    _ => unreachable!(),
                }
            }
            assert!(Engine::endpoint_lookup(&catalog, &selected).is_none(), "{mode}");
            assert!(endpoint_intent(&catalog, &selected, &BTreeMap::from([("company.name".into(), json!("Example"))])).is_err(), "{mode}");
        }
    }

    #[test]
    fn shared_endpoint_requires_its_explicit_capability() {
        let mut catalog = catalog();
        let original = catalog.services[0].clone();
        let mut other = original.clone();
        other.id = Uuid::new_v4();
        other.contract.capability_slug = "company.alternative".into();
        catalog.services.push(other);
        assert_eq!(Engine::endpoint_service(&catalog, original.endpoint_id, None).unwrap_err().0, "ambiguous_endpoint");
        assert_eq!(Engine::endpoint_service(&catalog, original.endpoint_id, Some("company.alternative")).unwrap().contract.capability_slug, "company.alternative");
        assert_eq!(Engine::endpoint_service(&catalog, original.endpoint_id, Some("unpublished.operation")).unwrap_err().0, "endpoint_unavailable");
    }

    #[test]
    fn address_prices_native_building_and_neighbourhood_dependencies_without_inventing_ids() {
        let catalog = crate::ask_v2::test_catalog::locations();
        for (operation, expected) in [
            ("building.bag-unit", vec!["location.property-search", "building.bag-unit"]),
            ("building.bag-building", vec!["location.property-search", "building.bag-unit", "building.bag-building"]),
            ("area.cbs-neighbourhood-2025", vec!["location.property-search", "location.bag-address-context", "area.cbs-neighbourhood-2025"]),
        ] {
            let service = catalog.services.iter().find(|s| s.contract.capability_slug == operation).unwrap();
            let (seed, resolves) = Engine::endpoint_lookup(&catalog, service).expect(operation);
            assert_eq!(seed, "geo.input.q");
            assert_eq!(resolves.len(), 1);
            let supplied = BTreeMap::from([("geo.input.q".into(), json!("Damrak 1, Amsterdam"))]);
            let mut intent = endpoint_intent(&catalog, service, &supplied).unwrap();
            assert_eq!(intent.subjects[0].values, supplied);
            let mut task = Task::default();
            input::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
            let Resolution::Plan(plan) = resolver::resolve(&catalog, &task, &intent) else { panic!("expected address lookup and {operation}") };
            assert_eq!(plan.steps.iter().map(|s| s.service.contract.capability_slug.as_str()).collect::<Vec<_>>(), expected);
            assert_eq!(plan.steps.last().unwrap().service.endpoint_id, service.endpoint_id);
            assert_eq!(plan.cap_atomic, plan.steps.iter().map(|s| s.service.total()).sum::<i64>());
            assert!(task.facts.iter().all(|fact| fact.semantic_type == "geo.input.q"), "no BAG ID, building link or neighbourhood code is inferred");
        }
    }

    #[test]
    fn address_lookup_requires_a_complete_verified_native_chain() {
        for mode in ["missing", "foreign", "unverified", "cycle"] {
            let mut catalog = crate::ask_v2::test_catalog::locations();
            let selected = catalog.services.iter().find(|s| s.contract.capability_slug == "area.cbs-neighbourhood-2025").unwrap().clone();
            let geocoder = catalog.services.iter_mut().find(|s| s.contract.capability_slug == "location.property-search").unwrap();
            match mode {
                "missing" => geocoder.contract.output_map.produces.clear(),
                "foreign" => geocoder.api_slug = "other-provider".into(),
                "unverified" => geocoder.unverified = true,
                "cycle" => geocoder.contract.requires[0].fact_type = "location.neighbourhood_code".into(),
                _ => unreachable!(),
            }
            assert!(Engine::endpoint_lookup(&catalog, &selected).is_none(), "{mode}");
            assert!(endpoint_intent(&catalog, &selected, &BTreeMap::from([("geo.input.q".into(), json!("Damrak 1, Amsterdam"))])).is_err(), "{mode}");
        }
    }
}
