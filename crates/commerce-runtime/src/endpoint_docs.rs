//! The agent-facing documentation of one payable endpoint.
//!
//! `/.well-known/x402` is the page agents actually read — a crawler ingests it
//! once and every downstream agent decides from that text alone whether it can
//! call us. Until now each discovery item carried only `method`, `path`, a price
//! and a one-line description, so an agent that wanted to *use* a resource had
//! to make a second trip to `/{slug}/metadata` to learn what parameters it takes
//! — or guess, and burn a payment on a 400.
//!
//! Everything needed to construct a correct first call is already in the
//! database (`api_endpoints.query_params`, `request_body`, `request_example`,
//! `response_body`, `response_example`, `endpoint_pricing_rules`); the discovery
//! document simply threw it away. This module turns those columns into the three
//! blocks an agent needs — `input`, `example`, `output` — plus the pricing block
//! that tells it when the advertised price is not the whole story.
//!
//! Everything here is pure and byte-budgeted. The document already ships ~1.4k
//! items, so each field is capped (see the `MAX_*` constants) and anything that
//! overflows is replaced by a pointer to `/{slug}/metadata`, which serves the
//! untruncated version. Richer must never mean unparseable.

use serde::Deserialize;
use serde_json::{json, Map, Value};

/// Byte caps for the values copied into a discovery item. A schema is worth more
/// per byte than an example (it describes every call, not one), so it gets the
/// bigger budget. Anything larger is replaced with a `truncated` marker carrying
/// the URL that serves the full value.
const MAX_SCHEMA_BYTES: usize = 4_096;
const MAX_EXAMPLE_BYTES: usize = 1_536;
/// Cap on parameters materialised into the example request, so a schema with
/// hundreds of optional properties cannot inflate every item.
const MAX_EXAMPLE_PARAMS: usize = 24;
/// Cap on a description copied into `accepts[].description`, which some clients
/// render verbatim in a payment prompt.
const MAX_DESCRIPTION_CHARS: usize = 400;
/// Cap on the prose documentation block. Generous next to the one-line
/// description, because this is the field that explains what the endpoint is
/// *for*, but still bounded — the full text is always at `/{slug}/metadata`.
const MAX_DOCUMENTATION_CHARS: usize = 1_200;

/// One endpoint row out of the `endpoint_docs` jsonb aggregate built by
/// `Database::list_active_apis` (see `db::discovery`). Unknown keys are ignored,
/// and every documentation field is optional — most listings have some of them.
#[derive(Debug, Default, Deserialize)]
pub struct EndpointDoc {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub price_usd: Value,
    #[serde(default)]
    pub payment_required: bool,
    #[serde(default)]
    pub network_token_id: Option<uuid::Uuid>,
    /// JSON Schema for the query string (`{"type":"object","properties":{...}}`).
    #[serde(default)]
    pub query_params: Option<Value>,
    /// JSON Schema for the request body.
    #[serde(default)]
    pub request_body: Option<Value>,
    /// JSON Schema for the response body.
    #[serde(default)]
    pub response_body: Option<Value>,
    /// A concrete example request body, as the provider documented it.
    #[serde(default)]
    pub request_example: Option<Value>,
    /// A concrete example response, as the provider documented it.
    #[serde(default)]
    pub response_example: Option<Value>,
    /// Extra request headers the upstream expects (beyond payment).
    #[serde(default)]
    pub headers: Option<Value>,
    /// Parameter-conditional prices — see [`EndpointDoc::pricing`].
    #[serde(default)]
    pub pricing_rules: Value,
    /// The provider's prose description of what this endpoint does and how to
    /// use it — richer than `description`, which is one line.
    #[serde(default)]
    pub documentation_markdown: Option<String>,
    #[serde(default)]
    pub external_docs_url: Option<String>,
    #[serde(default)]
    pub openapi_url: Option<String>,
    #[serde(default)]
    pub authentication_notes: Option<String>,
}

impl EndpointDoc {
    /// The gateway URL an agent calls to execute (and pay for) this endpoint.
    pub fn resource_url(&self, base_url: &str, slug: &str) -> String {
        format!("{base_url}/{slug}/{}", self.path.trim_start_matches('/'))
    }

    /// The description an agent reads before deciding to pay.
    ///
    /// The old fallback ("API call to {slug}") told a reader nothing beyond the
    /// slug they already had. Prefer, in order: the endpoint's own description,
    /// then the listing description prefixed with the operation, then the
    /// operation alone — so the worst case is still `GET /current on weather`
    /// rather than a tautology.
    pub fn description(&self, api_name: &str, api_description: &str, slug: &str) -> String {
        let operation = format!(
            "{} /{}",
            self.method_upper(),
            self.path.trim_start_matches('/')
        );

        let text = match trimmed(self.description.as_deref()) {
            Some(own) => own.to_string(),
            None => match trimmed(Some(api_description)) {
                Some(listing) => format!("{operation} — {listing}"),
                None => {
                    let subject = trimmed(Some(api_name)).unwrap_or(slug);
                    format!("{operation} on {subject}")
                }
            },
        };

        truncate_chars(&text, MAX_DESCRIPTION_CHARS)
    }

    pub fn method_upper(&self) -> String {
        let method = self.method.trim();
        if method.is_empty() {
            "GET".to_string()
        } else {
            method.to_ascii_uppercase()
        }
    }

    /// What the caller may send: the query-string schema, the body schema and
    /// any extra headers. `None` when the listing documented none of them, so
    /// the item stays as small as it is today.
    pub fn input_contract(&self, full_docs_url: &str) -> Option<Value> {
        let query = usable_schema(self.query_params.as_ref());
        let body = usable_schema(self.request_body.as_ref());
        let headers = usable_schema(self.headers.as_ref());
        if query.is_none() && body.is_none() && headers.is_none() {
            return None;
        }

        let mut out = Map::new();
        if let Some(query) = query {
            out.insert(
                "query".to_string(),
                budgeted(query.clone(), MAX_SCHEMA_BYTES, full_docs_url),
            );
        }
        if let Some(body) = body {
            out.insert(
                "body".to_string(),
                budgeted(body.clone(), MAX_SCHEMA_BYTES, full_docs_url),
            );
        }
        if let Some(headers) = headers {
            out.insert(
                "headers".to_string(),
                budgeted(headers.clone(), MAX_SCHEMA_BYTES, full_docs_url),
            );
        }
        Some(Value::Object(out))
    }

    /// The provider's prose documentation for this endpoint.
    ///
    /// It was already being read out of the database on every discovery build
    /// and then dropped on the floor: `documentation_markdown` is selected into
    /// the `endpoint_docs` aggregate (see `db::discovery`), but this struct had
    /// no field for it, so serde discarded it. The listing page rendered it and
    /// the document agents actually read did not — so the sentence explaining
    /// that an endpoint serves *SEC Form 4 filings* reached humans only, while
    /// the agent saw "Retrieves insider transactions".
    ///
    /// A schema says what an endpoint accepts; this says what it is for. An
    /// agent choosing between two listings needs both.
    pub fn documentation(&self) -> Option<String> {
        let text = trimmed(self.documentation_markdown.as_deref())?;
        Some(truncate_chars(text, MAX_DOCUMENTATION_CHARS))
    }

    /// What the caller gets back: response schema and a real example payload.
    pub fn output_contract(&self, full_docs_url: &str) -> Option<Value> {
        let schema = usable_schema(self.response_body.as_ref());
        let example = self.response_example.as_ref().filter(|v| !v.is_null());
        if schema.is_none() && example.is_none() {
            return None;
        }

        let mut out = Map::new();
        out.insert("mimeType".to_string(), json!("application/json"));
        if let Some(schema) = schema {
            out.insert(
                "schema".to_string(),
                budgeted(schema.clone(), MAX_SCHEMA_BYTES, full_docs_url),
            );
        }
        if let Some(example) = example {
            out.insert(
                "example".to_string(),
                budgeted(example.clone(), MAX_EXAMPLE_BYTES, full_docs_url),
            );
        }
        Some(Value::Object(out))
    }

    /// A ready-to-run example call: the resource URL with example query values
    /// filled in, the body an agent should send, and the equivalent curl.
    ///
    /// Values come from the schema's own `example`/`default`/`enum` annotations,
    /// never invented. When a *required* parameter has no documented value the
    /// example carries a `<name>` placeholder and lists it under `placeholders`,
    /// so an agent can tell "copy this" from "fill this in first" — a silently
    /// plausible example is worse than an obviously incomplete one.
    pub fn example(&self, resource_url: &str, full_docs_url: &str) -> Option<Value> {
        let method = self.method_upper();
        let mut placeholders = Vec::new();

        let query_values = self
            .query_params
            .as_ref()
            .map(|schema| example_values(schema, &mut placeholders))
            .unwrap_or_default();

        let (url, mut path_bound) = apply_path_parameters(resource_url, &query_values);
        let url = append_path_parameters(
            &url,
            self.query_params.as_ref(),
            &query_values,
            &mut path_bound,
        );
        let url = append_query(&url, &query_values, &path_bound);

        let body = self
            .request_example
            .clone()
            .filter(|value| !value.is_null())
            .or_else(|| {
                let values = example_values(self.request_body.as_ref()?, &mut placeholders);
                if values.is_empty() {
                    None
                } else {
                    Some(Value::Object(values.into_iter().collect()))
                }
            })
            .map(|body| budgeted(body, MAX_EXAMPLE_BYTES, full_docs_url));

        // An example with no parameters, no bound path segment and no body adds
        // nothing the `resource` URL does not already say.
        let has_query = url.contains('?');
        if !has_query && body.is_none() && path_bound.is_empty() {
            return None;
        }

        let mut out = Map::new();
        out.insert("method".to_string(), json!(method));
        out.insert("url".to_string(), json!(url));
        if let Some(body) = body.clone() {
            out.insert("body".to_string(), body);
        }
        if !placeholders.is_empty() {
            out.insert("placeholders".to_string(), json!(placeholders));
        }
        out.insert("curl".to_string(), json!(curl_for(&method, &url, &body)));
        Some(Value::Object(out))
    }

    /// The price block. Most endpoints charge one price per call; some carry
    /// `endpoint_pricing_rules`, where a parameter value selects a different
    /// price. An item advertising a single amount while the gateway charges by
    /// rule is exactly the kind of stale requirement that makes an agent's
    /// prepared payment fail, so surface the rules alongside the base price.
    pub fn pricing(&self, price_usd: &str, atomic_amount: &str, asset_symbol: &str) -> Value {
        let rules: Vec<Value> = self
            .pricing_rules
            .as_array()
            .map(|rules| rules.iter().map(normalize_pricing_rule).collect())
            .unwrap_or_default();

        let mut out = Map::new();
        out.insert("usd".to_string(), json!(price_usd));
        out.insert("amount".to_string(), json!(atomic_amount));
        out.insert("asset".to_string(), json!(asset_symbol));
        out.insert(
            "model".to_string(),
            json!(if rules.is_empty() {
                "per-call"
            } else {
                "per-call-conditional"
            }),
        );
        if !rules.is_empty() {
            out.insert("rules".to_string(), Value::Array(rules));
            out.insert(
                "note".to_string(),
                json!(
                    "The amount charged depends on request parameters; the live 402 challenge \
                     for your exact request is authoritative."
                ),
            );
        }
        Value::Object(out)
    }

    /// The x402 v1 `outputSchema` block, as the CDP Bazaar's
    /// `extractDiscoveryInfoV1` reads it:
    /// `{ input: { type, method, discoverable, queryParams|bodyFields }, output }`.
    ///
    /// The live 402 already advertises this (see `payment::BazaarMetadata`); the
    /// discovery document did not, so an indexer reading only this document
    /// learned a price and nothing about the call. Same shape in both places, so
    /// a resource is catalogued identically whether it was found by crawling or
    /// by hitting a 402.
    pub fn discovery_schema(&self, full_docs_url: &str) -> Value {
        let method = self.method_upper();
        let mut input = json!({
            "type": "http",
            "method": method,
            "discoverable": true,
        });
        let object = input.as_object_mut().expect("input is an object literal");
        if matches!(method.as_str(), "GET" | "HEAD" | "DELETE") {
            object.insert(
                "queryParams".to_string(),
                usable_schema(self.query_params.as_ref())
                    .map(|schema| budgeted(schema.clone(), MAX_SCHEMA_BYTES, full_docs_url))
                    .unwrap_or_else(|| json!({})),
            );
        } else {
            // Body methods: the v1 extractor reads `bodyFields`, which is the
            // property map rather than the whole schema.
            let fields = usable_schema(self.request_body.as_ref())
                .map(|schema| {
                    schema
                        .get("properties")
                        .cloned()
                        .unwrap_or_else(|| schema.clone())
                })
                .map(|fields| budgeted(fields, MAX_SCHEMA_BYTES, full_docs_url))
                .unwrap_or_else(|| json!({}));
            object.insert("bodyFields".to_string(), fields);
        }

        let mut out = Map::new();
        out.insert("input".to_string(), input);
        if let Some(example) = self.response_example.as_ref().filter(|v| !v.is_null()) {
            out.insert(
                "output".to_string(),
                budgeted(example.clone(), MAX_EXAMPLE_BYTES, full_docs_url),
            );
        }
        Value::Object(out)
    }

    /// Per-endpoint documentation links, when the provider supplied any.
    pub fn doc_links(&self) -> Option<Value> {
        let mut out = Map::new();
        if let Some(url) = trimmed(self.external_docs_url.as_deref()) {
            out.insert("externalDocs".to_string(), json!(url));
        }
        if let Some(url) = trimmed(self.openapi_url.as_deref()) {
            out.insert("openapi".to_string(), json!(url));
        }
        if let Some(notes) = trimmed(self.authentication_notes.as_deref()) {
            out.insert(
                "authenticationNotes".to_string(),
                json!(truncate_chars(notes, MAX_DESCRIPTION_CHARS)),
            );
        }
        if out.is_empty() {
            None
        } else {
            Some(Value::Object(out))
        }
    }
}

fn normalize_pricing_rule(rule: &Value) -> Value {
    let get = |key: &str| rule.get(key).cloned().unwrap_or(Value::Null);
    json!({
        "parameter": get("parameter_name"),
        "operator": get("operator"),
        "value": get("value"),
        "usd": get("price"),
    })
}

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let kept: String = value.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// A schema is usable when it is a non-empty object or array. Empty objects and
/// SQL nulls are noise that would only make items bigger.
fn usable_schema(value: Option<&Value>) -> Option<&Value> {
    match value? {
        Value::Object(map) if !map.is_empty() => value,
        Value::Array(items) if !items.is_empty() => value,
        _ => None,
    }
}

/// Keep `value` if it serializes within `max_bytes`; otherwise replace it with a
/// marker pointing at the endpoint's full metadata document. The document is
/// read by crawlers in one shot, so one pathological listing must not be able to
/// blow up the response for every other listing in it.
fn budgeted(value: Value, max_bytes: usize, full_docs_url: &str) -> Value {
    let bytes = serde_json::to_string(&value).map(|s| s.len()).unwrap_or(0);
    if bytes <= max_bytes {
        return value;
    }
    json!({
        "truncated": true,
        "bytes": bytes,
        "seeAlso": full_docs_url,
    })
}

/// The parameters a schema describes, plus the names of the required ones.
///
/// Two shapes are in the database and both are legitimate, because both are
/// what providers and importers actually wrote:
///
/// * JSON Schema — `{type, required: [names], properties: {name: spec}}`
/// * flat — `{name: spec}`, with each spec carrying its own `required: true`
///
/// Only the first was understood, so 50 published endpoints carried a perfectly
/// good input schema and no runnable example: the example builder looked for
/// `properties`, found none, and gave up. Reading both here is what turns those
/// into a call an agent can make without a second round-trip.
///
/// A flat map is only recognised when every value is itself an object, so a
/// schema with no parameters (or one shaped like something else entirely) is
/// never mistaken for one.
fn parameter_map(schema: &Value) -> Option<(&Map<String, Value>, Vec<String>)> {
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        return Some((properties, required));
    }

    let flat = schema.as_object()?;
    if flat.is_empty() || !flat.values().all(Value::is_object) {
        return None;
    }
    let required = flat
        .iter()
        .filter(|(_, spec)| spec.get("required").and_then(Value::as_bool) == Some(true))
        .map(|(name, _)| name.clone())
        .collect();
    Some((flat, required))
}

/// Pull concrete parameter values out of a schema, in the order an agent
/// should care about: documented examples first, then defaults, then the first
/// enum member. Required properties with no documented value become
/// `<name>` placeholders and are reported through `placeholders`.
fn example_values(schema: &Value, placeholders: &mut Vec<String>) -> Vec<(String, Value)> {
    let Some((properties, required)) = parameter_map(schema) else {
        return Vec::new();
    };
    let required: Vec<&str> = required.iter().map(String::as_str).collect();

    let mut out = Vec::new();
    for (name, spec) in properties {
        if out.len() >= MAX_EXAMPLE_PARAMS {
            break;
        }
        let is_required = required.contains(&name.as_str());
        match documented_value(spec) {
            Some(value) => out.push((name.clone(), value)),
            None if is_required => {
                placeholders.push(name.clone());
                out.push((name.clone(), json!(format!("<{name}>"))));
            }
            None => {}
        }
    }

    // Required parameters first: an agent skimming the example URL should see
    // the ones it cannot omit.
    out.sort_by_key(|(name, _)| !required.contains(&name.as_str()));
    out
}

fn documented_value(spec: &Value) -> Option<Value> {
    for key in ["example", "default"] {
        if let Some(value) = spec.get(key) {
            if !value.is_null() {
                return Some(value.clone());
            }
        }
    }
    spec.get("examples")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
        .cloned()
        .filter(|value| !value.is_null())
        .or_else(|| {
            spec.get("enum")
                .and_then(Value::as_array)
                .and_then(|values| values.first())
                .cloned()
        })
}

/// Substitute `{name}` / `:name` path segments with example values. Returns the
/// bound parameter names so they are not repeated in the query string.
fn apply_path_parameters(url: &str, values: &[(String, Value)]) -> (String, Vec<String>) {
    let mut out = url.to_string();
    let mut bound = Vec::new();

    // `/:name` is checked with its leading slash — every absolute URL contains
    // the scheme's colon, which is not a parameter.
    if out.contains('{') || out.contains("/:") {
        for (name, value) in values {
            let rendered = scalar_to_string(value);
            for pattern in [format!("{{{name}}}"), format!(":{name}")] {
                if out.contains(&pattern) {
                    out = out.replace(&pattern, &rendered);
                    bound.push(name.clone());
                }
            }
        }
    }
    (out, bound)
}

/// Append parameters the schema marks as path segments, in declaration order.
///
/// Some upstreams take their argument as a trailing segment rather than a query
/// string — `/alpha/us`, not `/alpha?code=us`. The endpoint's stored path cannot
/// carry a `{code}` placeholder for those, because request routing matches the
/// stored path as a literal prefix (`db::get_endpoint`), so a templated path
/// would stop resolving the very requests it describes. Marking the parameter
/// `"x-parameter-location": "path"` documents it without touching routing —
/// the same key the catalog already uses to mark query parameters.
fn append_path_parameters(
    url: &str,
    schema: Option<&Value>,
    values: &[(String, Value)],
    bound: &mut Vec<String>,
) -> String {
    let Some((properties, _)) = schema.and_then(parameter_map) else {
        return url.to_string();
    };

    // Declaration order, not the (alphabetical) order of `values`: for a
    // two-segment path the sequence is the whole meaning.
    let mut out = url.trim_end_matches('/').to_string();
    for (name, spec) in properties {
        if spec.get("x-parameter-location").and_then(Value::as_str) != Some("path") {
            continue;
        }
        if bound.contains(name) {
            continue;
        }
        let Some((_, value)) = values.iter().find(|(candidate, _)| candidate == name) else {
            continue;
        };
        out.push('/');
        out.push_str(&urlencoding_segment(&scalar_to_string(value)));
        bound.push(name.clone());
    }
    out
}

/// Percent-encode one path segment. `form_urlencoded` is for query strings — it
/// would turn a space into `+`, which is a literal plus inside a path.
pub(crate) fn urlencoding_segment(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn append_query(url: &str, values: &[(String, Value)], skip: &[String]) -> String {
    let pairs: Vec<(String, String)> = values
        .iter()
        .filter(|(name, _)| !skip.contains(name))
        .map(|(name, value)| (name.clone(), scalar_to_string(value)))
        .collect();
    if pairs.is_empty() {
        return url.to_string();
    }
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish();
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}{query}")
}

/// Render a JSON value for a URL. Strings go in bare (the serializer escapes
/// them); anything structural is JSON-encoded so the example stays valid.
fn scalar_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The example call as a shell one-liner, payment header included — the header
/// is the part agents most often get wrong, and the document is where they read
/// it. `PAYMENT-SIGNATURE` is the x402 v2 header this gateway issues; the legacy
/// `X-Payment` header is still accepted (see `payment.rs`).
fn curl_for(method: &str, url: &str, body: &Option<Value>) -> String {
    let mut curl = format!("curl -X {method} '{}'", url.replace('\'', "'\\''"));
    curl.push_str(" -H 'PAYMENT-SIGNATURE: <x402 payment payload>'");
    if let Some(body) = body {
        if !matches!(body.get("truncated"), Some(Value::Bool(true))) {
            let encoded = serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string());
            curl.push_str(" -H 'Content-Type: application/json'");
            curl.push_str(&format!(" -d '{}'", encoded.replace('\'', "'\\''")));
        }
    }
    curl
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOCS: &str = "https://gateway.apiosk.com/ipdata/metadata";

    fn doc(json: Value) -> EndpointDoc {
        serde_json::from_value(json).expect("endpoint doc")
    }

    #[test]
    fn prose_documentation_reaches_the_discovery_item() {
        let ep = doc(json!({
            "method": "get",
            "path": "/insider_transactions",
            "description": "Retrieves insider transactions",
            "documentation_markdown":
                "Insider buying and selling data from SEC Form 4 filings. Filter by \
                 transaction type, whether it is a scheduled sale, and date range."
        }));

        let documentation = ep.documentation().expect("documentation");
        assert!(
            documentation.contains("SEC Form 4"),
            "the thing that makes this endpoint findable was being dropped"
        );
        // The one-line description stays what it was; this is additional.
        assert_eq!(
            ep.description("CityFALCON", "Financial content API", "cityfalcon"),
            "Retrieves insider transactions"
        );
    }

    #[test]
    fn documentation_is_absent_rather_than_empty() {
        assert!(doc(json!({ "method": "get", "path": "/x" }))
            .documentation()
            .is_none());
        assert!(
            doc(json!({ "method": "get", "path": "/x", "documentation_markdown": "   " }))
                .documentation()
                .is_none()
        );
    }

    #[test]
    fn path_located_parameters_become_trailing_segments() {
        // countrylayer takes its argument as a segment: /alpha/us, never
        // /alpha?code=us. The stored path stays literal so routing keeps
        // working; the schema says where the value goes.
        let ep = doc(json!({
            "method": "get",
            "path": "/alpha",
            "query_params": {
                "type": "object",
                "required": ["code"],
                "properties": {
                    "code": {
                        "type": "string",
                        "example": "us",
                        "x-parameter-location": "path"
                    }
                }
            }
        }));

        let example = ep
            .example("https://gateway.apiosk.com/countrylayer/alpha", DOCS)
            .expect("example");
        assert_eq!(
            example["url"], "https://gateway.apiosk.com/countrylayer/alpha/us",
            "the value is a path segment, not a query parameter"
        );
    }

    #[test]
    fn a_path_parameter_is_percent_encoded_as_a_segment() {
        let ep = doc(json!({
            "method": "get",
            "path": "/capital",
            "query_params": {
                "type": "object",
                "required": ["capital"],
                "properties": {
                    "capital": {
                        "type": "string",
                        "example": "san jose",
                        "x-parameter-location": "path"
                    }
                }
            }
        }));

        let example = ep.example("https://g.example/c/capital", DOCS).expect("ex");
        assert_eq!(
            example["url"], "https://g.example/c/capital/san%20jose",
            "a space in a path segment is %20, never the query string's +"
        );
    }

    #[test]
    fn query_located_parameters_are_untouched_by_the_path_rule() {
        let ep = doc(json!({
            "method": "get",
            "path": "/lookup",
            "query_params": {
                "type": "object",
                "properties": {
                    "ip": {
                        "type": "string",
                        "example": "8.8.8.8",
                        "x-parameter-location": "query"
                    }
                }
            }
        }));

        let example = ep.example("https://g.example/i/lookup", DOCS).expect("ex");
        assert_eq!(example["url"], "https://g.example/i/lookup?ip=8.8.8.8");
    }

    #[test]
    fn a_flat_parameter_map_builds_an_example_too() {
        // The other shape in the database: no `properties` wrapper, each
        // parameter carrying its own `required` flag. 50 published endpoints
        // used it and got an input block but never a runnable example.
        let ep = doc(json!({
            "method": "get",
            "path": "/lookup",
            "query_params": {
                "ip": { "type": "string", "required": true, "example": "8.8.8.8" },
                "include": { "type": "string", "required": false, "example": "security" }
            }
        }));

        let example = ep
            .example("https://gateway.apiosk.com/ipdata/lookup", DOCS)
            .expect("example");
        assert_eq!(
            example["url"],
            "https://gateway.apiosk.com/ipdata/lookup?ip=8.8.8.8&include=security"
        );
        assert!(example.get("placeholders").is_none());
    }

    #[test]
    fn a_flat_map_reports_undocumented_required_parameters_as_placeholders() {
        let ep = doc(json!({
            "method": "get",
            "path": "/quote",
            "query_params": {
                "symbol": { "type": "string", "required": true }
            }
        }));

        let example = ep
            .example("https://gateway.apiosk.com/x/quote", DOCS)
            .expect("example");
        assert_eq!(
            example["url"],
            "https://gateway.apiosk.com/x/quote?symbol=%3Csymbol%3E"
        );
        assert_eq!(example["placeholders"], json!(["symbol"]));
    }

    #[test]
    fn a_map_that_is_not_a_parameter_map_yields_no_example_values() {
        // Values that are not objects must never be read as parameter specs.
        let mut placeholders = Vec::new();
        assert!(example_values(&json!({ "note": "free text" }), &mut placeholders).is_empty());
        assert!(example_values(&json!({}), &mut placeholders).is_empty());
        assert!(placeholders.is_empty());
    }

    #[test]
    fn example_url_uses_documented_parameter_examples() {
        let ep = doc(json!({
            "method": "get",
            "path": "/lookup",
            "query_params": {
                "type": "object",
                "properties": {
                    "ip": { "type": "string", "example": "8.8.8.8" },
                    "include": { "type": "string", "example": "security" }
                },
                "required": ["ip"]
            }
        }));

        let example = ep
            .example("https://gateway.apiosk.com/ipdata/lookup", DOCS)
            .expect("example");
        assert_eq!(example["method"], "GET");
        assert_eq!(
            example["url"],
            "https://gateway.apiosk.com/ipdata/lookup?ip=8.8.8.8&include=security"
        );
        assert!(example["curl"]
            .as_str()
            .unwrap()
            .contains("PAYMENT-SIGNATURE"));
        // Nothing was invented, so no placeholder list.
        assert!(example.get("placeholders").is_none());
    }

    #[test]
    fn required_parameters_without_examples_are_marked_as_placeholders() {
        let ep = doc(json!({
            "method": "GET",
            "path": "/search",
            "query_params": {
                "type": "object",
                "properties": {
                    "q": { "type": "string" },
                    "limit": { "type": "integer", "default": 10 }
                },
                "required": ["q"]
            }
        }));

        let example = ep
            .example("https://gateway.apiosk.com/search-results/search", DOCS)
            .expect("example");
        assert_eq!(example["placeholders"], json!(["q"]));
        assert!(example["url"].as_str().unwrap().contains("q=%3Cq%3E"));
        // Required first, so the URL reads q=... before the optional default.
        assert!(example["url"]
            .as_str()
            .unwrap()
            .ends_with("?q=%3Cq%3E&limit=10"));
    }

    #[test]
    fn body_example_prefers_the_documented_request_example() {
        let ep = doc(json!({
            "method": "POST",
            "path": "/extract",
            "request_body": {
                "type": "object",
                "properties": { "url": { "type": "string", "example": "https://a.example" } },
                "required": ["url"]
            },
            "request_example": { "url": "https://news.example/article" }
        }));

        let example = ep
            .example("https://gateway.apiosk.com/web-extract/extract", DOCS)
            .expect("example");
        assert_eq!(example["body"]["url"], "https://news.example/article");
        let curl = example["curl"].as_str().unwrap();
        assert!(curl.contains("-X POST"));
        assert!(curl.contains("Content-Type: application/json"));
    }

    #[test]
    fn body_example_falls_back_to_schema_annotations() {
        let ep = doc(json!({
            "method": "POST",
            "path": "/transcript",
            "request_body": {
                "type": "object",
                "properties": {
                    "audio_url": { "type": "string", "example": "https://a.example/x.mp3" },
                    "speaker_labels": { "type": "boolean", "example": true }
                },
                "required": ["audio_url"]
            }
        }));

        let example = ep
            .example("https://gateway.apiosk.com/transcribe/transcript", DOCS)
            .expect("example");
        assert_eq!(example["body"]["audio_url"], "https://a.example/x.mp3");
        assert_eq!(example["body"]["speaker_labels"], true);
    }

    #[test]
    fn path_parameters_are_substituted_not_appended() {
        let ep = doc(json!({
            "method": "GET",
            "path": "/transcript/{id}",
            "query_params": {
                "type": "object",
                "properties": { "id": { "type": "string", "example": "abc-123" } },
                "required": ["id"]
            }
        }));

        let example = ep
            .example(
                "https://gateway.apiosk.com/transcribe/transcript/{id}",
                DOCS,
            )
            .expect("example");
        assert_eq!(
            example["url"],
            "https://gateway.apiosk.com/transcribe/transcript/abc-123"
        );
    }

    #[test]
    fn endpoints_without_documented_input_get_no_example() {
        let ep = doc(json!({ "method": "GET", "path": "/current" }));
        assert!(ep
            .example("https://gateway.apiosk.com/weather/current", DOCS)
            .is_none());
        assert!(ep.input_contract(DOCS).is_none());
        assert!(ep.output_contract(DOCS).is_none());
    }

    #[test]
    fn description_falls_back_to_the_listing_not_to_the_slug() {
        let ep = doc(json!({ "method": "GET", "path": "/current" }));
        assert_eq!(
            ep.description("Weather API", "Current conditions by city.", "weather"),
            "GET /current — Current conditions by city."
        );
        assert_eq!(
            ep.description("Weather API", "  ", "weather"),
            "GET /current on Weather API"
        );

        let documented = doc(json!({
            "method": "GET",
            "path": "/current",
            "description": "Current temperature, wind and precipitation for one city."
        }));
        assert_eq!(
            documented.description("Weather API", "ignored", "weather"),
            "Current temperature, wind and precipitation for one city."
        );
    }

    #[test]
    fn oversized_values_become_pointers_to_full_metadata() {
        let big: String = "x".repeat(MAX_EXAMPLE_BYTES + 100);
        let ep = doc(json!({
            "method": "GET",
            "path": "/big",
            "response_example": { "blob": big }
        }));

        let output = ep.output_contract(DOCS).expect("output");
        assert_eq!(output["example"]["truncated"], true);
        assert_eq!(output["example"]["seeAlso"], DOCS);
    }

    #[test]
    fn pricing_surfaces_conditional_rules() {
        let ep = doc(json!({
            "method": "POST",
            "path": "/order",
            "pricing_rules": [
                { "parameter_name": "size", "operator": "eq", "value": "large", "price": 0.02 }
            ]
        }));

        let pricing = ep.pricing("0.01", "10000", "USDC");
        assert_eq!(pricing["model"], "per-call-conditional");
        assert_eq!(pricing["rules"][0]["parameter"], "size");
        assert_eq!(pricing["rules"][0]["usd"], 0.02);

        let flat = doc(json!({ "method": "GET", "path": "/x" })).pricing("0.01", "10000", "USDC");
        assert_eq!(flat["model"], "per-call");
        assert!(flat.get("rules").is_none());
    }

    #[test]
    fn input_contract_carries_query_body_and_headers() {
        let ep = doc(json!({
            "method": "POST",
            "path": "/x",
            "query_params": { "type": "object", "properties": { "a": { "type": "string" } } },
            "request_body": { "type": "object", "properties": { "b": { "type": "string" } } },
            "headers": { "X-Trace": "optional" }
        }));

        let input = ep.input_contract(DOCS).expect("input");
        assert_eq!(input["query"]["properties"]["a"]["type"], "string");
        assert_eq!(input["body"]["properties"]["b"]["type"], "string");
        assert_eq!(input["headers"]["X-Trace"], "optional");
    }
}
