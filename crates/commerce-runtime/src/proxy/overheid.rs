//! Overheid.io distinguishes exact postcodes from postcode-area searches.
use url::Url;

pub fn prepare(slug: &str, method: &str, raw: &str) -> Result<Option<String>, String> {
    if slug != "overheid-io" || method != "GET" {
        return Ok(None);
    }
    let mut url = Url::parse(raw).map_err(|_| "Invalid source URL")?;
    if url.host_str() != Some("api.overheid.io") || url.path() != "/bag" {
        return Ok(None);
    }
    let mut params: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let postcode = params
        .iter()
        .find(|(key, _)| key == "filters[postcode]")
        .map(|(_, value)| {
            value
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .to_ascii_uppercase()
        });
    let area = postcode.as_deref().filter(|value| {
        value.len() == 4 && value.bytes().all(|c| c.is_ascii_digit()) && !value.starts_with('0')
    });
    if let Some(area) = area {
        if params
            .iter()
            .any(|(key, _)| matches!(key.as_str(), "query" | "queryfields" | "queryfields[]"))
        {
            return Err(
                "Use a full postcode when combining a postcode filter with another search.".into(),
            );
        }
        params.retain(|(key, _)| key != "filters[postcode]");
        params.push(("query".into(), format!("{area}*")));
        params.push(("queryfields[]".into(), "postcode".into()));
        if !params.iter().any(|(key, _)| key == "size") {
            params.push(("size".into(), "25".into()));
        }
    } else if let Some(postcode) = postcode {
        for (key, value) in &mut params {
            if key == "filters[postcode]" {
                *value = postcode.clone();
            }
        }
    }
    // This parameter is documented without brackets, but the live endpoint
    // requires the array spelling; the scalar spelling returns HTTP 500.
    for (key, _) in &mut params {
        if key == "queryfields" {
            *key = "queryfields[]".into();
        }
    }
    url.query_pairs_mut().clear().extend_pairs(params);
    Ok((url.as_str() != raw).then(|| url.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn area_search_preserves_paging_and_additional_filters() {
        let raw = "https://api.overheid.io/bag?filters%5Bpostcode%5D=3011&page=2&size=10&filters%5Bhuisnummer%5D=31";
        let corrected = prepare("overheid-io", "GET", raw).unwrap().unwrap();
        let url = Url::parse(&corrected).unwrap();
        let pairs: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs["query"], "3011*");
        assert_eq!(pairs["queryfields[]"], "postcode");
        assert_eq!(pairs["page"], "2");
        assert_eq!(pairs["size"], "10");
        assert_eq!(pairs["filters[huisnummer]"], "31");
        assert!(!pairs.contains_key("filters[postcode]"));
        assert_ne!(raw, corrected); // corrected URL also isolates the old empty cache entry
    }

    #[test]
    fn full_postcodes_remain_exact_and_unrelated_requests_are_untouched() {
        let exact = prepare(
            "overheid-io",
            "GET",
            "https://api.overheid.io/bag?filters%5Bpostcode%5D=3011%20ha",
        )
        .unwrap()
        .unwrap();
        assert!(exact.contains("3011HA"));
        assert!(!exact.contains("query="));
        for (slug, method, url) in [
            ("pdok", "GET", "https://api.overheid.io/bag?postcode=3011"),
            ("overheid-io", "POST", "https://api.overheid.io/bag"),
            (
                "overheid-io",
                "GET",
                "https://api.overheid.io/v3/openkvk?postcode=3011",
            ),
        ] {
            assert_eq!(prepare(slug, method, url).unwrap(), None);
        }
        assert!(prepare(
            "overheid-io",
            "GET",
            "https://api.overheid.io/bag?filters%5Bpostcode%5D=3011&query=Dam"
        )
        .is_err());
        let query = prepare(
            "overheid-io",
            "GET",
            "https://api.overheid.io/bag?query=3011*&queryfields=postcode",
        )
        .unwrap()
        .unwrap();
        assert!(query.contains("queryfields%5B%5D=postcode"));
    }
}
