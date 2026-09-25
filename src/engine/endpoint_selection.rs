//! The endpoints a person selected in the App (`context_delta.selection`,
//! feature `chat_selection_v1`). The literal question is never rewritten: the
//! model sees the selection next to it, and the gateway binds the validated
//! endpoint ids and entered values afterwards. A selection only narrows the
//! choice among already-reviewed catalog services. It carries no price, cap
//! or approval; prices still come from the resolver.
use crate::{
    catalog::Catalog,
    parser::{Intent, ModelSelection},
    protocol::{limits, KnownInput, ListingRef, Selection},
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

/// A selected endpoint after validation.
struct Endpoint {
    endpoint_id: Option<Uuid>,
    listing: Option<ListingRef>,
    /// Catalog indices of the reviewed services behind this endpoint. Empty
    /// when it has no planner contract: then it can never be planned.
    services: Vec<usize>,
    inputs: Vec<KnownInput>,
}

pub(crate) struct Selected {
    endpoints: Vec<Endpoint>,
}

/// Why a selection was refused. Nothing was interpreted, planned or priced.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct Invalid {
    message: String,
    endpoint_id: Option<Uuid>,
    fact_type: Option<String>,
}
impl Invalid {
    fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), endpoint_id: None, fact_type: None }
    }
    /// A workflow collects its own details; selected sources have no role there.
    pub fn workflow() -> Self {
        Self::new("Clear the selected sources to start or continue a workflow.")
    }
    fn at(mut self, endpoint: &Endpoint) -> Self {
        self.endpoint_id = endpoint.endpoint_id;
        self
    }
    fn input(mut self, input: &KnownInput) -> Self {
        self.fact_type = Some(input.fact_type.clone());
        self
    }
    pub fn error(&self) -> Value {
        let mut error = json!({"code":"invalid_selection","message":self.message});
        if let Some(endpoint) = self.endpoint_id { error["endpoint_id"] = json!(endpoint); }
        if let Some(fact_type) = &self.fact_type { error["fact_type"] = json!(fact_type); }
        error
    }
}

/// An entered value as the person typed it. Objects and arrays are not values
/// a person enters.
fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// A selection is an App chat feature; connected chatbots name sources in
/// the question, so `/v2/discover` refuses one and the MCP contract is unchanged.
pub(crate) fn accept(selection: &Selection, app_chat: bool, starts_workflow: bool, catalog: &Catalog) -> Result<Selected, Invalid> {
    if !app_chat {
        return Err(Invalid::new("Selecting endpoints is available in the Apiosk App. Name the source in the question instead."));
    }
    if starts_workflow {
        return Err(Invalid::workflow());
    }
    validate(selection, catalog)
}

fn validate(selection: &Selection, catalog: &Catalog) -> Result<Selected, Invalid> {
    let budget = &limits::limits().selection;
    if selection.version != 1 {
        return Err(Invalid::new("This selection format is not supported. Reload the page and select the sources again."));
    }
    if selection.endpoints.is_empty() || selection.endpoints.len() > budget.max_endpoints {
        return Err(Invalid::new(format!("Select between 1 and {} sources.", budget.max_endpoints)));
    }
    let mut endpoints: Vec<Endpoint> = Vec::new();
    let mut total = 0;
    for selected in &selection.endpoints {
        let endpoint = Endpoint {
            endpoint_id: selected.endpoint_id,
            listing: selected.listing.clone(),
            services: selected.endpoint_id.map(|id| (0..catalog.services.len()).filter(|index| catalog.services[*index].endpoint_id == id).collect()).unwrap_or_default(),
            inputs: selected.inputs.clone(),
        };
        if endpoint.endpoint_id.is_some() == endpoint.listing.is_some() {
            return Err(Invalid::new("Each selected source needs exactly one endpoint reference.").at(&endpoint));
        }
        if endpoint.listing.as_ref().is_some_and(|listing| [&listing.slug, &listing.method, &listing.path].iter().any(|part| part.is_empty() || limits::length(part) > 500)) {
            return Err(Invalid::new("A selected source has an invalid listing reference."));
        }
        if endpoints.iter().any(|known| (known.endpoint_id, &known.listing) == (endpoint.endpoint_id, &endpoint.listing)) {
            return Err(Invalid::new("A source was selected twice.").at(&endpoint));
        }
        if endpoint.inputs.len() > budget.max_inputs_per_endpoint {
            return Err(Invalid::new(format!("Enter at most {} values per source.", budget.max_inputs_per_endpoint)).at(&endpoint));
        }
        let required: BTreeSet<(&str, &str)> = endpoint.services.iter()
            .flat_map(|index| catalog.services[*index].contract.requires.iter().map(|r| (r.fact_type.as_str(), r.subject_role.as_str())))
            .collect();
        let mut seen = BTreeSet::new();
        for input in &endpoint.inputs {
            let key = (input.fact_type.as_str(), input.subject_role.as_str());
            if !required.contains(&key) || !seen.insert(key) {
                return Err(Invalid::new("This source does not take that value, or it was entered twice.").at(&endpoint).input(input));
            }
            let text = value_text(&input.value).filter(|text| !text.trim().is_empty())
                .ok_or_else(|| Invalid::new("Entered values must be text, a number or yes/no.").at(&endpoint).input(input))?;
            let length = limits::length(&text);
            if length > budget.max_value_chars {
                return Err(Invalid::new(format!("Each entered value can be at most {} characters.", budget.max_value_chars)).at(&endpoint).input(input));
            }
            total += length;
            let valid = catalog.vocabulary.get(&input.fact_type)
                .is_some_and(|fact| catalog_core::facts::schema::validate(&fact.value_schema, &input.value).is_ok());
            if !valid {
                return Err(Invalid::new("An entered value does not have the format this source needs.").at(&endpoint).input(input));
            }
        }
        endpoints.push(endpoint);
    }
    if total > budget.max_total_value_chars {
        return Err(Invalid::new(format!("The entered values can be at most {} characters together.", budget.max_total_value_chars)));
    }
    Ok(Selected { endpoints })
}

impl Selected {
    /// What the model reads next to the literal question.
    pub fn model_selection(&self, catalog: &Catalog) -> ModelSelection {
        let endpoints: Vec<Value> = self.endpoints.iter().map(|endpoint| {
            let capabilities: BTreeSet<&str> = endpoint.services.iter().map(|index| catalog.services[*index].contract.capability_slug.as_str()).collect();
            let inputs: Vec<Value> = endpoint.inputs.iter().map(|input| json!({"fact_type":input.fact_type,"subject_role":input.subject_role,"value":input.value})).collect();
            json!({"endpoint_id":endpoint.endpoint_id,"listing":endpoint.listing,
                "source":endpoint.services.first().map(|index| &catalog.services[*index].api_slug),
                "capabilities":capabilities,"planner_available":!endpoint.services.is_empty(),"inputs":inputs})
        }).collect();
        ModelSelection {
            view: json!({"mode":"restrict",
                "rule":"The person selected these endpoints for this question. Plan only goals whose capability a selected endpoint with planner_available=true offers. Its inputs are values the person entered for that endpoint; use them for the subject they describe. If no selected endpoint can serve the request, set discover=false and explain which part is not available.",
                "endpoints":endpoints}),
            literals: self.endpoints.iter().flat_map(|endpoint| endpoint.inputs.iter().filter_map(|input| value_text(&input.value))).collect(),
        }
    }

    /// Pin every goal to the selected endpoints that serve it and bind the
    /// entered values; an entered value replaces the model's. Returns the
    /// person-facing reason when part of the request lies outside the
    /// selection, so nothing is planned.
    pub fn bind(&self, intent: &mut Intent, catalog: &Catalog) -> Result<(), String> {
        let mut outside = BTreeSet::new();
        for goal in &mut intent.goals {
            let serving: BTreeSet<Uuid> = self.endpoints.iter()
                .filter(|endpoint| endpoint.services.iter().any(|index| catalog.services[*index].contract.capability_slug == goal.capability))
                .filter_map(|endpoint| endpoint.endpoint_id)
                .collect();
            match serving.len() {
                0 => { outside.insert(goal.capability.clone()); }
                1 => { goal.constraints.insert("endpoint_id".into(), json!(serving.first())); }
                _ => { goal.constraints.insert("endpoint_id".into(), json!(serving)); }
            }
        }
        if !outside.is_empty() {
            return Err(format!("The selected sources cannot answer this part of your question: {}. Select a source that offers it, or clear the selection so Apiosk can choose.", outside.into_iter().collect::<Vec<_>>().join(", ")));
        }
        let mut entered: BTreeMap<usize, BTreeMap<&str, &Value>> = BTreeMap::new();
        for endpoint in &self.endpoints {
            let subjects: BTreeSet<usize> = intent.goals.iter()
                .filter(|goal| endpoint.services.iter().any(|index| catalog.services[*index].contract.capability_slug == goal.capability))
                .map(|goal| goal.subject)
                .collect();
            // Values entered once describe one subject. When several subjects
            // use the endpoint, the model's own literal values stand.
            let [subject] = subjects.into_iter().collect::<Vec<_>>()[..] else { continue };
            for input in &endpoint.inputs {
                let values = entered.entry(subject).or_default();
                if values.insert(&input.fact_type, &input.value).is_some_and(|previous| previous != &input.value) {
                    return Err(format!("The selected sources were given different values for {}. Enter one value, or ask about one of them.", input.fact_type));
                }
            }
        }
        for (subject, values) in entered {
            for (fact_type, value) in values {
                intent.subjects[subject].values.insert(fact_type.to_owned(), value.clone());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SelectedEndpoint;

    fn catalog() -> Catalog {
        Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/integration-catalog.json")).unwrap())
    }
    fn endpoint(catalog: &Catalog, capability: &str) -> Uuid {
        catalog.services.iter().find(|s| s.contract.capability_slug == capability).unwrap().endpoint_id
    }
    fn selection(endpoints: Vec<SelectedEndpoint>) -> Selection {
        Selection { version: 1, mode: Default::default(), endpoints }
    }
    fn selected(id: Uuid, inputs: &[(&str, Value)]) -> SelectedEndpoint {
        SelectedEndpoint { endpoint_id: Some(id), listing: None, inputs: inputs.iter().map(|(fact_type, value)| KnownInput { fact_type: (*fact_type).into(), subject_role: "subject".into(), value: value.clone() }).collect() }
    }
    fn modelled(label: &str, capabilities: &[&str]) -> Intent {
        serde_json::from_value(json!({"subjects":[{"entity_ref":null,"label":label,"values":{"company.name":label}}],
            "goals":capabilities.iter().map(|capability| json!({"capability":capability,"subject":0,"constraints":{}})).collect::<Vec<_>>(),
            "answer_operations":[],"unresolved_references":[]})).unwrap()
    }

    #[test]
    fn refusals_name_the_endpoint_and_value() {
        let catalog = catalog();
        let accounts = endpoint(&catalog, "company.accounts");
        let refused = |selection: Selection| validate(&selection, &catalog).err().expect("refused").error();
        assert_eq!(refused(selection(vec![])).get("endpoint_id"), None);
        let wrong_type = refused(selection(vec![selected(accounts, &[("company.id", json!(12))])]));
        assert_eq!((&wrong_type["code"], &wrong_type["endpoint_id"], &wrong_type["fact_type"]), (&json!("invalid_selection"), &json!(accounts), &json!("company.id")));
        let not_taken = refused(selection(vec![selected(accounts, &[("company.name", json!("Example"))])]));
        assert_eq!(not_taken["fact_type"], "company.name", "accounts does not take a name");
        let twice = refused(selection(vec![selected(accounts, &[]), selected(accounts, &[])]));
        assert_eq!(twice["endpoint_id"], json!(accounts));
        let both = SelectedEndpoint { endpoint_id: Some(accounts), listing: Some(ListingRef { slug: "x".into(), method: "GET".into(), path: "/y".into() }), inputs: vec![] };
        assert_eq!(refused(selection(vec![both]))["endpoint_id"], json!(accounts));
        let empty = refused(selection(vec![selected(accounts, &[("company.id", json!("  "))])]));
        assert_eq!(empty["fact_type"], "company.id");
        let mut newer = selection(vec![selected(accounts, &[])]);
        newer.version = 2;
        assert!(validate(&newer, &catalog).is_err());
    }

    #[test]
    fn value_budgets_count_characters() {
        let catalog = catalog();
        let accounts = endpoint(&catalog, "company.accounts");
        let budget = &limits::limits().selection;
        let at_most = "é".repeat(budget.max_value_chars);
        assert!(validate(&selection(vec![selected(accounts, &[("company.id", json!(at_most))])]), &catalog).is_ok());
        let over = format!("{at_most}é");
        assert!(validate(&selection(vec![selected(accounts, &[("company.id", json!(over))])]), &catalog).is_err());
        let many: Vec<_> = (0..=budget.max_endpoints).map(|_| SelectedEndpoint { endpoint_id: Some(Uuid::new_v4()), listing: None, inputs: vec![] }).collect();
        assert!(validate(&selection(many), &catalog).is_err());
    }

    #[test]
    fn an_endpoint_without_a_contract_is_shown_but_never_planned() {
        let catalog = catalog();
        let unknown = Uuid::new_v4();
        let listing = SelectedEndpoint { endpoint_id: None, listing: Some(ListingRef { slug: "legacy".into(), method: "GET".into(), path: "/v1/y".into() }), inputs: vec![] };
        let chosen = validate(&selection(vec![selected(unknown, &[]), listing]), &catalog).unwrap();
        let view = chosen.model_selection(&catalog).view;
        assert!(view["endpoints"].as_array().unwrap().iter().all(|e| e["planner_available"] == false));
        let mut intent = modelled("Example", &["company.accounts"]);
        let refusal = chosen.bind(&mut intent, &catalog).unwrap_err();
        assert!(refusal.contains("company.accounts"), "{refusal}");
    }

    #[test]
    fn binding_pins_goals_and_entered_values_win() {
        let catalog = catalog();
        let accounts = endpoint(&catalog, "company.accounts");
        let chosen = validate(&selection(vec![selected(accounts, &[("company.id", json!("12345678"))])]), &catalog).unwrap();
        let view = chosen.model_selection(&catalog);
        assert_eq!(view.view["endpoints"][0]["capabilities"], json!(["company.accounts"]));
        assert_eq!(view.literals, ["12345678"]);
        let mut intent = modelled("Example", &["company.accounts"]);
        intent.subjects[0].values.insert("company.id".into(), json!("99"));
        chosen.bind(&mut intent, &catalog).unwrap();
        assert_eq!(intent.goals[0].constraints["endpoint_id"], json!(accounts));
        assert_eq!(intent.subjects[0].values["company.id"], "12345678");
        assert_eq!(intent.subjects[0].values["company.name"], "Example", "the model's other values stay");
    }

    #[test]
    fn values_for_an_endpoint_used_by_several_subjects_are_not_guessed() {
        let catalog = catalog();
        let accounts = endpoint(&catalog, "company.accounts");
        let chosen = validate(&selection(vec![selected(accounts, &[("company.id", json!("1"))])]), &catalog).unwrap();
        let mut intent: Intent = serde_json::from_value(json!({"subjects":[
            {"entity_ref":null,"label":"A","values":{"company.name":"A"}},{"entity_ref":null,"label":"B","values":{"company.name":"B"}}],
            "goals":[{"capability":"company.accounts","subject":0,"constraints":{}},{"capability":"company.accounts","subject":1,"constraints":{}}],
            "answer_operations":[],"unresolved_references":[]})).unwrap();
        chosen.bind(&mut intent, &catalog).unwrap();
        assert!(intent.subjects.iter().all(|subject| !subject.values.contains_key("company.id")));
        assert!(intent.goals.iter().all(|goal| goal.constraints["endpoint_id"] == json!(accounts)));
    }

    #[test]
    fn a_pinned_goal_plans_only_its_endpoint() {
        let catalog = catalog();
        let accounts = endpoint(&catalog, "company.accounts");
        let search = endpoint(&catalog, "company.search");
        let chosen = validate(&selection(vec![selected(search, &[])]), &catalog).unwrap();
        let mut intent = modelled("Example", &["company.search"]);
        chosen.bind(&mut intent, &catalog).unwrap();
        let mut task = crate::state::Task::default();
        crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(&catalog, &task, &intent) else { panic!("search is planned") };
        assert_eq!(plan.steps.iter().map(|s| s.service.endpoint_id).collect::<Vec<_>>(), [search]);
        // An endpoint id no service has can never be planned.
        intent.goals[0].constraints.insert("endpoint_id".into(), json!(accounts));
        assert!(matches!(crate::resolver::resolve(&catalog, &task, &intent), crate::resolver::Resolution::Unsupported(_)));
    }
}
