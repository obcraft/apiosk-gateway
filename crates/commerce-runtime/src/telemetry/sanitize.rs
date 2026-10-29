//! Sanitisation + safe derivation of telemetry values from untrusted request
//! data. Nothing here ever stores a raw secret, body, proof, or credential.
//!
//! Covers: sensitive-name redaction (§2.11), query-key extraction, User-Agent /
//! SDK parsing, upstream-host sanitisation, and trusted-proxy client-IP
//! resolution (§2.12).

use std::net::IpAddr;

/// Substrings that mark a header or query-parameter NAME as sensitive. If a name
/// contains any of these (case-insensitive) its value is never stored and, for
/// query params, the name itself is redacted + counted.
const SENSITIVE_NAME_FRAGMENTS: &[&str] = &[
    "authorization",
    "cookie",
    "token",
    "secret",
    "key",
    "signature",
    "password",
    "credential",
    "payment",
];

/// True if a header / param name is sensitive and must be redacted.
pub fn is_sensitive_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    SENSITIVE_NAME_FRAGMENTS.iter().any(|frag| n.contains(frag))
}

/// Result of sanitising a raw query string.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SanitizedQuery {
    /// Non-sensitive parameter names, order-preserving, de-duplicated.
    pub keys: Vec<String>,
    /// Count of parameters whose names were redacted as sensitive.
    pub redacted_count: i64,
}

/// Extract non-sensitive parameter NAMES from a raw query string. Values are
/// never retained. Sensitive names are dropped and counted in `redacted_count`.
pub fn sanitize_query(raw_query: Option<&str>) -> SanitizedQuery {
    let mut out = SanitizedQuery::default();
    let raw = match raw_query {
        Some(q) if !q.is_empty() => q,
        _ => return out,
    };
    for pair in raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let name = pair.split('=').next().unwrap_or("");
        if name.is_empty() {
            continue;
        }
        // Percent-decoding is intentionally skipped — we only need a stable label.
        if is_sensitive_name(name) {
            out.redacted_count += 1;
            continue;
        }
        let name = name.to_string();
        if !out.keys.contains(&name) {
            out.keys.push(name);
        }
    }
    out
}

/// Sanitise an upstream URL down to its host (no scheme, path, query, or creds).
/// Used by the deferred upstream-attempt emitter (§2.9); `allow(dead_code)` until
/// that hook is wired in the payment/upstream rollout stage.
#[allow(dead_code)]
pub fn sanitize_upstream_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    parsed.host_str().map(|h| h.to_string())
}

/// Parsed client / agent identity, best-effort, from the User-Agent header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentInfo {
    pub agent_name: Option<String>,
    pub agent_version: Option<String>,
    pub agent_framework: Option<String>,
    pub sdk_name: Option<String>,
    pub sdk_version: Option<String>,
    /// agent | browser | bot | sdk | unknown
    pub client_kind: Option<String>,
}

fn split_product(token: &str) -> (String, Option<String>) {
    match token.split_once('/') {
        Some((name, ver)) if !ver.is_empty() => (name.to_string(), Some(ver.to_string())),
        _ => (token.to_string(), None),
    }
}

/// Parse a best-effort agent/SDK identity out of a User-Agent string. Purely
/// descriptive — never used for access or trust decisions.
pub fn parse_agent(user_agent: Option<&str>) -> AgentInfo {
    let mut info = AgentInfo::default();
    let ua = match user_agent {
        Some(u) if !u.trim().is_empty() => u.trim(),
        _ => return info,
    };
    let lower = ua.to_ascii_lowercase();

    // Known AI agent frameworks.
    for (needle, label) in [
        ("langchain", "langchain"),
        ("llamaindex", "llamaindex"),
        ("llama-index", "llamaindex"),
        ("crewai", "crewai"),
        ("autogpt", "autogpt"),
        ("autogen", "autogen"),
        ("openai", "openai"),
        ("anthropic", "anthropic"),
        ("vercel-ai", "vercel-ai"),
    ] {
        if lower.contains(needle) {
            info.agent_framework = Some(label.to_string());
            break;
        }
    }

    // SDK / HTTP-client families.
    let first = ua.split_whitespace().next().unwrap_or(ua);
    let (name, version) = split_product(first);
    let name_lower = name.to_ascii_lowercase();
    let is_sdk = [
        "apiosk",
        "python-requests",
        "httpx",
        "aiohttp",
        "node-fetch",
        "axios",
        "undici",
        "go-http-client",
        "okhttp",
        "reqwest",
        "curl",
        "postmanruntime",
    ]
    .iter()
    .any(|s| name_lower.contains(s));
    if is_sdk {
        info.sdk_name = Some(name.clone());
        info.sdk_version = version.clone();
    }

    info.agent_name = Some(name);
    info.agent_version = version;

    // Coarse client kind.
    info.client_kind = Some(
        if info.agent_framework.is_some() {
            "agent"
        } else if lower.contains("bot") || lower.contains("crawler") || lower.contains("spider") {
            "bot"
        } else if lower.contains("mozilla") {
            "browser"
        } else if is_sdk {
            "sdk"
        } else {
            "unknown"
        }
        .to_string(),
    );
    info
}

// ---------------------------------------------------------------------------
// Trusted-proxy client-IP resolution (§2.12).
// ---------------------------------------------------------------------------

/// A set of trusted proxy CIDR ranges. Parsed once from config; matched per
/// request. Hand-rolled (v4 + v6) to avoid pulling in an IP-network dependency.
#[derive(Debug, Clone, Default)]
pub struct TrustedProxies {
    v4: Vec<(u32, u32)>,   // (network, mask)
    v6: Vec<(u128, u128)>, // (network, mask)
}

impl TrustedProxies {
    /// Parse a comma/space separated list of CIDRs (e.g. "10.0.0.0/8,::1/128").
    /// Bare IPs are treated as /32 (v4) or /128 (v6). Unparseable entries are
    /// skipped.
    pub fn parse(list: &str) -> Self {
        let mut set = TrustedProxies::default();
        for raw in list.split([',', ' ', '\n', '\t']) {
            let entry = raw.trim();
            if entry.is_empty() {
                continue;
            }
            let (addr_part, prefix_part) = match entry.split_once('/') {
                Some((a, p)) => (a, Some(p)),
                None => (entry, None),
            };
            match addr_part.parse::<IpAddr>() {
                Ok(IpAddr::V4(v4)) => {
                    let prefix: u8 = prefix_part
                        .and_then(|p| p.parse().ok())
                        .filter(|p| *p <= 32)
                        .unwrap_or(32);
                    let mask = if prefix == 0 {
                        0
                    } else {
                        u32::MAX << (32 - prefix)
                    };
                    let net = u32::from(v4) & mask;
                    set.v4.push((net, mask));
                }
                Ok(IpAddr::V6(v6)) => {
                    let prefix: u16 = prefix_part
                        .and_then(|p| p.parse().ok())
                        .filter(|p| *p <= 128)
                        .unwrap_or(128);
                    let mask = if prefix == 0 {
                        0
                    } else {
                        u128::MAX << (128 - prefix)
                    };
                    let net = u128::from(v6) & mask;
                    set.v6.push((net, mask));
                }
                Err(_) => continue,
            }
        }
        set
    }

    pub fn is_empty(&self) -> bool {
        self.v4.is_empty() && self.v6.is_empty()
    }

    pub fn contains(&self, ip: &IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => {
                let x = u32::from(*v4);
                self.v4.iter().any(|(net, mask)| (x & mask) == *net)
            }
            IpAddr::V6(v6) => {
                let x = u128::from(*v6);
                self.v6.iter().any(|(net, mask)| (x & mask) == *net)
            }
        }
    }
}

/// The resolved client IP + whether the chain that produced it was trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedIp {
    pub ip: Option<IpAddr>,
    /// True when the value came from a verified (trusted-proxy) chain or the
    /// direct socket. False when we had to accept an unverifiable forwarded
    /// header (no trusted ranges configured).
    pub trusted: bool,
}

/// Resolve the real client IP, replacing "blindly trust the first XFF value".
///
/// Algorithm: walk the `X-Forwarded-For` chain RIGHT→LEFT, skipping hops that
/// fall inside a configured trusted range; the first non-trusted hop is the
/// client. With no trusted ranges configured we cannot verify the chain, so we
/// fall back to the leftmost XFF value / `X-Real-IP` / socket and mark the
/// result `trusted = false`.
pub fn resolve_client_ip(
    xff: Option<&str>,
    x_real_ip: Option<&str>,
    socket: Option<IpAddr>,
    trusted: &TrustedProxies,
) -> ResolvedIp {
    // Parse the XFF chain (left→right = client … nearest proxy).
    let chain: Vec<IpAddr> = xff
        .map(|raw| {
            raw.split(',')
                .filter_map(|part| parse_ip(part.trim()))
                .collect()
        })
        .unwrap_or_default();

    if !trusted.is_empty() && !chain.is_empty() {
        // Right→left: skip trusted proxies, first untrusted is the client.
        for ip in chain.iter().rev() {
            if !trusted.contains(ip) {
                return ResolvedIp {
                    ip: Some(*ip),
                    trusted: true,
                };
            }
        }
        // Entire chain was trusted infrastructure → leftmost is the origin.
        return ResolvedIp {
            ip: chain.first().copied(),
            trusted: true,
        };
    }

    // No trusted config (or no XFF): best-effort, unverified.
    if let Some(ip) = chain.first().copied() {
        return ResolvedIp {
            ip: Some(ip),
            trusted: false,
        };
    }
    if let Some(ip) = x_real_ip.and_then(|v| parse_ip(v.trim())) {
        return ResolvedIp {
            ip: Some(ip),
            trusted: false,
        };
    }
    ResolvedIp {
        ip: socket,
        trusted: socket.is_some(),
    }
}

/// Parse an IP that may carry brackets/ports, mirroring the legacy helper in
/// traffic.rs (`[::1]:port`, `1.2.3.4:port`, bare).
fn parse_ip(raw: &str) -> Option<IpAddr> {
    let candidate = raw.trim().trim_matches('"');
    if candidate.is_empty() {
        return None;
    }
    if let Ok(ip) = candidate.parse::<IpAddr>() {
        return Some(ip);
    }
    if let Some(stripped) = candidate
        .strip_prefix('[')
        .and_then(|s| s.split(']').next())
    {
        if let Ok(ip) = stripped.parse::<IpAddr>() {
            return Some(ip);
        }
    }
    if candidate.contains('.') && candidate.matches(':').count() == 1 {
        if let Some((host, _port)) = candidate.rsplit_once(':') {
            if let Ok(ip) = host.parse::<IpAddr>() {
                return Some(ip);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_names_redacted() {
        assert!(is_sensitive_name("Authorization"));
        assert!(is_sensitive_name("X-Api-Key"));
        assert!(is_sensitive_name("access_token"));
        assert!(is_sensitive_name("X-Payment"));
        assert!(is_sensitive_name("signature"));
        assert!(!is_sensitive_name("q"));
        assert!(!is_sensitive_name("units"));
    }

    #[test]
    fn query_keeps_names_redacts_secrets() {
        let s = sanitize_query(Some("q=boston&units=metric&api_key=SECRET&token=abc&q=dup"));
        assert_eq!(s.keys, vec!["q".to_string(), "units".to_string()]);
        assert_eq!(s.redacted_count, 2);
    }

    #[test]
    fn empty_query() {
        assert_eq!(sanitize_query(None), SanitizedQuery::default());
        assert_eq!(sanitize_query(Some("")), SanitizedQuery::default());
    }

    #[test]
    fn upstream_host_only() {
        assert_eq!(
            sanitize_upstream_host("https://api.weather.com/v1/forecast?token=x").as_deref(),
            Some("api.weather.com")
        );
        assert_eq!(sanitize_upstream_host("not a url"), None);
    }

    #[test]
    fn agent_parsing() {
        let sdk = parse_agent(Some("apiosk-ai-sdk/1.4.3"));
        assert_eq!(sdk.sdk_name.as_deref(), Some("apiosk-ai-sdk"));
        assert_eq!(sdk.sdk_version.as_deref(), Some("1.4.3"));
        assert_eq!(sdk.client_kind.as_deref(), Some("sdk"));

        let lc = parse_agent(Some("langchain/0.1 python-requests/2.31"));
        assert_eq!(lc.agent_framework.as_deref(), Some("langchain"));
        assert_eq!(lc.client_kind.as_deref(), Some("agent"));

        let browser = parse_agent(Some(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
        ));
        assert_eq!(browser.client_kind.as_deref(), Some("browser"));

        assert_eq!(parse_agent(None), AgentInfo::default());
    }

    #[test]
    fn trusted_proxy_right_to_left() {
        // Fly-style: two trusted internal hops, real client on the far left.
        let trusted = TrustedProxies::parse("10.0.0.0/8, 172.16.0.0/12");
        let r = resolve_client_ip(
            Some("203.0.113.7, 10.1.2.3, 172.16.0.9"),
            None,
            None,
            &trusted,
        );
        assert_eq!(r.ip.unwrap().to_string(), "203.0.113.7");
        assert!(r.trusted);
    }

    #[test]
    fn spoofed_forwarded_header_is_not_trusted_without_config() {
        // A client injecting a fake XFF with no trusted config → accepted but
        // flagged untrusted, never silently trusted.
        let trusted = TrustedProxies::default();
        let r = resolve_client_ip(Some("1.2.3.4"), None, None, &trusted);
        assert_eq!(r.ip.unwrap().to_string(), "1.2.3.4");
        assert!(!r.trusted);
    }

    #[test]
    fn spoof_inside_trusted_chain_is_ignored() {
        // Attacker prepends a spoofed public IP that is a trusted-range address;
        // right-to-left resolution still finds the first UNTRUSTED hop.
        let trusted = TrustedProxies::parse("10.0.0.0/8");
        let r = resolve_client_ip(
            Some("10.9.9.9, 198.51.100.5, 10.0.0.2"),
            None,
            None,
            &trusted,
        );
        assert_eq!(r.ip.unwrap().to_string(), "198.51.100.5");
        assert!(r.trusted);
    }

    #[test]
    fn socket_fallback() {
        let trusted = TrustedProxies::default();
        let sock: IpAddr = "192.0.2.1".parse().unwrap();
        let r = resolve_client_ip(None, None, Some(sock), &trusted);
        assert_eq!(r.ip, Some(sock));
        assert!(r.trusted);
    }
}
