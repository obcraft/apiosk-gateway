//! Capture bounded public evidence during execution; never on a report download.
//! Only fixed PDOK map services and source-linked official PDF hosts are contacted.
use anyhow::{ensure, Context};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::Utc;
use futures_util::StreamExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{io::Cursor, time::Duration};
use url::Url;

const MAX_FILE: usize = 4 * 1024 * 1024;
const MAX_MAP: usize = 1024 * 1024;
pub const KEY: &str = "report_assets";

pub fn evidence_pointer(pointer: &str) -> bool {
    pointer.starts_with("/data/") || pointer.strip_prefix("/report_assets/assets/")
        .and_then(|s|s.strip_suffix("/extracted_text"))
        .is_some_and(|index|!index.is_empty() && index.bytes().all(|c|c.is_ascii_digit()))
}

/// A separate, time-limited process reads a bounded excerpt. Never execute
/// document instructions, follow embedded links, or interpret a scanned page as text.
async fn pdf_text(bytes: &[u8]) -> anyhow::Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut child=tokio::process::Command::new("pdftotext")
        .args(["-f","1","-l","12","-layout","-enc","UTF-8","-","-"])
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null()).kill_on_drop(true).spawn()?;
    let mut input=child.stdin.take().context("missing document input")?;
    let output=child.stdout.take().context("missing document output")?;
    let (write,read)=tokio::time::timeout(Duration::from_secs(6),async {
        tokio::join!(async move {input.write_all(bytes).await?;input.shutdown().await?;drop(input);Ok::<_,std::io::Error>(())},async {
            let mut text=Vec::new();output.take(24001).read_to_end(&mut text).await?;
            Ok::<_,std::io::Error>(text)
        })
    }).await?;
    let mut raw=read?;
    if raw.len()>24000 {
        raw.truncate(24000);
        let _=child.kill().await;
    } else {
        write?;
        ensure!(tokio::time::timeout(Duration::from_secs(2),child.wait()).await??.success(),"PDF text extraction failed");
    }
    let text=String::from_utf8_lossy(&raw).trim().to_owned();
    ensure!(!text.is_empty(),"PDF has no extractable text");
    Ok(text)
}

pub fn requested(question: &str) -> bool {
    let q = question.to_lowercase();
    [
        "report",
        "rapport",
        "pdf",
        "dossier",
        "map",
        "kaart",
        "plattegrond",
    ]
    .iter()
    .any(|s| q.contains(s))
}

pub fn public_document(document: &Value) -> Value {
    let mut v = document.clone();
    if let Some(assets) = v[KEY]["assets"].as_array_mut() {
        for asset in assets {
            if let Some(a) = asset.as_object_mut() {
                a.remove("data_base64");
            }
        }
    }
    v
}

fn point(s: &str) -> Option<(f64, f64)> {
    let values: Vec<_> = s
        .trim()
        .strip_prefix("POINT(")?
        .strip_suffix(')')?
        .split_whitespace()
        .collect();
    if values.len() != 2 {
        return None;
    }
    let (lon, lat): (f64, f64) = (values[0].parse().ok()?, values[1].parse().ok()?);
    if !(3.0..=7.5).contains(&lon) || !(50.5..=54.0).contains(&lat) {
        return None;
    }
    Some((lon, lat))
}

/// A unique, exact address match is required. Do not silently map the first candidate.
pub fn map_location(document: &Value) -> Option<(String, f64, f64)> {
    if document["source"]["provider"] != "pdok" || document["coverage"] == "partial" {
        return None;
    }
    let data = &document["data"];
    if !matches!(
        data["route"].as_str(),
        Some("company-address" | "bag-address")
    ) {
        return None;
    }
    let items = data["items"].as_array()?;
    if items.len() != 1 || data["coverage"]["complete"] != true {
        return None;
    }
    let row = &items[0];
    let (lon, lat) = point(row["centroid_wgs84"].as_str()?)?;
    Some((row["address"].as_str()?.to_owned(), lon, lat))
}

pub fn map_urls(lon: f64, lat: f64) -> Vec<(&'static str, Url)> {
    let x = lon.to_radians() * 6378137.0;
    let y = (std::f64::consts::FRAC_PI_4 + lat.to_radians() / 2.0)
        .tan()
        .ln()
        * 6378137.0;
    // Scale adjusted to latitude: approximately 800 by 500 ground metres.
    let half = 400.0 / lat.to_radians().cos();
    let bbox = format!(
        "{},{},{},{}",
        x - half,
        y - half * 0.625,
        x + half,
        y + half * 0.625
    );
    [
        (
            "Buildings",
            "https://service.pdok.nl/lv/bag/wms/v2_0",
            "pand",
        ),
        (
            "Parcels",
            "https://service.pdok.nl/kadaster/kadastralekaart/wms/v5_0",
            "Kadastralekaart",
        ),
    ]
    .into_iter()
    .map(|(label, base, layer)| {
        let mut url = Url::parse(base).unwrap();
        url.query_pairs_mut().extend_pairs([
            ("SERVICE", "WMS"),
            ("VERSION", "1.1.1"),
            ("REQUEST", "GetMap"),
            ("LAYERS", layer),
            ("STYLES", ""),
            ("SRS", "EPSG:3857"),
            ("BBOX", &bbox),
            ("WIDTH", "1000"),
            ("HEIGHT", "625"),
            ("FORMAT", "image/png"),
            ("TRANSPARENT", "FALSE"),
        ]);
        (label, url)
    })
    .collect()
}

fn official_pdf(raw: &str) -> Option<Url> {
    let u = Url::parse(raw).ok()?;
    if u.scheme() != "https"
        || !u.username().is_empty()
        || u.password().is_some()
        || u.port().is_some()
        || !u.path().to_lowercase().ends_with(".pdf")
    {
        return None;
    }
    let host = u.host_str()?;
    [
        "asml.com",
        "kvk.nl",
        "kadaster.nl",
        "pdok.nl",
        "bodemloket.nl",
        "officielebekendmakingen.nl",
    ]
    .iter()
    .any(|base| host == *base || host.ends_with(&format!(".{base}")))
    .then_some(u)
}

/// Reuse the exact reviewed map layer and extent that produced a source result.
/// Do not turn an arbitrary source URL into a fetch target.
fn context_map(document: &Value) -> Option<(&'static str, Url)> {
    let data = &document["data"];
    let original = Url::parse(data["source_url"].as_str()?).ok()?;
    if original.scheme() != "https" || !original.username().is_empty()
        || original.password().is_some() || original.port().is_some() { return None; }
    let name = match (document["source"]["provider"].as_str()?, original.host_str()?, original.path()) {
        ("bodemloket", "gis.gdngeoservices.nl", "/standalone/services/blk_gdn/lks_blk_rd_v1/MapServer/WMSServer") => "Soil information coverage",
        ("pdok-ruimtelijke-plannen", "service.pdok.nl", "/kadaster/ruimtelijke-plannen/wms/v1_0") => "Spatial planning context",
        _ => return None,
    };
    let q: std::collections::BTreeMap<_,_> = original.query_pairs().map(|(k,v)|(k.to_string(),v.to_string())).collect();
    if q.get("REQUEST")?.to_lowercase() != "getfeatureinfo" || q.get("CRS")? != "EPSG:28992" { return None; }
    let bbox = q.get("BBOX")?;
    let n: Vec<f64> = bbox.split(',').map(str::parse).collect::<Result<_,_>>().ok()?;
    if n.len()!=4 || !n.iter().all(|v|v.is_finite()) || !(0.0..=300000.0).contains(&n[0])
        || !(280000.0..=630000.0).contains(&n[1]) || n[2]<=n[0] || n[3]<=n[1]
        || n[2]>300000.0 || n[3]>630000.0 { return None; }
    let layer = q.get("LAYERS")?;
    if layer != data["data"]["layer"].as_str()? || bbox != data["data"]["bbox"].as_str()? { return None; }
    let mut url = original.clone(); url.set_query(None);
    url.query_pairs_mut().extend_pairs([
        ("SERVICE","WMS"),("VERSION","1.3.0"),("REQUEST","GetMap"),
        ("CRS","EPSG:28992"),("BBOX",bbox),("LAYERS",layer),("STYLES",""),
        ("WIDTH","1000"),("HEIGHT","1000"),("FORMAT","image/png"),("TRANSPARENT","FALSE")
    ]);
    Some((name,url))
}

fn pdf_links(value: &Value, pointer: &str, links: &mut Vec<(String, Url)>) {
    if links.len() >= 2 {
        return;
    }
    match value {
        Value::String(s) => {
            // Search/extraction providers may return document URLs inside Markdown.
            // Only literal URLs in the saved response are candidates; no invented paths.
            for (start, _) in s.match_indices("https://") {
                let rest = &s[start..];
                let end = rest.find(|c: char| c.is_whitespace() || "\"'<>[]()".contains(c)).unwrap_or(rest.len());
                if let Some(u) = official_pdf(rest[..end].trim_end_matches([',', ';'])) {
                    if !links.iter().any(|(_, known)| known == &u) { links.push((pointer.into(), u)); }
                }
                if links.len() >= 2 { break; }
            }
        }
        Value::Object(o) => {
            for (k, v) in o {
                pdf_links(
                    v,
                    &format!("{pointer}/{}", k.replace('~', "~0").replace('/', "~1")),
                    links,
                );
            }
        }
        Value::Array(a) => {
            for (i, v) in a.iter().enumerate() {
                pdf_links(v, &format!("{pointer}/{i}"), links);
            }
        }
        _ => {}
    }
}

async fn download(
    http: &reqwest::Client,
    url: Url,
    limit: usize,
    mime: &str,
) -> anyhow::Result<Vec<u8>> {
    let response = http.get(url).send().await?.error_for_status()?;
    ensure!(response.status().is_success(), "redirects are not followed");
    ensure!(
        response
            .headers()
            .get("content-type")
            .and_then(|s| s.to_str().ok())
            .is_some_and(|s| s.split(';').next().unwrap_or("").trim() == mime),
        "unexpected file type"
    );
    ensure!(
        response.content_length().unwrap_or(0) <= limit as u64,
        "file exceeds capture limit"
    );
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        ensure!(
            bytes.len() + chunk.len() <= limit,
            "file exceeds capture limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    if mime == "image/png" {
        ensure!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "not a PNG");
        let dimensions = image::io::Reader::new(Cursor::new(&bytes))
            .with_guessed_format()?
            .into_dimensions()?;
        ensure!(matches!(dimensions, (1000, 625) | (1000, 1000)), "unexpected map dimensions");
    } else {
        ensure!(bytes.starts_with(b"%PDF-"), "not a PDF");
    }
    Ok(bytes)
}

fn asset(kind: &str, name: &str, url: &Url, pointer: &str, bytes: &[u8], mime: &str) -> Value {
    json!({"kind":kind,"name":name,"source_url":url.as_str(),"evidence_pointer":pointer,
        "retrieved_at":Utc::now(),"sha256":hex::encode(Sha256::digest(bytes)),"mime_type":mime,
        "byte_length":bytes.len(),"data_base64":STANDARD.encode(bytes)})
}

/// Failure is recorded as missing evidence and never converts a paid source success to a retry.
pub async fn capture(document: &Value) -> Value {
    let http = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8))
        .connect_timeout(Duration::from_secs(3))
        .user_agent("Apiosk-Evidence/1.0 (+https://apiosk.com)")
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            return json!({"version":1,"assets":[],"notices":["Evidence capture unavailable."]})
        }
    };
    let mut assets = Vec::new();
    let mut notices = Vec::new();
    if let Some((address, lon, lat)) = map_location(document) {
        let jobs = map_urls(lon,lat).into_iter().map(|(name,url)| {
            let http = &http; let address = &address;
            async move {
                let result = download(http,url.clone(),MAX_MAP,"image/png").await;
                result.map(|bytes| {
                    let mut a = asset("map",name,&url,"/data/items/0/centroid_wgs84",&bytes,"image/png");
                    a["address"] = json!(address); a["center"] = json!([lon,lat]);
                    a["attribution"] = json!("PDOK / Kadaster");
                    a["scope"] = json!("North up. Red cross: registered address point. Approx. 800 x 500 m. Surrounding buildings and parcels are not proof of company ownership. No interior floor plan.");
                    a
                })
            }
        });
        for result in futures_util::future::join_all(jobs).await {
            match result {
                Ok(a) => assets.push(a),
                Err(_) => notices.push(
                    "An official map could not be captured; no substitute image was generated."
                        .to_owned(),
                ),
            }
        }
    } else if document["source"]["provider"] == "pdok" {
        notices
            .push("No single verified address coordinate was available for a location map.".into());
    }
    if let Some((name,url)) = context_map(document) {
        match download(&http,url.clone(),MAX_MAP,"image/png").await {
            Ok(bytes) => {
                let mut a=asset("map",name,&url,"/data/source_url",&bytes,"image/png");
                a["attribution"]=json!(if name.starts_with("Soil") {"Bodemloket / Geological Survey of the Netherlands"} else {"PDOK / Kadaster"});
                a["scope"]=json!("Same layer and map extent as the saved source query. Map-layer coverage is not a complete soil investigation or a current planning permission. See the source record for layer, coordinates and limitations.");
                assets.push(a);
            },
            Err(_)=>notices.push(format!("{name} image could not be captured; the original source result remains in the evidence bundle."))
        }
    }
    let mut links = Vec::new();
    pdf_links(&document["data"], "/data", &mut links);
    for (pointer, url) in links {
        match download(&http,url.clone(),MAX_FILE,"application/pdf").await {
            Ok(bytes) => {
                let mut a=asset("document","Original source document",&url,&pointer,&bytes,"application/pdf");
                match pdf_text(&bytes).await {
                    Ok(text)=>a["extracted_text"]=json!(text),
                    Err(_)=>notices.push(format!("The original PDF was saved but its text could not be read automatically: {url}")),
                }
                a["text_scope"]=json!("Bounded text excerpt: at most the first twelve pages and twenty-four thousand bytes. Not a full-document review or OCR. Tables and units must be read in their original context.");
                assets.push(a);
            },
            Err(_) => notices.push(format!("Original document not captured (size, availability or file type); inspect the source link: {url}"))
        }
    }
    json!({"version":1,"assets":assets,"notices":notices})
}

pub fn bytes(asset: &Value) -> anyhow::Result<Vec<u8>> {
    let encoded = asset["data_base64"]
        .as_str()
        .context("missing saved file")?;
    ensure!(
        encoded.len() <= (MAX_FILE * 4 / 3 + 8),
        "saved file too large"
    );
    let bytes = STANDARD.decode(encoded)?;
    ensure!(
        asset["sha256"] == hex::encode(Sha256::digest(&bytes)),
        "saved file digest mismatch"
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn map_requires_one_verified_dutch_address() {
        let mut d = json!({"source":{"provider":"pdok"},"data":{"route":"company-address","coverage":{"complete":true},"items":[{"address":"Example","centroid_wgs84":"POINT(5.4 51.4)"}]}});
        assert!(map_location(&d).is_some());
        let duplicate = d["data"]["items"][0].clone();
        d["data"]["items"].as_array_mut().unwrap().push(duplicate);
        assert!(map_location(&d).is_none());
        assert!(point("POINT(NaN 51.4)").is_none());
        assert!(point("POINT(0 0)").is_none());
    }
    #[test]
    fn source_files_are_bounded_official_urls_and_hash_checked() {
        for url in [
            "https://asml.com.evil.test/a.pdf",
            "http://asml.com/a.pdf",
            "https://u@asml.com/a.pdf",
            "https://127.0.0.1/a.pdf",
            "https://asml.com:8443/a.pdf",
        ] {
            assert!(official_pdf(url).is_none());
        }
        let mut links=Vec::new();
        pdf_links(&json!({"text":"Read [the report](https://www.asml.com/a.pdf) and https://asml.com.evil.test/fake.pdf"}),"/data",&mut links);
        assert_eq!(links.len(),1);
        assert_eq!(links[0].0,"/data/text");
        let u = official_pdf("https://www.asml.com/a.pdf").unwrap();
        let a = asset(
            "document",
            "test",
            &u,
            "/data/url",
            b"%PDF-test",
            "application/pdf",
        );
        assert_eq!(bytes(&a).unwrap(), b"%PDF-test");
        let mut bad = a.clone();
        bad["sha256"] = json!("bad");
        assert!(bytes(&bad).is_err());
        let d = json!({KEY:{"assets":[a]}});
        assert!(public_document(&d)[KEY]["assets"][0]
            .get("data_base64")
            .is_none());
    }
    fn context_fixture() -> Value {
        json!({"source":{"provider":"bodemloket"},"data":{"source_url":"https://gis.gdngeoservices.nl/standalone/services/blk_gdn/lks_blk_rd_v1/MapServer/WMSServer?REQUEST=GetFeatureInfo&CRS=EPSG:28992&BBOX=120900,487200,121000,487300&LAYERS=Beschikbaarheid_gegevens","data":{"bbox":"120900,487200,121000,487300","layer":"Beschikbaarheid_gegevens"}}})
    }
    #[test]
    fn context_maps_preserve_reviewed_layer_and_extent() {
        let mut d=context_fixture();
        let (_,url)=context_map(&d).unwrap();
        let q:std::collections::BTreeMap<_,_>=url.query_pairs().collect();
        assert_eq!(q.get("BBOX").unwrap(), "120900,487200,121000,487300");
        assert_eq!(q.get("REQUEST").unwrap(), "GetMap");
        d["data"]["data"]["layer"]=json!("other");
        assert!(context_map(&d).is_none());
        d=context_fixture(); d["data"]["source_url"]=json!("https://example.com/a");
        assert!(context_map(&d).is_none());
    }
    #[tokio::test]
    #[ignore = "free public ASML source document capture; no assessment of ASML"]
    async fn live_original_pdf() {
        let source=json!({"data":{"publication":"https://ourbrand.asml.com/asset/6ec62875-6d6a-4ec5-a4d7-43f59ce8e5c5/ASML-Press-Release-Financial-Results-Q1-2026.pdf"}});
        let result=capture(&source).await;
        assert_eq!(result["assets"].as_array().unwrap().len(),1,"{}",result["notices"]);
        let a=&result["assets"][0];
        assert_eq!(a["kind"],"document");
        assert_eq!(a["evidence_pointer"],"/data/publication");
        assert!(bytes(a).unwrap().starts_with(b"%PDF-"));
        assert!(a["extracted_text"].as_str().unwrap().contains("ASML"));
        assert!(evidence_pointer("/report_assets/assets/0/extracted_text"));
        assert!(!evidence_pointer("/report_assets/assets/0/data_base64"));
    }
    #[tokio::test]
    #[ignore = "free live PDOK map capture"]
    async fn live_maps() {
        let d = json!({"source":{"provider":"pdok"},"data":{"route":"company-address","coverage":{"complete":true},"items":[{"address":"Test location (fixture coordinate)","centroid_wgs84":"POINT(5.4 51.4)"}]}});
        let r = capture(&d).await;
        assert_eq!(r["assets"].as_array().unwrap().len(), 2, "{r}");
        for a in r["assets"].as_array().unwrap() {
            assert!(bytes(a).unwrap().starts_with(b"\x89PNG"));
        }
        let context=capture(&context_fixture()).await;
        assert_eq!(context["assets"].as_array().unwrap().len(),1,"{context}");
        let mut plan=context_fixture();
        plan["source"]["provider"]=json!("pdok-ruimtelijke-plannen");
        plan["data"]["data"]["layer"]=json!("enkelbestemming");
        plan["data"]["source_url"]=json!("https://service.pdok.nl/kadaster/ruimtelijke-plannen/wms/v1_0?REQUEST=GetFeatureInfo&CRS=EPSG:28992&BBOX=120900,487200,121000,487300&LAYERS=enkelbestemming");
        let result=capture(&plan).await;
        assert_eq!(result["assets"].as_array().unwrap().len(),1,"{result}");
    }
}
