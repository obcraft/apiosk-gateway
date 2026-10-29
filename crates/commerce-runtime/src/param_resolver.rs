//! The generic missing-input resolver: pure helpers.
//!
//! "Missing required input → find resolver API → resolve identifier →
//! continue original API automatically." The declarations live in
//! `param_resolvers` (migration 094, loaded by `db::resolvers_for`); the
//! orchestration — binding the resolver, paying it through the loopback,
//! attaching the value, reporting the steps — lives beside `/v1/do` in
//! `v1_routes::do_job`, because that is where the payment plumbing is.
//!
//! What lives HERE is everything that can be tested without a database or a
//! wallet: walking an extraction path into a response body, and deciding
//! whether a multi-hit search result has one defensible winner.
//!
//! Disambiguation is deliberately conservative. A registry search ("Mollie",
//! "Shell") can return many registrations, and the upstream guarantees no
//! semantic ranking — for a paid authoritative source, binding the wrong
//! identity is far worse than asking once. So: zero hits is a refusal, one
//! hit is used, and more than one is used only when exactly one hit survives
//! an exact normalized-name match (plus active/type preferences); anything
//! else is handed back as candidates for the caller to choose from.

use serde_json::Value;

/// How many candidates a `resolver_ambiguous` refusal names.
pub const MAX_CANDIDATES: usize = 10;

/// Walk a dotted/indexed path (`resultaten[0].kvkNummer`) into a JSON body.
///
/// Supports object keys separated by `.` and `[n]` array indexing appended to
/// a key (or standalone at the start). No wildcard, no filters — a resolver
/// declaration that needs more than this needs a design conversation, not a
/// jsonpath dependency.
pub fn extract<'a>(path: &str, body: &'a Value) -> Option<&'a Value> {
    let mut current = body;
    for segment in path.split('.') {
        if segment.is_empty() {
            return None;
        }
        // `name[0][1]` → key lookup, then each index in turn.
        let (key, rest) = match segment.find('[') {
            Some(pos) => (&segment[..pos], &segment[pos..]),
            None => (segment, ""),
        };
        if !key.is_empty() {
            current = current.get(key)?;
        }
        let mut rest = rest;
        while let Some(stripped) = rest.strip_prefix('[') {
            let end = stripped.find(']')?;
            let index: usize = stripped[..end].parse().ok()?;
            current = current.get(index)?;
            rest = &stripped[end + 1..];
        }
        if !rest.is_empty() {
            return None;
        }
    }
    Some(current)
}

/// Whether a CONCRETE value can be the identifier the declaration describes.
///
/// The other half of the `digits:N` vocabulary — [`shape_is_present`] asks it
/// of free text, this asks it of one bound value.
///
/// Judgement is one-way: an unknown vocabulary, or a value of a type the shape
/// cannot speak about, answers "fits". A declaration this function cannot read
/// must not start rejecting values on a guess, and the cost of a false "fits"
/// is only today's behaviour.
pub fn shape_fits(shape: Option<&str>, value: &Value) -> bool {
    let Some(shape) = shape else { return true };
    let said = match value {
        Value::String(s) => s.trim().to_string(),
        // A number is a legitimate way to write an identifier, and `12345678`
        // arriving unquoted must not read as "not eight digits".
        Value::Number(n) => n.to_string(),
        _ => return true,
    };
    match shape.split_once(':') {
        Some(("digits", n)) => match n.parse::<usize>() {
            Ok(width) => said.len() == width && said.chars().all(|c| c.is_ascii_digit()),
            Err(_) => true,
        },
        _ => true,
    }
}

/// Whether a value of the declared shape is already present in the job text
/// or the caller's supplied inputs.
///
/// Prediction only, for the read-only surfaces (`/v1/discover`, `/v1/quote`,
/// `/v1/preflight`) that draw the pre-step line before anything is bound.
/// `/v1/do` does not consult this to decide whether a lookup is NEEDED: a run
/// of digits in a sentence is no proof it is the identifier the endpoint wants
/// — it could be a phone fragment or an order number — and only the binder,
/// working from the endpoint's own schema, can settle that. Being wrong here
/// costs a grey line, never a charge.
///
/// Vocabulary: `digits:N` — a standalone run of exactly N digits. Unknown
/// shapes report "present" so an unrecognised declaration hides the line
/// rather than promising a step that may not run.
pub fn shape_is_present(shape: Option<&str>, param: &str, job: &str, supplied: &Value) -> bool {
    // An explicit input settles it — but only if it could BE the identifier.
    //
    // This used to return true for any non-empty value, "whatever it looks
    // like: the caller answered the question". They had not. The one job this
    // whole mechanism exists for — "de kvk informatie van Mollie BV" — puts a
    // company NAME in `kvkNummer`, because that is the only thing the sentence
    // holds and the field is the only place to put it. Reading that as an
    // answer hid the lookup line, quoted the profile call alone, and sent
    // "Mollie BV" upstream as a registration number, which KVK refuses with
    // IPD0004. A value the declaration says cannot be the identifier is not an
    // answer; it is the search term for the step that produces one.
    if let Some(value) = supplied
        .get(param)
        .filter(|v| !v.is_null() && v.as_str() != Some(""))
    {
        return shape_fits(shape, value);
    }
    let Some(shape) = shape else {
        // Nothing declared: assume the lookup is needed, which is the case
        // the line exists for.
        return false;
    };
    match shape.split_once(':') {
        Some(("digits", n)) => match n.parse::<usize>() {
            Ok(width) => has_digit_run(job, width),
            Err(_) => true,
        },
        _ => true,
    }
}

/// A run of exactly `width` digits, not adjacent to other digits.
fn has_digit_run(text: &str, width: usize) -> bool {
    if width == 0 {
        return false;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if !chars[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < chars.len() && chars[index].is_ascii_digit() {
            index += 1;
        }
        if index - start == width {
            return true;
        }
    }
    false
}

/// The pre-step lines for one candidate: which lookups a read-only surface
/// should show above the price, and what each would add to the total.
///
/// Prediction, not a promise — see [`shape_is_present`]. `/v1/do` charges
/// only what actually runs and reports it in `steps`.
pub fn pre_steps_for(
    rows: &[crate::db::PreStepRow],
    endpoint_id: Option<uuid::Uuid>,
    capability: Option<&str>,
    job: &str,
    supplied: &Value,
) -> Vec<Value> {
    let mut seen: Vec<&str> = Vec::new();
    let mut out = Vec::new();
    // Endpoint-level declarations win over capability-level ones for the same
    // parameter, the same way `Database::resolvers_for` resolves them.
    for endpoint_level in [true, false] {
        for row in rows {
            let matches_target = if endpoint_level {
                row.target_endpoint_id.is_some() && row.target_endpoint_id == endpoint_id
            } else {
                row.target_endpoint_id.is_none()
                    && row.target_capability.as_deref().is_some()
                    && row.target_capability.as_deref() == capability.filter(|c| !c.is_empty())
            };
            if !matches_target || seen.contains(&row.target_param.as_str()) {
                continue;
            }
            seen.push(&row.target_param);
            if shape_is_present(
                row.target_shape.as_deref(),
                &row.target_param,
                job,
                supplied,
            ) {
                continue;
            }
            out.push(serde_json::json!({
                "role": "resolver",
                "label": row.label,
                "parameter": row.target_param,
                // What the identifier looks like, so a surface drawing this
                // line can decide for itself whether what the user typed is
                // one — see `shape_fits`. Without it the confirm panel can
                // only test "is the field non-empty", which drops the line the
                // moment a company name is typed into it.
                "shape": row.target_shape,
                // The resolver's own upstream leg, beside the buyer figure —
                // the same pair every listing publishes (`ApiInfoResponse`).
                // Never rendered: it is here because the ceiling a caller
                // sends is compared in LIST prices, so a client that has to
                // bound a two-call job needs both legs of both calls. See
                // `selectionLegs` in the app.
                "price_usd": row.resolver_price_usd,
                "api_slug": row.resolver_api_slug,
                "endpoint": format!("{} {}", row.resolver_method, row.resolver_path),
                "buyer_price_usd": crate::fees::buyer_total_usd(
                    row.resolver_price_usd,
                    row.resolver_hosted_externally,
                ),
                "executed": false,
            }));
        }
    }
    out
}

/// The buyer total of a candidate plus every pre-step it would need.
pub fn combined_buyer_price_usd(candidate_buyer_price_usd: f64, pre_steps: &[Value]) -> f64 {
    let extra: f64 = pre_steps
        .iter()
        .filter_map(|s| s.get("buyer_price_usd").and_then(Value::as_f64))
        .sum();
    candidate_buyer_price_usd + extra
}

/// The verdict on a hit list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitChoice {
    /// No hits: the searched value names nothing in the registry.
    Empty,
    /// This index is defensible — either the only hit, or the only hit whose
    /// normalized name exactly matches the searched value (tightened by the
    /// active flag and preferred types when declared).
    Chosen(usize),
    /// More than one hit and no clear winner. Never guessed past.
    Ambiguous,
}

/// Choose a hit, conservatively. `match_fields` is the declaration's
/// disambiguation map:
///
/// ```json
/// {"name": "naam", "active": "actief", "type": "type",
///  "preferred_types": ["rechtspersoon", "hoofdvestiging"]}
/// ```
///
/// All keys optional; with no usable map a multi-hit result is simply
/// ambiguous.
/// Keep only hits that can actually produce the declared value.
///
/// KVK Search returns both legal entities (rechtspersoon: kvkNummer, no
/// vestigingsnummer) and establishments (hoofdvestiging/nevenvestiging: has
/// one) in a single result set. A resolver extracting `vestigingsnummer`
/// must never be handed a legal-entity hit to choose among — it was never a
/// real candidate for producing that value, whatever a declaration's
/// `preferred_types` says. Filtering here, ahead of `choose_hit`, keeps that
/// true generically rather than depending on every declaration getting its
/// own `match_fields` right.
pub fn hits_that_carry(hits: Vec<Value>, extraction_path: &str) -> Vec<Value> {
    hits.into_iter()
        .filter(|hit| extract(extraction_path, hit).is_some())
        .collect()
}

pub fn choose_hit(hits: &[Value], searched: &str, match_fields: Option<&Value>) -> HitChoice {
    match hits.len() {
        0 => HitChoice::Empty,
        1 => HitChoice::Chosen(0),
        _ => {
            let Some(fields) = match_fields else {
                return HitChoice::Ambiguous;
            };
            let name_field = fields.get("name").and_then(Value::as_str);
            let Some(name_field) = name_field else {
                return HitChoice::Ambiguous;
            };
            let active_field = fields.get("active").and_then(Value::as_str);
            let type_field = fields.get("type").and_then(Value::as_str);
            let preferred_types: Vec<&str> = fields
                .get("preferred_types")
                .and_then(Value::as_array)
                .map(|v| v.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();

            let (wanted, wanted_suffix) = normalize_name(searched);
            if wanted.is_empty() {
                return HitChoice::Ambiguous;
            }

            let exact: Vec<usize> = hits
                .iter()
                .enumerate()
                .filter(|(_, hit)| {
                    let name = hit.get(name_field).and_then(Value::as_str).unwrap_or("");
                    let (hit_base, hit_suffix) = normalize_name(name);
                    // Base name must match regardless. The legal-form suffix
                    // (B.V./N.V./V.O.F./C.V.) only has to match when the
                    // SEARCHED name stated one: "Mollie" still finds "Mollie
                    // B.V." (today's behaviour, and how a caller usually
                    // writes it), but "Jansen V.O.F." must not silently
                    // resolve to the different company "Jansen B.V." just
                    // because both strip down to "jansen" — two distinct
                    // legal forms are two distinct registrations, and picking
                    // the wrong one is worse than refusing.
                    hit_base == wanted
                        && wanted_suffix.is_none_or(|s| hit_suffix == Some(s))
                        && !is_inactive(hit, active_field)
                })
                .map(|(i, _)| i)
                .collect();

            // Addresses often gain an official postcode in the display name:
            // `Coolsingel 40 Rotterdam` becomes `Coolsingel 40, 3011AD
            // Rotterdam`. That is not an exact normalized string, but it is a
            // defensible unique match when the declaration opts in and every
            // token the buyer supplied occurs in exactly one active hit. This
            // stays opt-in so company-name resolution keeps its deliberately
            // stricter exact-identity rule.
            if exact.is_empty()
                && fields
                    .get("contains_searched_tokens")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                let wanted_tokens = name_tokens(searched);
                if !wanted_tokens.is_empty() {
                    let contained: Vec<usize> = hits
                        .iter()
                        .enumerate()
                        .filter(|(_, hit)| {
                            let hit_tokens = name_tokens(
                                hit.get(name_field).and_then(Value::as_str).unwrap_or(""),
                            );
                            wanted_tokens.iter().all(|token| hit_tokens.contains(token))
                                && !is_inactive(hit, active_field)
                        })
                        .map(|(i, _)| i)
                        .collect();
                    if contained.len() == 1 {
                        return HitChoice::Chosen(contained[0]);
                    }
                }
            }

            match exact.len() {
                1 => HitChoice::Chosen(exact[0]),
                0 => HitChoice::Ambiguous,
                _ => {
                    // Several registrations of the exact same name (a legal
                    // entity plus its main branch, typically). The declared
                    // type preference breaks the tie only when it singles out
                    // exactly one; a tie within the preferred type stays
                    // ambiguous.
                    let Some(type_field) = type_field else {
                        return HitChoice::Ambiguous;
                    };
                    for preferred in &preferred_types {
                        let of_type: Vec<usize> = exact
                            .iter()
                            .copied()
                            .filter(|&i| {
                                hits[i].get(type_field).and_then(Value::as_str) == Some(*preferred)
                            })
                            .collect();
                        match of_type.len() {
                            1 => return HitChoice::Chosen(of_type[0]),
                            0 => continue,
                            _ => return HitChoice::Ambiguous,
                        }
                    }
                    HitChoice::Ambiguous
                }
            }
        }
    }
}

fn name_tokens(value: &str) -> std::collections::HashSet<String> {
    value
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

/// Candidate summaries for a `resolver_ambiguous` refusal: the value the
/// caller would supply, plus enough identity to choose by. Capped at
/// [`MAX_CANDIDATES`].
pub fn candidate_summaries(
    hits: &[Value],
    value_path: &str,
    match_fields: Option<&Value>,
) -> Vec<Value> {
    let name_field = match_fields
        .and_then(|f| f.get("name"))
        .and_then(Value::as_str);
    let type_field = match_fields
        .and_then(|f| f.get("type"))
        .and_then(Value::as_str);
    hits.iter()
        .take(MAX_CANDIDATES)
        .map(|hit| {
            let mut entry = serde_json::Map::new();
            if let Some(value) = extract(value_path, hit) {
                entry.insert("value".into(), value.clone());
            }
            if let Some(name) = name_field.and_then(|f| hit.get(f)).cloned() {
                entry.insert("name".into(), name);
            }
            if let Some(ty) = type_field.and_then(|f| hit.get(f)).cloned() {
                entry.insert("type".into(), ty);
            }
            // A place name is the single most useful discriminator a human
            // reader has; carried when the hit has one, under either spelling
            // the catalogue's sources use.
            for place_key in ["plaats", "city"] {
                if let Some(place) = hit.get(place_key).cloned() {
                    entry.insert("plaats".into(), place);
                    break;
                }
            }
            Value::Object(entry)
        })
        .collect()
}

/// Lowercase, strip punctuation, collapse whitespace, and split off a
/// trailing Dutch legal-form suffix (B.V., N.V., V.O.F., C.V.) as its own
/// value — `("mollie", Some("bv"))` for "Mollie B.V.", `("mollie", None)` for
/// bare "Mollie". Conservative on purpose: only one, unambiguous legal-form
/// token is ever removed, and only from the END of the name.
///
/// The suffix is returned rather than silently discarded because two
/// registrations can share a base name under different legal forms — "Jansen
/// B.V." and "Jansen V.O.F." are different companies with different
/// kvkNummers — and a caller who typed the legal form said something specific
/// that must not be matched away. See [`choose_hit`].
pub fn normalize_name(name: &str) -> (String, Option<&'static str>) {
    let lowered: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let mut tokens: Vec<&str> = lowered.split_whitespace().collect();
    // Punctuation splits "B.V." into ["b", "v"], so a suffix is matched as
    // the CONCATENATION of the trailing one-to-three tokens.
    const LEGAL_SUFFIXES: &[&str] = &["bv", "nv", "vof", "cv"];
    let mut suffix = None;
    for take in 1..=3.min(tokens.len()) {
        let joined: String = tokens[tokens.len() - take..].concat();
        if let Some(canonical) = LEGAL_SUFFIXES.iter().find(|s| **s == joined) {
            tokens.truncate(tokens.len() - take);
            suffix = Some(*canonical);
            break;
        }
    }
    (tokens.join(" "), suffix)
}

/// Whether the declared active-flag field marks this hit inactive. A hit
/// without the field — or with an unrecognized value — counts as active:
/// the flag narrows, it never excludes on absence.
fn is_inactive(hit: &Value, active_field: Option<&str>) -> bool {
    let Some(field) = active_field else {
        return false;
    };
    match hit.get(field) {
        Some(Value::Bool(active)) => !active,
        Some(Value::String(s)) => {
            let s = s.trim().to_lowercase();
            s == "n" || s == "nee" || s == "no" || s == "false" || s == "inactief"
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn zoeken_body() -> Value {
        json!({
            "pagina": 1,
            "totaal": 3,
            "resultaten": [
                {"kvkNummer": "30204462", "naam": "Mollie B.V.", "type": "rechtspersoon", "plaats": "Amsterdam"},
                {"kvkNummer": "30204462", "naam": "Mollie B.V.", "type": "hoofdvestiging", "plaats": "Amsterdam", "vestigingsnummer": "000018296932"},
                {"kvkNummer": "87654321", "naam": "Mollie Catering", "type": "hoofdvestiging", "plaats": "Utrecht"}
            ]
        })
    }

    #[test]
    fn extract_walks_arrays_and_objects() {
        let body = zoeken_body();
        assert_eq!(
            extract("resultaten[0].kvkNummer", &body),
            Some(&json!("30204462"))
        );
        assert_eq!(
            extract("resultaten[2].plaats", &body),
            Some(&json!("Utrecht"))
        );
        assert_eq!(extract("resultaten[9].kvkNummer", &body), None);
        assert_eq!(extract("nope.kvkNummer", &body), None);
        assert_eq!(
            extract("resultaten", &body)
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(3)
        );
    }

    #[test]
    fn a_declared_digit_shape_predicts_whether_the_lookup_runs() {
        let none = json!({});
        // Eight digits in the sentence: the identifier is probably already there.
        assert!(shape_is_present(
            Some("digits:8"),
            "kvkNummer",
            "basisprofiel van 30204462",
            &none
        ));
        // A company name only: the lookup line shows.
        assert!(!shape_is_present(
            Some("digits:8"),
            "kvkNummer",
            "hoeveel werknemers heeft Mollie",
            &none
        ));
        // A longer run is not an eight-digit number.
        assert!(!shape_is_present(
            Some("digits:8"),
            "kvkNummer",
            "bel 0612345678 voor info",
            &none
        ));
        assert!(shape_is_present(
            Some("digits:12"),
            "vestigingsnummer",
            "vestiging 000018296932",
            &none
        ));
    }

    #[test]
    fn an_explicit_input_outranks_the_shape_guess_when_it_could_be_the_value() {
        // The caller answered the question; no line, whatever the job says.
        assert!(shape_is_present(
            Some("digits:8"),
            "kvkNummer",
            "profiel van Mollie",
            &json!({"kvkNummer": "30204462"})
        ));
        // An empty or null input is not an answer.
        assert!(!shape_is_present(
            Some("digits:8"),
            "kvkNummer",
            "profiel van Mollie",
            &json!({"kvkNummer": null})
        ));
    }

    #[test]
    fn a_company_name_in_the_number_field_is_not_an_answer() {
        // The job this whole mechanism exists for. One candidate value in the
        // sentence, one field to put it in, so the NAME lands in `kvkNummer`.
        // Reading that as an answer hid the lookup line and quoted the profile
        // call alone — then KVK refused the call with IPD0004.
        assert!(!shape_is_present(
            Some("digits:8"),
            "kvkNummer",
            "ik wil de kvk informatie van Mollie BV",
            &json!({"kvkNummer": "Mollie BV"})
        ));
        // And the line it draws is the whole point: 0.02 + 0.05 = 0.07.
        let rows = [kvk_pre_step_row()];
        let steps = pre_steps_for(
            &rows,
            Some(uuid::Uuid::nil()),
            None,
            "ik wil de kvk informatie van Mollie BV",
            &json!({"kvkNummer": "Mollie BV"}),
        );
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0]["shape"], json!("digits:8"));
        assert!((combined_buyer_price_usd(0.05, &steps) - 0.07).abs() < 1e-9);
    }

    #[test]
    fn a_shape_judges_a_value_one_way_only() {
        // Eight digits, however they were written down.
        assert!(shape_fits(Some("digits:8"), &json!("30204462")));
        assert!(shape_fits(Some("digits:8"), &json!(30204462)));
        assert!(shape_fits(Some("digits:8"), &json!(" 30204462 ")));
        // Anything that cannot be the identifier.
        assert!(!shape_fits(Some("digits:8"), &json!("Mollie BV")));
        assert!(!shape_fits(Some("digits:8"), &json!("<UNKNOWN>")));
        assert!(!shape_fits(Some("digits:8"), &json!("302044620")));
        // A declaration this function cannot read never rejects anything: an
        // unknown vocabulary, no declaration at all, or a value of a type the
        // shape says nothing about.
        assert!(shape_fits(Some("uuid"), &json!("Mollie BV")));
        assert!(shape_fits(None, &json!("Mollie BV")));
        assert!(shape_fits(Some("digits:8"), &json!(["30204462"])));
    }

    #[test]
    fn an_unknown_shape_hides_the_line_rather_than_promising_a_step() {
        assert!(shape_is_present(
            Some("uuid"),
            "id",
            "something",
            &json!({})
        ));
        // No declaration at all means the lookup is assumed necessary.
        assert!(!shape_is_present(None, "id", "something", &json!({})));
    }

    fn kvk_pre_step_row() -> crate::db::PreStepRow {
        crate::db::PreStepRow {
            target_endpoint_id: Some(uuid::Uuid::nil()),
            target_capability: None,
            target_param: "kvkNummer".into(),
            target_shape: Some("digits:8".into()),
            label: "KVK company lookup".into(),
            resolver_api_slug: "kvk".into(),
            resolver_method: "GET".into(),
            resolver_path: "/api/v2/zoeken".into(),
            resolver_price_usd: 0.017391,
            resolver_hosted_externally: false,
        }
    }

    #[test]
    fn a_named_company_draws_the_lookup_line_at_its_buyer_price() {
        let rows = [kvk_pre_step_row()];
        let steps = pre_steps_for(
            &rows,
            Some(uuid::Uuid::nil()),
            None,
            "hoeveel werknemers heeft Mollie",
            &json!({}),
        );
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0]["label"], json!("KVK company lookup"));
        // The grey line shows what the buyer pays, never the list price.
        assert!((steps[0]["buyer_price_usd"].as_f64().unwrap() - 0.02).abs() < 1e-9);
        // 0.02 lookup + 0.07 profile = the 0.09 on the confirm button.
        assert!((combined_buyer_price_usd(0.07, &steps) - 0.09).abs() < 1e-9);
    }

    #[test]
    fn a_job_that_already_names_the_number_draws_no_line() {
        let rows = [kvk_pre_step_row()];
        let steps = pre_steps_for(
            &rows,
            Some(uuid::Uuid::nil()),
            None,
            "basisprofiel van 30204462",
            &json!({}),
        );
        assert!(steps.is_empty());
        assert!((combined_buyer_price_usd(0.07, &steps) - 0.07).abs() < 1e-9);
    }

    #[test]
    fn a_row_for_a_different_endpoint_is_not_borrowed() {
        let rows = [kvk_pre_step_row()];
        let steps = pre_steps_for(
            &rows,
            Some(uuid::Uuid::from_u128(7)),
            None,
            "hoeveel werknemers heeft Mollie",
            &json!({}),
        );
        assert!(steps.is_empty());
    }

    #[test]
    fn a_capability_level_row_matches_by_capability() {
        let mut row = kvk_pre_step_row();
        row.target_endpoint_id = None;
        row.target_capability = Some("company.enrich".into());
        let steps = pre_steps_for(
            &[row],
            Some(uuid::Uuid::from_u128(7)),
            Some("company.enrich"),
            "enrich Mollie",
            &json!({}),
        );
        assert_eq!(steps.len(), 1);
    }

    #[test]
    fn zero_hits_is_empty_and_one_hit_is_chosen() {
        assert_eq!(choose_hit(&[], "mollie", None), HitChoice::Empty);
        let one = [json!({"naam": "X"})];
        assert_eq!(choose_hit(&one, "anything", None), HitChoice::Chosen(0));
    }

    #[test]
    fn multi_hit_without_match_fields_is_ambiguous() {
        let hits = zoeken_body()["resultaten"].as_array().unwrap().clone();
        assert_eq!(choose_hit(&hits, "Mollie", None), HitChoice::Ambiguous);
    }

    #[test]
    fn hits_that_carry_drops_a_legal_entity_when_extracting_a_branch_number() {
        // The exact bug the reviewer found: a rechtspersoon hit ranks first in
        // KVK's own response shape and matches on name, but has no
        // vestigingsnummer at all. Without filtering, choose_hit would pick it
        // and extraction would fail — a paid search reported as "not found"
        // when the branch number was one index away.
        let hits = zoeken_body()["resultaten"].as_array().unwrap().clone();
        assert_eq!(hits[0]["type"], json!("rechtspersoon"));
        assert!(hits[0].get("vestigingsnummer").is_none());

        let carrying = hits_that_carry(hits, "vestigingsnummer");
        assert_eq!(carrying.len(), 1);
        assert!(carrying.iter().all(|h| h.get("vestigingsnummer").is_some()));

        // Even with the (wrong-for-this-param) rechtspersoon-first preference,
        // the filtered set no longer contains a rechtspersoon to prefer.
        let fields = json!({
            "name": "naam", "type": "type",
            "preferred_types": ["rechtspersoon", "hoofdvestiging"]
        });
        match choose_hit(&carrying, "Mollie", Some(&fields)) {
            HitChoice::Chosen(i) => assert!(carrying[i].get("vestigingsnummer").is_some()),
            other => panic!("expected a defensible branch hit, got {other:?}"),
        }
    }

    #[test]
    fn hits_that_carry_is_a_no_op_when_every_hit_has_the_value() {
        let hits = vec![json!({"kvkNummer": "1"}), json!({"kvkNummer": "2"})];
        assert_eq!(hits_that_carry(hits.clone(), "kvkNummer"), hits);
    }

    #[test]
    fn exact_name_plus_preferred_type_singles_out_a_winner() {
        // "Mollie" normalizes onto "Mollie B.V." (legal suffix stripped) for
        // two hits; the declared type preference (rechtspersoon first) breaks
        // the tie.
        let hits = zoeken_body()["resultaten"].as_array().unwrap().clone();
        let fields = json!({
            "name": "naam", "type": "type",
            "preferred_types": ["rechtspersoon", "hoofdvestiging"]
        });
        assert_eq!(
            choose_hit(&hits, "Mollie", Some(&fields)),
            HitChoice::Chosen(0)
        );
    }

    #[test]
    fn no_exact_name_match_stays_ambiguous_rather_than_guessing() {
        // Neither registration is exactly named "Moll" — a prefix hit is not
        // an identity. For a paid authoritative source a wrong identity is
        // worse than a confirmation round.
        let hits = zoeken_body()["resultaten"].as_array().unwrap().clone();
        let fields = json!({"name": "naam", "type": "type", "preferred_types": ["rechtspersoon"]});
        assert_eq!(
            choose_hit(&hits, "Moll", Some(&fields)),
            HitChoice::Ambiguous
        );
    }

    #[test]
    fn an_address_declaration_can_accept_one_unique_token_complete_hit() {
        let hits = vec![
            json!({"weergavenaam":"Coolsingel 40, 3011AD Rotterdam","type":"adres"}),
            json!({"weergavenaam":"Coolsingel 93A-03, 3012AE Rotterdam","type":"adres"}),
        ];
        let fields = json!({
            "name":"weergavenaam",
            "type":"type",
            "preferred_types":["adres"],
            "contains_searched_tokens":true
        });
        assert_eq!(
            choose_hit(&hits, "Coolsingel 40 Rotterdam", Some(&fields)),
            HitChoice::Chosen(0)
        );
        assert_eq!(
            choose_hit(&hits, "Coolsingel Rotterdam", Some(&fields)),
            HitChoice::Ambiguous
        );
    }

    #[test]
    fn inactive_registrations_are_excluded_from_the_exact_match() {
        let hits = vec![
            json!({"naam": "Shell B.V.", "type": "rechtspersoon", "actief": "N"}),
            json!({"naam": "Shell B.V.", "type": "rechtspersoon", "actief": "J"}),
        ];
        let fields = json!({"name": "naam", "active": "actief", "type": "type"});
        assert_eq!(
            choose_hit(&hits, "Shell", Some(&fields)),
            HitChoice::Chosen(1)
        );
    }

    #[test]
    fn a_tie_within_the_preferred_type_stays_ambiguous() {
        let hits = vec![
            json!({"naam": "Shell", "type": "rechtspersoon"}),
            json!({"naam": "Shell", "type": "rechtspersoon"}),
        ];
        let fields = json!({"name": "naam", "type": "type", "preferred_types": ["rechtspersoon"]});
        assert_eq!(
            choose_hit(&hits, "Shell", Some(&fields)),
            HitChoice::Ambiguous
        );
    }

    #[test]
    fn candidates_carry_value_name_type_and_place() {
        let hits = zoeken_body()["resultaten"].as_array().unwrap().clone();
        let fields = json!({"name": "naam", "type": "type"});
        let candidates = candidate_summaries(&hits, "kvkNummer", Some(&fields));
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0]["value"], json!("30204462"));
        assert_eq!(candidates[0]["name"], json!("Mollie B.V."));
        assert_eq!(candidates[2]["plaats"], json!("Utrecht"));
    }

    #[test]
    fn normalization_splits_off_the_legal_suffix_and_the_punctuation() {
        assert_eq!(
            normalize_name("Mollie B.V."),
            ("mollie".to_string(), Some("bv"))
        );
        assert_eq!(
            normalize_name("KONINKLIJKE PHILIPS N.V."),
            ("koninklijke philips".to_string(), Some("nv"))
        );
        assert_eq!(
            normalize_name("Van der Berg V.O.F."),
            ("van der berg".to_string(), Some("vof"))
        );
        // "Holding" is a real name component, never stripped.
        assert_eq!(
            normalize_name("Shell Holding B.V."),
            ("shell holding".to_string(), Some("bv"))
        );
        // No legal form at all: base only, suffix None.
        assert_eq!(normalize_name("Mollie"), ("mollie".to_string(), None));
    }

    #[test]
    fn a_searched_legal_form_is_not_matched_away_onto_a_different_one() {
        // "Jansen B.V." and "Jansen V.O.F." are DIFFERENT registrations that
        // happen to share a base name. Searching "Jansen V.O.F." must resolve
        // the V.O.F., never silently fall back to the B.V. because both strip
        // to "jansen".
        let hits = vec![
            json!({"naam": "Jansen B.V.", "type": "rechtspersoon"}),
            json!({"naam": "Jansen V.O.F.", "type": "hoofdvestiging"}),
        ];
        let fields = json!({"name": "naam", "type": "type", "preferred_types": ["rechtspersoon"]});
        assert_eq!(
            choose_hit(&hits, "Jansen V.O.F.", Some(&fields)),
            HitChoice::Chosen(1)
        );
    }

    #[test]
    fn a_bare_search_with_no_legal_form_still_matches_either_one() {
        // Today's behaviour, preserved: "Mollie" (no stated legal form) still
        // finds "Mollie B.V." — the suffix constraint only activates when the
        // SEARCHED name states one.
        let hits = vec![json!({"naam": "Mollie B.V.", "type": "rechtspersoon"})];
        let fields = json!({"name": "naam", "type": "type"});
        assert_eq!(
            choose_hit(&hits, "Mollie", Some(&fields)),
            HitChoice::Chosen(0)
        );
    }
}
