//! Dataset discovery from Eurostat's own documented table of contents.
//! The public route keeps its identifier/title envelope; numeric observations
//! still come from the Statistics API. No dependency on data.europa.eu search.
use super::{integer, required, Prepared};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use url::Url;

pub const CATALOGUE_URL: &str =
    "https://ec.europa.eu/eurostat/api/dissemination/catalogue/toc/txt?lang=en";

pub fn prepare(method: &str, original: &str) -> Result<Option<Prepared>, String> {
    let url = Url::parse(original).map_err(|_| "Invalid Eurostat URL")?;
    if url.path() != "/api/hub/search/search" {
        return Ok(None); // All existing data, structure and catalogue routes pass through.
    }
    if method != "GET"
        || url.scheme() != "https"
        || url.host_str() != Some("data.europa.eu")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("Unsupported Eurostat search route".into());
    }
    let mut params = BTreeMap::new();
    for (key, value) in url.query_pairs() {
        if ![
            "q",
            "page",
            "limit",
            "filter",
            "facets",
            "includes",
            "aggregation",
        ]
        .contains(&key.as_ref())
            || params
                .insert(key.into_owned(), value.into_owned())
                .is_some()
        {
            return Err("Unsupported or repeated Eurostat search parameter".into());
        }
    }
    let query = required(&params, "q")?;
    if query.chars().count() > 200
        || query.chars().any(char::is_control)
        || !query.chars().any(char::is_alphanumeric)
    {
        return Err("Use a search term of at most 200 characters".into());
    }
    integer(&params, "page", 0, 0, 100_000)?;
    integer(&params, "limit", 5, 1, 100)?;
    for (key, expected) in [
        ("filter", "dataset"),
        ("includes", "identifier,title.en"),
        ("aggregation", "false"),
    ] {
        if params.get(key).is_some_and(|value| value != expected) {
            return Err(format!("Unsupported Eurostat {key}"));
        }
    }
    if let Some(facets) = params.get("facets") {
        if serde_json::from_str::<Value>(facets).ok() != Some(json!({"catalog":["estat"]})) {
            return Err("Only the Eurostat catalogue is supported".into());
        }
    }
    Ok(Some(Prepared {
        url: CATALOGUE_URL.into(),
        json_body: None,
        source: "eurostat-catalogue".into(),
        params,
    }))
}

fn cell(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.starts_with('"') {
        let inner = value
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .ok_or("Invalid Eurostat catalogue cell")?;
        Ok(inner.replace("\"\"", "\"").trim().to_owned())
    } else {
        Ok(value.to_owned())
    }
}

fn words(text: &str) -> std::collections::BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|word| word.len() > 1 && !word.chars().all(char::is_numeric))
        .filter(|word| {
            ![
                "the", "for", "in", "of", "and", "both", "by", "use", "from", "with", "eurostat",
            ]
            .contains(word)
        })
        .map(|word| {
            match word {
                "ages" => "age",
                "sexes" => "sex",
                "rates" => "rate",
                _ => word,
            }
            .to_owned()
        })
        .collect()
}

pub fn normalize(request: &Prepared, bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("Eurostat catalogue exceeds 8 MiB".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "Invalid Eurostat catalogue encoding")?;
    let mut lines = text.trim_start_matches('\u{feff}').lines();
    let header: Vec<_> = lines
        .next()
        .ok_or("Empty Eurostat catalogue")?
        .split('\t')
        .map(cell)
        .collect::<Result<_, _>>()?;
    if header
        != [
            "title",
            "code",
            "type",
            "last update of data",
            "last table structure change",
            "data start",
            "data end",
            "values",
        ]
    {
        return Err("Unexpected Eurostat catalogue columns".into());
    }
    // A dataset can occur in several branches of the navigation tree.
    let mut datasets = BTreeMap::new();
    for line in lines.filter(|line| !line.trim().is_empty()) {
        let row: Vec<_> = line.split('\t').map(cell).collect::<Result<_, _>>()?;
        if row.len() != 8 {
            return Err("Incomplete Eurostat catalogue row".into());
        }
        if row[2] == "folder" {
            continue;
        }
        if !matches!(row[2].as_str(), "dataset" | "table")
            || row[0].is_empty()
            || row[1].is_empty()
            || !row[1]
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err("Invalid Eurostat dataset record".into());
        }
        datasets.entry(row[1].clone()).or_insert(row);
    }
    if datasets.is_empty() {
        return Err("Eurostat catalogue contains no datasets".into());
    }
    let query = required(&request.params, "q")?.trim().to_lowercase();
    // Natural-language questions include country/year/population constraints
    // which do not occur in dataset titles. Rank topical matches rather than
    // requiring every word to occur; never imply observation-level coverage.
    let terms = words(&query);
    let indexed: Vec<_> = datasets
        .values()
        .map(|row| (row, words(&format!("{} {}", row[0], row[1]))))
        .collect();
    let weights: BTreeMap<_, _> = terms
        .iter()
        .map(|term| {
            let frequency = indexed
                .iter()
                .filter(|(_, words)| words.contains(term))
                .count();
            (
                term,
                ((indexed.len() + 1) as f64 / (frequency + 1) as f64).ln() + 1.0,
            )
        })
        .collect();
    let mut matches: Vec<_> = datasets
        .values()
        .zip(indexed.iter())
        .filter_map(|(row, (_, words))| {
            let score = weights
                .iter()
                .filter(|(term, _)| words.contains(**term))
                .map(|(_, weight)| weight)
                .sum::<f64>();
            (score > 0.0).then_some((row, score / (words.len().max(1) as f64).sqrt()))
        })
        .collect();
    matches.sort_by(|(a, a_score), (b, b_score)| {
        (a[1].to_lowercase() != query)
            .cmp(&(b[1].to_lowercase() != query))
            .then_with(|| (a[0].to_lowercase() != query).cmp(&(b[0].to_lowercase() != query)))
            .then_with(|| b_score.total_cmp(a_score))
            .then_with(|| a[1].cmp(&b[1]))
    });
    let count = matches.len();
    let limit = integer(&request.params, "limit", 5, 1, 100)? as usize;
    let page = integer(&request.params, "page", 0, 0, 100_000)? as usize;
    let rows: Vec<_> = matches
        .into_iter()
        .skip(page * limit)
        .take(limit)
        .map(|(row, _)| {
            json!({
                "index":"dataset", "identifier":[row[1]], "title":{"en":row[0]},
                "type":row[2], "data_start":row[5], "data_end":row[6]
            })
        })
        .collect();
    Ok(
        json!({"result":{"index":"dataset","count":count,"results":rows,"facets":[]},
        "source_url":CATALOGUE_URL,"query":request.params,
        "coverage":"Ranked keyword matches in Eurostat dataset titles or codes, deduplicated by code. Includes datasets and predefined tables. Matches do not confirm country, year or population coverage; statistical observations require a dataset query."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    const HEADER: &str = "\"title\"\t\"code\"\t\"type\"\t\"last update of data\"\t\"last table structure change\"\t\"data start\"\t\"data end\"\t\"values\"\n";
    fn request(query: &str) -> Prepared {
        prepare(
            "GET",
            &format!("https://data.europa.eu/api/hub/search/search?q={query}"),
        )
        .unwrap()
        .unwrap()
    }
    fn fixture() -> String {
        format!("{HEADER}\"Labour\"\t\"lab\"\t\"folder\"\t\" \"\t\" \"\t\" \"\t\" \"\t\n\"  Unemployment by sex and age - annual data\"\t\"une_rt_a\"\t\"dataset\"\t\"10.09.2026\"\t\"12.03.2026\"\t\"2003\"\t\"2025\"\t39708\n\"Unemployment by sex and age - annual data\"\t\"une_rt_a\"\t\"dataset\"\t\"10.09.2026\"\t\"12.03.2026\"\t\"2003\"\t\"2025\"\t39708\n\"Unemployment rate\"\t\"tps00203\"\t\"table\"\t\"10.09.2026\"\t\"12.03.2026\"\t\"2003\"\t\"2025\"\t25\n")
    }
    #[test]
    fn eurostat_catalogue_search_deduplicates_and_paginates() {
        let mut p = request("unemployment");
        p.params.insert("limit".into(), "1".into());
        let data = normalize(&p, fixture().as_bytes()).unwrap();
        assert_eq!(data["result"]["count"], 2);
        assert_eq!(data["result"]["results"][0]["identifier"][0], "tps00203");
        p.params.insert("page".into(), "1".into());
        let data = normalize(&p, fixture().as_bytes()).unwrap();
        assert_eq!(data["result"]["results"][0]["identifier"][0], "une_rt_a");
        assert_eq!(data["result"]["results"][0]["data_end"], "2025");
        assert_eq!(data["source_url"], CATALOGUE_URL);
    }
    #[test]
    fn eurostat_catalogue_no_match_differs_from_malformed_source() {
        assert_eq!(
            normalize(&request("no-such-topic"), fixture().as_bytes()).unwrap()["result"]["count"],
            0
        );
        assert!(normalize(&request("unemployment"), b"<html>403 ERROR</html>").is_err());
        assert!(normalize(
            &request("unemployment"),
            format!("{HEADER}broken\n").as_bytes()
        )
        .is_err());
        assert_eq!(
            normalize(&request("UNE_RT_A"), fixture().as_bytes()).unwrap()["result"]["count"],
            1
        );
    }
    #[test]
    fn eurostat_catalogue_accepts_the_live_planners_natural_language_query() {
        let p = request("Annual%20average%20unemployment%20rate%20for%20the%20Netherlands%20in%202024%2C%20ages%2015%E2%80%9374%2C%20both%20sexes.");
        let data = normalize(&p, fixture().as_bytes()).unwrap();
        assert_eq!(data["result"]["results"][0]["identifier"][0], "une_rt_a");
        assert!(data["coverage"]
            .as_str()
            .unwrap()
            .contains("do not confirm country"));
    }
    #[test]
    fn eurostat_catalogue_validates_route_and_filters() {
        assert!(prepare(
            "GET",
            "https://ec.europa.eu/eurostat/api/dissemination/statistics/1.0/data/une_rt_a?geo=NL"
        )
        .unwrap()
        .is_none());
        for url in [
            "https://example.com/api/hub/search/search?q=x",
            "https://data.europa.eu/api/hub/search/search?q=x&q=y",
            "https://data.europa.eu/api/hub/search/search?q=x&limit=0",
            "https://data.europa.eu/api/hub/search/search?q=x&facets=%7B%7D",
        ] {
            assert!(prepare("GET", url).is_err());
        }
    }
}
