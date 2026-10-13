//! Lossless intake for explicit "Search <source> for <subject> and show
//! <fields>" clauses. This only recognizes the complete literal syntax;
//! unsupported wording/fields go through the normal model parser. It neither
//! selects an endpoint nor asserts that a named source is available.
use super::{Capability, Deliverable, Meta, ParsedRequest, Subject};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(super) fn parse(request: &str, model: &str) -> Option<ParsedRequest> {
    let mut remaining = request.trim();
    let mut subjects = Vec::new();
    let mut capabilities = Vec::new();
    while !remaining.is_empty() {
        let lower = remaining.to_ascii_lowercase();
        if !lower.starts_with("search ") { return None; }
        let for_at = lower.find(" for ")?;
        let show_at = lower.find(" and show ")?;
        if for_at < 7 || show_at <= for_at + 5 { return None; }
        let source = &remaining[7..for_at];
        if source.len() > 80 || !source.chars().all(|c| c.is_alphanumeric() || " &'-".contains(c)) { return None; }
        let scope = &remaining[for_at + 5..show_at];
        let location_at = scope.to_ascii_lowercase().find(" in ");
        let (name, location) = location_at.map_or((scope, None), |at| (&scope[..at], Some(&scope[at + 4..])));
        if name.trim().is_empty() || name.len() > 200 || location.is_some_and(|v| v.trim().is_empty() || v.len() > 200) { return None; }
        let rest = &remaining[show_at + 10..];
        let end = rest.find(". ").unwrap_or(rest.len());
        let fields = rest[..end].trim_end_matches('.').to_lowercase();
        let fields = fields.strip_suffix(" of the locations returned").unwrap_or(&fields);
        let fields = fields.strip_prefix("its ").or_else(|| fields.strip_prefix("the ")).unwrap_or(fields);
        let mut outputs = Vec::new();
        for field in fields.split(" and ").flat_map(|field| field.split(", ")) {
            let output = match field.trim().trim_start_matches("its ") {
                "short description" => "short_description",
                "description" | "company description" => "description",
                "profile link" | "profile url" => "profile_url",
                "employer rating" | "employer ratings" => "employer_rating",
                "customer rating" | "customer ratings" => "customer_rating",
                "review count" | "review counts" | "number of reviews" => "review_count",
                _ => return None,
            };
            if !outputs.iter().any(|v| v == output) { outputs.push(output.to_owned()); }
        }
        let employer = outputs.iter().any(|v| v == "employer_rating");
        let customer = outputs.iter().any(|v| v == "customer_rating");
        if employer && customer { return None; }
        let (slug, title, description, domain) = if employer {
            ("company.employer.ratings", "Employer ratings", "Search employers by company name and return requested ratings and review counts.", "company")
        } else if customer {
            ("location.business.ratings", "Business customer ratings", "Search businesses by name and location and return requested customer ratings and review counts.", "location")
        } else {
            ("company.description", "Company descriptions and links", "Search companies by name and return requested descriptions and profile links.", "company")
        };
        let id = format!("s{}", subjects.len() + 1);
        let mut identifiers = BTreeMap::from([("company_name".into(), name.into()), ("query".into(), name.into()), ("request".into(), request.into())]);
        let mut inputs = vec!["company_name".into()];
        if let Some(location) = location { identifiers.insert("location".into(), location.into()); inputs.push("location".into()); }
        subjects.push(Subject { id: id.clone(), label: name.into(), kind: "company".into(), identifiers });
        capabilities.push(Capability { slug: slug.into(), name: title.into(), description: description.into(), domain: domain.into(),
            inputs, outputs, subject_id: Some(id), source_hint: Some(source.to_lowercase()), confidence: 1.0, missing: vec![] });
        remaining = if end < rest.len() { rest[end + 1..].trim() } else { "" };
        // Let the existing compound-request path join complete clauses and
        // prove each source's fields before producing one combined price.
        if !remaining.is_empty() { return None; }
    }
    if capabilities.is_empty() { return None; }
    Some(ParsedRequest { language: "en".into(), subjects, capabilities, deliverable: Deliverable { format: "chat".into(), operations: vec![] },
        meta: Meta { model: model.into(), parsed_at: chrono::Utc::now().to_rfc3339(), text_sha256: hex::encode(Sha256::digest(request.as_bytes())) } })
}


#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_sources_fields_and_independent_scopes_survive() {
        let example = parse("Search Crunchbase for Starbucks and show its company description and profile link.", "fixture").unwrap();
        assert_eq!(example.capabilities[0].outputs, ["description", "profile_url"]);
        assert_eq!(example.capabilities[0].source_hint.as_deref(), Some("crunchbase"));
        let question = "Search Crunchbase for Starbucks and show its short description and profile link. Search Glassdoor for Starbucks and show its employer rating and review count. Search Yelp for Starbucks in Seattle and show the customer ratings and review counts of the locations returned.";
        let p = crate::capability_intake::literal_request(question, "fixture").unwrap();
        assert_eq!(p.capabilities.len(), 3);
        assert_eq!(p.capabilities[0].outputs, ["short_description", "profile_url"]);
        assert_eq!(p.capabilities[1].outputs, ["employer_rating", "review_count"]);
        assert_eq!(p.capabilities[2].outputs, ["customer_rating", "review_count"]);
        assert!(p.subjects[..2].iter().all(|s| !s.identifiers.contains_key("location")));
        assert_eq!(p.subjects[2].label, "Starbucks");
        assert_eq!(p.subjects[2].identifiers["location"], "Seattle");
        assert!(p.subjects.iter().all(|s| question.contains(&s.identifiers["request"])));
        let alternative = crate::capability_intake::literal_request("Search Unknown Directory for Fictional Café in Ghent and show customer ratings and number of reviews. Search Other Source for ExampleCo and show description and profile URL.", "fixture").unwrap();
        assert_eq!(alternative.capabilities[0].source_hint.as_deref(), Some("unknown directory"));
        assert_eq!(alternative.subjects[0].label, "Fictional Café");
        assert_eq!(alternative.subjects[0].identifiers["location"], "Ghent");
        assert_eq!(alternative.capabilities[1].source_hint.as_deref(), Some("other source"));
    }
    #[test]
    fn unrecognized_scope_is_never_silently_discarded() {
        for question in [
            "Search Yelp for Starbucks in Seattle and show customer ratings and opening hours.",
            "Search Glassdoor for Starbucks and show employer rating and review count. Also compare with Peet's.",
            "Find pages about Starbucks on Glassdoor",
            "Search Yelp for Starbucks and show its customer rating. Exclude closed locations.",
            "Search Glassdoor for Starbucks and show its employer rating and customer rating.",
        ] { assert!(parse(question, "fixture").is_none(), "{question}"); }
    }
}
