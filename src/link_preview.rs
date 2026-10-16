//! Optional public-page metadata. No model, source execution, credentials or task state.
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use futures_util::StreamExt;
use scraper::{Html, Selector};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::Cursor,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use url::Url;
use uuid::Uuid;

const HTML_BYTES: usize = 512 * 1024;
const IMAGE_BYTES: usize = 1024 * 1024;
const PREVIEW_BYTES: usize = 96 * 1024;
const WINDOW: Duration = Duration::from_secs(60);
const DEADLINE: Duration = Duration::from_secs(6);

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Preview {
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub site_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_data_url: Option<String>,
}
impl Preview {
    fn empty(url: &str) -> Self {
        Self {
            url: url.into(),
            title: None,
            description: None,
            site_name: None,
            final_url: None,
            image_data_url: None,
        }
    }
}

#[derive(Clone)]
struct Cached {
    preview: Preview,
    until: Instant,
}
pub(crate) struct Service {
    cache: moka::future::Cache<String, Cached>,
    owners: Mutex<HashMap<Uuid, (Instant, u8)>>,
    requests: Arc<Semaphore>,
    dns: Arc<Semaphore>,
    workers: Arc<Semaphore>,
}
impl Service {
    fn new() -> Self {
        Self {
            cache: moka::future::Cache::builder()
                .max_capacity(128)
                .time_to_live(Duration::from_secs(900))
                .build(),
            owners: Mutex::new(HashMap::new()),
            requests: Arc::new(Semaphore::new(8)),
            dns: Arc::new(Semaphore::new(8)),
            workers: Arc::new(Semaphore::new(2)),
        }
    }
    pub(crate) fn shared() -> &'static Self {
        static SERVICE: OnceLock<Service> = OnceLock::new();
        SERVICE.get_or_init(Self::new)
    }
    pub(crate) fn admit(&self, owner: Uuid) -> bool {
        self.admit_at(owner, Instant::now())
    }
    fn admit_at(&self, owner: Uuid, now: Instant) -> bool {
        let Ok(mut owners) = self.owners.lock() else {
            return false;
        };
        owners.retain(|_, (start, _)| now.duration_since(*start) < WINDOW);
        if owners.len() >= 4096 && !owners.contains_key(&owner) {
            return false;
        }
        let (_, count) = owners.entry(owner).or_insert((now, 0));
        if *count >= 60 {
            return false;
        }
        *count += 1;
        true
    }
    pub(crate) async fn preview(&self, original: &str) -> Preview {
        let fallback = Preview::empty(original);
        let Ok(url) = public_url(original) else {
            return fallback;
        };
        let key = hex::encode(Sha256::digest(url.as_str().as_bytes()));
        if let Some(mut cached) = self
            .cache
            .get(&key)
            .await
            .filter(|c| c.until > Instant::now())
        {
            cached.preview.url = original.into();
            return cached.preview;
        }
        let Ok(_permit) = self.requests.clone().try_acquire_owned() else {
            return fallback;
        };
        let deadline = tokio::time::Instant::now() + DEADLINE;
        let result = tokio::time::timeout_at(deadline, self.fetch(url, Kind::Html)).await;
        let Ok(Ok((final_url, bytes))) = result else {
            self.remember(key, fallback.clone(), Duration::from_secs(30))
                .await;
            return fallback;
        };
        // Blocking parsing/decoding owns its permit until it really stops, even
        // if the HTTP caller times out. A timeout cannot spawn unbounded workers.
        let Ok(permit) = self.workers.clone().try_acquire_owned() else {
            return fallback;
        };
        let base = final_url.clone();
        let parsed = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            metadata(&bytes, &base)
        });
        let Ok(Ok((mut preview, image))) = tokio::time::timeout_at(deadline, parsed).await else {
            return fallback;
        };
        preview.url = original.into();
        preview.final_url = Some(final_url.into());
        if let Some(image) = image {
            if let Ok(Ok((_, bytes))) =
                tokio::time::timeout_at(deadline, self.fetch(image, Kind::Image)).await
            {
                if let Ok(permit) = self.workers.clone().try_acquire_owned() {
                    let image = tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        thumbnail(&bytes)
                    });
                    preview.image_data_url = tokio::time::timeout_at(deadline, image)
                        .await
                        .ok()
                        .and_then(Result::ok)
                        .and_then(Result::ok);
                }
            }
        }
        let ttl = if preview.title.is_some() || preview.description.is_some() {
            900
        } else {
            30
        };
        self.remember(key, preview.clone(), Duration::from_secs(ttl))
            .await;
        preview
    }
    /// Bounded public HTML for company context, with the same DNS pinning,
    /// redirect checks and no credentials as previews. Never executes scripts.
    pub(crate) async fn public_html(&self, raw: &str) -> Result<(Url, Vec<u8>)> {
        let url = public_url(raw)?;
        let _permit = self.requests.clone().try_acquire_owned()?;
        tokio::time::timeout(DEADLINE, self.fetch(url, Kind::Page)).await?
    }
    /// Public search feeds use the same bounded, DNS-pinned fetch as HTML.
    pub(crate) async fn public_feed(&self, raw: &str) -> Result<(Url, Vec<u8>)> {
        let url = public_url(raw)?;
        let _permit = self.requests.clone().try_acquire_owned()?;
        tokio::time::timeout(DEADLINE, self.fetch(url, Kind::Feed)).await?
    }
    async fn remember(&self, key: String, preview: Preview, ttl: Duration) {
        self.cache
            .insert(
                key,
                Cached {
                    preview,
                    until: Instant::now() + ttl,
                },
            )
            .await;
    }
    async fn addresses(&self, url: &Url) -> Result<Vec<SocketAddr>> {
        let host = url.host_str().context("missing host")?.to_owned();
        let port = url.port_or_known_default().context("missing port")?;
        let permit = self.dns.clone().try_acquire_owned()?;
        // getaddrinfo cannot be cancelled. Keep the separate DNS permit inside
        // its blocking worker so slow DNS never produces runaway background jobs.
        let lookup = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            (host.as_str(), port)
                .to_socket_addrs()
                .map(|ips| ips.take(17).collect::<Vec<_>>())
        });
        let addresses = tokio::time::timeout(Duration::from_secs(2), lookup).await???;
        ensure!(
            !addresses.is_empty()
                && addresses.len() <= 16
                && addresses.iter().all(|a| public_ip(a.ip())),
            "not a public destination"
        );
        Ok(addresses)
    }
    async fn fetch(&self, mut url: Url, kind: Kind) -> Result<(Url, Vec<u8>)> {
        for hop in 0..=3 {
            let addresses = self.addresses(&url).await?;
            // A new, pinned connection for every redirect; no ambient proxy,
            // cookies, Authorization, Referer, automatic redirects or DNS retry.
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .resolve_to_addrs(url.host_str().context("missing host")?, &addresses)
                .connect_timeout(Duration::from_secs(2))
                .timeout(DEADLINE)
                .user_agent("Apiosk-LinkPreview/1.0 (+https://apiosk.com)")
                .build()?;
            let response = client
                .get(url.clone())
                .header("accept", kind.accept())
                .header("accept-encoding", "identity")
                .send()
                .await?;
            if response.status().is_redirection() {
                ensure!(hop < 3, "redirect limit");
                let location = response
                    .headers()
                    .get("location")
                    .context("missing redirect")?
                    .to_str()?;
                url = redirect_target(&url, location)?;
                continue;
            }
            ensure!(response.status().is_success(), "page unavailable");
            let mime = response
                .headers()
                .get("content-type")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("")
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            ensure!(kind.accepts(&mime), "unsupported page type");
            if matches!(kind, Kind::Image) {
                ensure!(
                    response.content_length().unwrap_or(0) <= kind.limit() as u64,
                    "body too large"
                );
            }
            let mut bytes = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                if append_body(&mut bytes, &chunk, kind)? {
                    break;
                }
            }
            return Ok((url, bytes));
        }
        unreachable!()
    }
}

/// HTML needs only its bounded head/prefix, even when the full page is large.
/// An image must be complete: truncation never becomes a decodable thumbnail.
fn append_body(bytes: &mut Vec<u8>, chunk: &[u8], kind: Kind) -> Result<bool> {
    if matches!(kind, Kind::Image) {
        ensure!(bytes.len() + chunk.len() <= IMAGE_BYTES, "body too large");
        bytes.extend_from_slice(chunk);
        return Ok(false);
    }
    let scan_from = bytes.len().saturating_sub(6);
    bytes.extend_from_slice(&chunk[..chunk.len().min(HTML_BYTES.saturating_sub(bytes.len()))]);
    if let Some(end) = bytes[scan_from..]
        .windows(7)
        .position(|part| matches!(kind, Kind::Html) && part.eq_ignore_ascii_case(b"</head>"))
    {
        bytes.truncate(scan_from + end + 7);
        return Ok(true);
    }
    Ok(bytes.len() == HTML_BYTES)
}

#[derive(Clone, Copy)]
enum Kind {
    Html,
    Page,
    Feed,
    Image,
}
impl Kind {
    fn limit(self) -> usize {
        match self {
            Self::Html | Self::Page | Self::Feed => HTML_BYTES,
            Self::Image => IMAGE_BYTES,
        }
    }
    fn accept(self) -> &'static str {
        match self {
            Self::Html | Self::Page => "text/html,application/xhtml+xml",
            Self::Feed => "application/rss+xml,application/xml,text/xml",
            Self::Image => "image/jpeg,image/png",
        }
    }
    fn accepts(self, mime: &str) -> bool {
        self.accept().split(',').any(|m| m == mime)
    }
}

/// Conservative public destinations only. Downloads, signatures and common
/// credential query fields retain normal click/download semantics, without fetch.
fn public_url(raw: &str) -> Result<Url> {
    ensure!(
        raw.len() <= 4096 && !raw.chars().any(char::is_control),
        "invalid URL"
    );
    let mut url = Url::parse(raw)?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none(),
        "invalid URL"
    );
    ensure!(
        url.port()
            .is_none_or(|p| matches!((url.scheme(), p), ("http", 80) | ("https", 443))),
        "unsupported port"
    );
    let host = url
        .host_str()
        .context("missing host")?
        .trim_end_matches('.');
    ensure!(
        url.host()
            .is_some_and(|h| matches!(h, url::Host::Domain(_)))
            && host.contains('.')
            && !host.starts_with('.')
            && ![
                "localhost",
                "local",
                "internal",
                "test",
                "invalid",
                "example",
                "onion",
                "home.arpa"
            ]
            .iter()
            .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}"))),
        "not a public hostname"
    );
    for (key, _) in url.query_pairs().chain(url::form_urlencoded::parse(
        url.fragment().unwrap_or("").as_bytes(),
    )) {
        let key: String = key
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        ensure!(
            ![
                "token",
                "key",
                "auth",
                "authorization",
                "signature",
                "sig",
                "secret",
                "password",
                "passwd",
                "jwt",
                "credential",
                "session",
                "code"
            ]
            .iter()
            .any(|suffix| key.ends_with(suffix))
                && !["expires", "expiry", "exp", "download"].contains(&key.as_str())
                && !key.starts_with("xamz")
                && !key.starts_with("xgoog"),
            "credential or download URL"
        );
    }
    ensure!(
        !["pdf", "zip", "doc", "docx", "xls", "xlsx", "csv", "ppt", "pptx"]
            .iter()
            .any(|ext| url
                .path()
                .to_ascii_lowercase()
                .ends_with(&format!(".{ext}"))),
        "download URL"
    );
    url.set_fragment(None);
    Ok(url)
}

fn redirect_target(base: &Url, location: &str) -> Result<Url> {
    ensure!(
        location.len() <= 4096 && !location.chars().any(char::is_control),
        "invalid redirect"
    );
    let next = public_url(base.join(location)?.as_str())?;
    ensure!(
        base.scheme() != "https" || next.scheme() == "https",
        "insecure redirect"
    );
    Ok(next)
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192
                    && ((b == 0 && matches!(c, 0 | 2)) || (b == 88 && c == 99) || b == 168))
                || (a == 198 && (matches!(b, 18 | 19) || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            // Native global unicast only; exclude transition, special-purpose
            // and documentation allocations, including IPv4-mapped addresses.
            (s[0] & 0xe000) == 0x2000
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

fn text(raw: &str, maximum: usize) -> Option<String> {
    let clean = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let value: String = clean
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(*c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(maximum)
        .collect();
    (!value.is_empty()).then_some(value)
}
fn metadata(bytes: &[u8], base: &Url) -> (Preview, Option<Url>) {
    let html = Html::parse_document(&String::from_utf8_lossy(bytes));
    let mut tags = HashMap::new();
    for meta in html
        .select(&Selector::parse("head meta[content]").unwrap())
        .take(256)
    {
        let Some(key) = meta
            .value()
            .attr("property")
            .or_else(|| meta.value().attr("name"))
        else {
            continue;
        };
        let key = key.to_ascii_lowercase();
        if [
            "og:title",
            "og:description",
            "og:site_name",
            "og:image",
            "og:image:secure_url",
            "twitter:title",
            "twitter:description",
            "twitter:image",
            "description",
        ]
        .contains(&key.as_str())
        {
            if let Some(value) = text(meta.value().attr("content").unwrap_or(""), 4096) {
                tags.entry(key).or_insert(value);
            }
        }
    }
    let get = |keys: &[&str], max| {
        keys.iter()
            .find_map(|k| tags.get(*k))
            .and_then(|v| text(v, max))
    };
    let mut preview = Preview::empty(base.as_str());
    preview.title = get(&["og:title", "twitter:title"], 240).or_else(|| {
        html.select(&Selector::parse("head title").unwrap())
            .next()
            .and_then(|n| text(&n.text().collect::<String>(), 240))
    });
    preview.description = get(
        &["og:description", "twitter:description", "description"],
        500,
    );
    preview.site_name = get(&["og:site_name"], 100);
    let image = get(&["og:image:secure_url", "og:image", "twitter:image"], 4096)
        .and_then(|raw| base.join(&raw).ok())
        .and_then(|url| public_url(url.as_str()).ok());
    (preview, image)
}

fn thumbnail(bytes: &[u8]) -> Result<String> {
    ensure!(bytes.len() <= IMAGE_BYTES, "image too large");
    let format = image::guess_format(bytes)?;
    ensure!(
        matches!(format, image::ImageFormat::Jpeg | image::ImageFormat::Png),
        "unsupported image"
    );
    let (width, height) =
        image::io::Reader::with_format(Cursor::new(bytes), format).into_dimensions()?;
    ensure!(
        width > 0
            && height > 0
            && width <= 4096
            && height <= 4096
            && u64::from(width) * u64::from(height) <= 4_000_000,
        "image dimensions too large"
    );
    let image = image::load_from_memory_with_format(bytes, format)?
        .thumbnail(384, 216)
        .to_rgb8();
    let mut clean = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut clean, 75).encode_image(&image)?;
    ensure!(clean.len() <= PREVIEW_BYTES * 3 / 4, "thumbnail too large");
    Ok(format!("data:image/jpeg;base64,{}", STANDARD.encode(clean)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_ordinary_pages_and_preserves_query_but_not_fragment() {
        assert_eq!(
            public_url("https://www.apiosk.com/docs?q=company#sources")
                .unwrap()
                .as_str(),
            "https://www.apiosk.com/docs?q=company"
        );
        assert!(public_url("http://www.apiosk.com").is_ok());
        for url in [
            "file:///tmp/page",
            "https://localhost/page",
            "https://service.internal/",
            "https://person:password@apiosk.com/",
            "https://apiosk.com:8443/",
            "https://apiosk.com/report.pdf",
            "https://apiosk.com/?access_token=redacted",
            "https://apiosk.com/?X-Amz-Credential=redacted",
            "https://apiosk.com/?expires=123&signature=redacted",
        ] {
            assert!(public_url(url).is_err(), "{url}");
        }
    }
    #[test]
    fn callback_and_compound_credentials_keep_click_only_semantics() {
        for suffix in [
            "?code=redacted",
            "?session=redacted",
            "?client_secret=redacted",
            "?api-key=redacted",
            "?session_token=redacted",
            "#access_token=redacted",
        ] {
            assert!(public_url(&format!("https://apiosk.com/callback{suffix}")).is_err());
        }
        assert!(public_url("https://apiosk.com/page?category=company#references").is_ok());
    }
    #[test]
    fn only_public_address_classes_can_be_connected() {
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "2606:4700:4700::1111",
            "2001:4860:4860::8888",
        ] {
            assert!(public_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.0.1",
            "172.16.0.1",
            "192.168.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "::",
            "::1",
            "::ffff:8.8.8.8",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2002::1",
            "3fff::1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
    }
    #[test]
    fn redirects_repeat_the_same_policy_and_keep_tls() {
        let base = Url::parse("https://apiosk.com/start").unwrap();
        assert_eq!(
            redirect_target(&base, "/docs").unwrap().as_str(),
            "https://apiosk.com/docs"
        );
        assert!(redirect_target(&base, "http://apiosk.com/docs").is_err());
        assert!(redirect_target(&base, "/report?signature=redacted").is_err());
        assert!(redirect_target(&base, "https://service.internal/docs").is_err());
    }
    #[test]
    fn metadata_prefers_og_decodes_entities_and_uses_final_page_as_base() {
        let base = Url::parse("https://apiosk.com/docs/page").unwrap();
        let (p, image) = metadata(br#"<html><head><title>Fallback</title><meta property="og:title" content="A &amp; B"><meta name="description" content="A useful page"><meta property="og:site_name" content="Apiosk"><meta property="og:image" content="../card.png"></head><body><meta property="og:title" content="Body is ignored"></body></html>"#, &base);
        assert_eq!(p.title.as_deref(), Some("A & B"));
        assert_eq!(p.description.as_deref(), Some("A useful page"));
        assert_eq!(p.site_name.as_deref(), Some("Apiosk"));
        assert_eq!(image.unwrap().as_str(), "https://apiosk.com/card.png");
        assert_eq!(text(&"é".repeat(700), 240).unwrap().chars().count(), 240);
        let (fallback, _) = metadata(b"<title>Plain title</title>", &base);
        assert_eq!(fallback.title.as_deref(), Some("Plain title"));
    }
    #[test]
    fn company_context_reads_past_the_head_but_never_past_the_html_limit() {
        let mut bytes = Vec::new();
        assert!(!append_body(&mut bytes, b"<head></head><body>Company profile</body>", Kind::Page).unwrap());
        assert!(String::from_utf8_lossy(&bytes).contains("Company profile"));
        assert!(append_body(&mut bytes, &vec![b'a'; HTML_BYTES + 1], Kind::Page).unwrap());
        assert_eq!(bytes.len(), HTML_BYTES);
    }

    #[test]
    fn large_html_keeps_metadata_and_stops_at_a_split_head_boundary() {
        let base = Url::parse("https://apiosk.com/page").unwrap();
        let mut bytes = Vec::new();
        assert!(!append_body(
            &mut bytes,
            b"<head><title>Large page</title></he",
            Kind::Html
        )
        .unwrap());
        assert!(append_body(&mut bytes, b"AD><body>Unneeded body", Kind::Html).unwrap());
        assert!(bytes.ends_with(b"</heAD>"));
        assert_eq!(
            metadata(&bytes, &base).0.title.as_deref(),
            Some("Large page")
        );
        let mut bytes = Vec::new();
        let mut large = b"<head><meta property='og:title' content='Bounded prefix'>".to_vec();
        large.resize(HTML_BYTES + 100, b' ');
        assert!(append_body(&mut bytes, &large, Kind::Html).unwrap());
        assert_eq!(bytes.len(), HTML_BYTES);
        assert_eq!(
            metadata(&bytes, &base).0.title.as_deref(),
            Some("Bounded prefix")
        );
        assert!(append_body(&mut Vec::new(), &vec![0; IMAGE_BYTES + 1], Kind::Image).is_err());
    }
    #[test]
    fn image_is_reencoded_small_raster_with_no_embedded_metadata() {
        let image = image::DynamicImage::new_rgb8(800, 400);
        let mut input = Vec::new();
        image
            .write_to(&mut input, image::ImageOutputFormat::Png)
            .unwrap();
        let data = thumbnail(&input).unwrap();
        let clean = STANDARD
            .decode(data.strip_prefix("data:image/jpeg;base64,").unwrap())
            .unwrap();
        let dimensions = image::io::Reader::new(Cursor::new(clean))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap();
        assert_eq!(dimensions, (384, 192));
        assert!(thumbnail(b"<svg></svg>").is_err());
        assert!(thumbnail(&vec![0; IMAGE_BYTES + 1]).is_err());
        let wide = image::DynamicImage::new_rgb8(4097, 1);
        let mut oversized_dimensions = Vec::new();
        wide.write_to(&mut oversized_dimensions, image::ImageOutputFormat::Png)
            .unwrap();
        assert!(thumbnail(&oversized_dimensions).is_err());
    }
    #[tokio::test]
    async fn fallback_cache_rate_and_concurrency_are_bounded_independently() {
        let service = Service::new();
        let owner = Uuid::new_v4();
        let now = Instant::now();
        for _ in 0..60 {
            assert!(service.admit_at(owner, now));
        }
        assert!(!service.admit_at(owner, now));
        assert!(service.admit_at(Uuid::new_v4(), now));
        assert!(service.admit_at(owner, now + WINDOW));
        let original = "https://apiosk.com/report.pdf?signature=redacted";
        let fallback = service.preview(original).await;
        assert_eq!(fallback.url, original);
        assert!(fallback.title.is_none());
        let url = "https://apiosk.com/docs";
        let key = hex::encode(Sha256::digest(url.as_bytes()));
        let mut cached = Preview::empty(url);
        cached.title = Some("Saved metadata".into());
        service.remember(key, cached, WINDOW).await;
        let _permits = service.requests.acquire_many(8).await.unwrap();
        assert_eq!(
            service.preview(url).await.title.as_deref(),
            Some("Saved metadata")
        );
        assert!(service
            .preview("https://apiosk.com/another")
            .await
            .title
            .is_none());
        assert_eq!(service.cache.policy().max_capacity(), Some(128));
    }
}
