//! Turning validated facts into the request one node actually sends.
//!
//! A plan node knows which facts it needs and where each one goes on the wire —
//! `input_map` says `{"in": "path_append", "name": "kvkNummer"}` and the
//! contract says the fact is `company.registration.nl.kvk`. Binding is
//! substituting one into the other, and it is the last point at which a job
//! can still refuse for free.
//!
//! ```text
//!   facts from finished nodes ─┐
//!   values the caller supplied ─┴─▶ bind ─▶ a request, or a refusal
//! ```
//!
//! REFUSING IS THE POINT. A node whose required fact is missing must not be
//! sent with the field left out, and must not be sent with a company NAME in a
//! field documented as eight digits — that is how a paid call comes back
//! empty and everyone blames the provider. It is also how the current
//! `/v1/do` path ends up firing an unplanned paid resolver: a binding that
//! merely looks unready triggers a lookup nobody approved. On the job path
//! there is no such rescue. If it cannot be bound, it is not sent.
//!
//! The wire vocabulary is the one migration 075 already defined for
//! `capability_offers.input_map`, so a contract and an offer say the same
//! thing the same way.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::facts::contract::{ExecutionContract, InputBinding};

/// One value bound into the endpoint's path, with the parameter it was bound
/// to.
///
/// The NAME travels with the value because the path alone does not say where
/// the value belongs. This catalogue writes a path parameter two ways:
///
/// ```text
///   /api/v1/basisprofielen                  the value is APPENDED
///   /api/v1/hvds/jaarrekeningen/kvknummer   the last segment IS the parameter
/// ```
///
/// In the second, `kvknummer` is a placeholder standing where the value goes.
/// Only the name makes the two distinguishable. See
/// [`crate::job::worker::request_path`].
#[derive(Debug, Clone, PartialEq)]
pub struct PathValue {
    pub name: String,
    pub value: String,
}

/// A bound request, ready for the proxy.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WireRequest {
    /// Substituted into, or appended to, the endpoint's stored path, in order.
    pub path_segments: Vec<PathValue>,
    /// Query parameters, sorted so the same request always renders the same
    /// way — which is what makes the input hash stable across processes.
    pub query: Vec<(String, String)>,
    pub body: Map<String, Value>,
}

/// Why a node cannot be sent.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum BindError {
    /// A required fact is not available. The node is not sent, and no resolver
    /// is fired to go and find it: an unplanned paid lookup is exactly the
    /// cost the plan was supposed to have already disclosed.
    #[error("`{fact_type}` is required and not available; the call is not sent")]
    Missing { fact_type: String },
    #[error("`{fact_type}` is required but the contract does not say where it goes")]
    Unmapped { fact_type: String },
    #[error("`{fact_type}` cannot be written into `{location}`")]
    Unsupported { fact_type: String, location: String },
    /// A value that cannot be a path segment or a query parameter — an object
    /// where a scalar belongs.
    #[error("`{fact_type}` is not a value that can be sent as `{name}`")]
    NotScalar { fact_type: String, name: String },
}

/// Bind one node's inputs.
///
/// `facts` is keyed by fact type and holds only VALIDATED values: everything in
/// it has already passed its type's schema in `facts::contract`. Binding does
/// not re-validate, and must not be given anything that has not.
pub fn bind(
    contract: &ExecutionContract,
    facts: &BTreeMap<String, Value>,
) -> Result<WireRequest, BindError> {
    let mut request = WireRequest::default();

    for requirement in &contract.requires {
        let Some(value) = facts.get(&requirement.fact_type) else {
            if requirement.optional {
                continue;
            }
            return Err(BindError::Missing {
                fact_type: requirement.fact_type.clone(),
            });
        };
        let Some(binding) = contract.input_map.get(&requirement.fact_type) else {
            if requirement.optional {
                continue;
            }
            return Err(BindError::Unmapped {
                fact_type: requirement.fact_type.clone(),
            });
        };
        place(&mut request, &requirement.fact_type, binding, value)?;
    }

    // Constants the endpoint needs that no fact models — `function=GLOBAL_QUOTE`
    // and friends. They ride in the query unless they say otherwise, which is
    // what `capability_offers.fixed_params` already means.
    request.query.sort();
    Ok(request)
}

fn place(
    request: &mut WireRequest,
    fact_type: &str,
    binding: &InputBinding,
    value: &Value,
) -> Result<(), BindError> {
    // `in` is optional in the curated rows of migration 075. Absent means the
    // query string, which is what every one of those rows relies on.
    let location = binding.location.as_deref().unwrap_or("query");
    match location {
        "query" => {
            let name = binding.name.clone();
            request
                .query
                .push((name.clone(), scalar(fact_type, &name, value)?));
        }
        "path" | "path_append" => {
            request.path_segments.push(PathValue {
                name: binding.name.clone(),
                value: scalar(fact_type, &binding.name, value)?,
            });
        }
        "body" => {
            let placed = match binding.wrap.as_deref() {
                Some("array") => Value::Array(vec![value.clone()]),
                _ => value.clone(),
            };
            request.body.insert(binding.name.clone(), placed);
        }
        other => {
            return Err(BindError::Unsupported {
                fact_type: fact_type.to_string(),
                location: other.to_string(),
            })
        }
    }
    Ok(())
}

/// A path segment and a query value are both text. An object is not.
fn scalar(fact_type: &str, name: &str, value: &Value) -> Result<String, BindError> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Number(number) => Ok(number.to_string()),
        Value::Bool(flag) => Ok(flag.to_string()),
        _ => Err(BindError::NotScalar {
            fact_type: fact_type.to_string(),
            name: name.to_string(),
        }),
    }
}

impl WireRequest {
    /// The canonical JSON of what will be sent, for the input hash and for the
    /// row written before the request leaves.
    pub fn as_json(&self) -> Value {
        serde_json::json!({
            // VALUES only, deliberately. The parameter name is needed to build
            // the URL but is not part of what was sent, and folding it in here
            // would change every idempotency key for no behavioural reason.
            "path": self.path_segments
                .iter()
                .map(|segment| segment.value.clone())
                .collect::<Vec<_>>(),
            "query": self.query
                .iter()
                .map(|(k, v)| serde_json::json!([k, v]))
                .collect::<Vec<_>>(),
            "body": Value::Object(self.body.clone()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ZOEKEN: &str = include_str!("../../fixtures/contracts/kvk-zoeken.json");
    const BASISPROFIEL: &str = include_str!("../../fixtures/contracts/kvk-basisprofiel.json");
    const JAARREKENINGEN: &str = include_str!("../../fixtures/contracts/kvk-jaarrekeningen.json");

    fn contract(text: &str) -> ExecutionContract {
        serde_json::from_str(text).unwrap()
    }

    fn facts(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn the_search_puts_the_name_in_the_query() {
        // Matches the real call: /api/v2/zoeken?naam=Volve Capital
        let bound = bind(
            &contract(ZOEKEN),
            &facts(&[("company.name", json!("Volve Capital"))]),
        )
        .unwrap();
        assert_eq!(
            bound.query,
            vec![("naam".to_string(), "Volve Capital".to_string())]
        );
        assert!(bound.path_segments.is_empty());
        assert!(bound.body.is_empty());
    }

    #[test]
    fn the_profile_appends_the_number_to_the_path() {
        // Matches the real call: /api/v1/basisprofielen/30204462
        let bound = bind(
            &contract(BASISPROFIEL),
            &facts(&[("company.registration.nl.kvk", json!("30204462"))]),
        )
        .unwrap();
        assert_eq!(
            bound
                .path_segments
                .iter()
                .map(|s| s.value.clone())
                .collect::<Vec<_>>(),
            vec!["30204462".to_string()]
        );
        assert!(bound.query.is_empty());
    }

    #[test]
    fn the_filing_appends_the_number_too() {
        let bound = bind(
            &contract(JAARREKENINGEN),
            &facts(&[("company.registration.nl.kvk", json!("86717162"))]),
        )
        .unwrap();
        assert_eq!(
            bound
                .path_segments
                .iter()
                .map(|s| s.value.clone())
                .collect::<Vec<_>>(),
            vec!["86717162".to_string()]
        );
    }

    #[test]
    fn a_missing_required_fact_refuses_instead_of_sending_a_gap() {
        assert_eq!(
            bind(&contract(BASISPROFIEL), &facts(&[])),
            Err(BindError::Missing {
                fact_type: "company.registration.nl.kvk".to_string()
            })
        );
    }

    #[test]
    fn a_missing_fact_never_triggers_a_lookup_to_go_and_find_it() {
        // The existing /v1/do path fires a paid KVK search when a binding looks
        // unready, uncapped unless the caller sent a price field. On the job
        // path the answer is a refusal, and it costs nothing.
        let error = bind(&contract(BASISPROFIEL), &facts(&[])).unwrap_err();
        assert!(error.to_string().contains("the call is not sent"));
    }

    #[test]
    fn an_optional_fact_that_is_missing_is_simply_left_out() {
        let mut relaxed = contract(BASISPROFIEL);
        relaxed.requires[0].optional = true;
        let bound = bind(&relaxed, &facts(&[])).unwrap();
        assert!(bound.path_segments.is_empty());
        assert!(bound.query.is_empty());
    }

    #[test]
    fn a_required_fact_with_no_wire_location_refuses() {
        let mut broken = contract(BASISPROFIEL);
        broken.input_map.clear();
        assert!(matches!(
            bind(
                &broken,
                &facts(&[("company.registration.nl.kvk", json!("30204462"))])
            ),
            Err(BindError::Unmapped { .. })
        ));
    }

    #[test]
    fn an_object_cannot_be_a_path_segment() {
        assert!(matches!(
            bind(
                &contract(BASISPROFIEL),
                &facts(&[("company.registration.nl.kvk", json!({ "n": 1 }))])
            ),
            Err(BindError::NotScalar { .. })
        ));
    }

    #[test]
    fn a_body_binding_can_wrap_a_value_in_an_array() {
        let mut contract = contract(ZOEKEN);
        contract.input_map.insert(
            "company.name".to_string(),
            InputBinding {
                location: Some("body".to_string()),
                name: "namen".to_string(),
                wrap: Some("array".to_string()),
            },
        );
        let bound = bind(&contract, &facts(&[("company.name", json!("Mollie"))])).unwrap();
        assert_eq!(bound.body.get("namen"), Some(&json!(["Mollie"])));
    }

    #[test]
    fn a_location_nobody_implemented_refuses_rather_than_guessing() {
        let mut contract = contract(ZOEKEN);
        contract.input_map.insert(
            "company.name".to_string(),
            InputBinding {
                location: Some("header".to_string()),
                name: "X-Name".to_string(),
                wrap: None,
            },
        );
        assert!(matches!(
            bind(&contract, &facts(&[("company.name", json!("Mollie"))])),
            Err(BindError::Unsupported { .. })
        ));
    }

    #[test]
    fn the_same_request_renders_identically_every_time() {
        // What makes the input hash — and therefore the idempotency key —
        // stable across processes and restarts.
        let bound = bind(
            &contract(ZOEKEN),
            &facts(&[("company.name", json!("Volve Capital"))]),
        )
        .unwrap();
        let once = crate::job::attempt::input_hash(&bound.as_json());
        let twice = crate::job::attempt::input_hash(&bound.as_json());
        assert_eq!(once, twice);
        assert_eq!(once.len(), 64);
    }

    #[test]
    fn two_different_companies_bind_to_two_different_requests() {
        let a = bind(
            &contract(BASISPROFIEL),
            &facts(&[("company.registration.nl.kvk", json!("30204462"))]),
        )
        .unwrap();
        let b = bind(
            &contract(BASISPROFIEL),
            &facts(&[("company.registration.nl.kvk", json!("92327737"))]),
        )
        .unwrap();
        assert_ne!(
            crate::job::attempt::input_hash(&a.as_json()),
            crate::job::attempt::input_hash(&b.as_json())
        );
    }
}
