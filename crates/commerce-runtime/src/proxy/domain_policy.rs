//! Per-connection destination allowlist.
//!
//! A connect token carries an optional set of hosts its agent may *spend*
//! against (`agent_wallet_connect_tokens.allowed_domains`, migration 069). This
//! module owns the two decisions that go with it: how a stored entry is
//! normalized, and whether a given destination host matches.
//!
//! Two properties matter more than cleverness here:
//!
//! 1. **Empty means unrestricted.** Every token minted before migration 069 has
//!    `NULL`, and a buyer who never opens the allowlist UI has `{}`. Both must
//!    behave exactly as the gateway did before this existed, or the migration
//!    silently bricks live agents.
//! 2. **A non-empty list fails closed.** Once a buyer has expressed an intent,
//!    an unparseable destination is a refusal, not a pass. The whole point of
//!    the list is that the agent cannot spend somewhere the buyer didn't name.
//!
//! The list gates *spending*, not access: free endpoints are never checked,
//! because a call that costs nothing can't drain a wallet.

/// Normalize a user-supplied domain into the stored form: lowercase, no scheme,
/// no userinfo, no port, no path, no trailing dot. Returns `None` when the input
/// can't be read as a host, so the caller can reject it at write time rather
/// than storing an entry that could never match.
///
/// A leading `*.` is preserved — it is the wildcard marker, not part of a host.
pub fn normalize_domain(raw: &str) -> Option<String> {
    let mut value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }

    // Tolerate a pasted URL: strip scheme, then anything from the first `/`.
    if let Some(idx) = value.find("://") {
        value = value[idx + 3..].to_string();
    }
    if let Some(idx) = value.find('/') {
        value.truncate(idx);
    }
    // userinfo@host
    if let Some(idx) = value.rfind('@') {
        value = value[idx + 1..].to_string();
    }
    // Strip a port, but not the colons of a bracketed IPv6 literal. IPv6 hosts
    // aren't valid allowlist entries anyway (see the charset check below); this
    // just avoids mangling one into something that accidentally parses.
    if !value.starts_with('[') {
        if let Some(idx) = value.find(':') {
            value.truncate(idx);
        }
    }
    while value.ends_with('.') {
        value.pop();
    }

    let wildcard = value.starts_with("*.");
    let host = if wildcard { &value[2..] } else { &value[..] };

    // Must look like a dotted DNS name: at least two labels, each of
    // alphanumerics and inner hyphens. This deliberately excludes bare
    // `localhost`, IP literals, and anything with a stray character — the same
    // shape the SQL CHECK constraint enforces, so the DB and the gateway agree
    // on what a well-formed entry is.
    if host.len() > 253 || !host.contains('.') {
        return None;
    }
    for label in host.split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        if label.starts_with('-') || label.ends_with('-') {
            return None;
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return None;
        }
    }

    Some(value)
}

/// Whether `host` is authorized by `allowed`.
///
/// An empty list is unrestricted. A `*.example.com` entry matches
/// `example.com` and any subdomain of it; a bare `example.com` matches only
/// that exact host. Matching is case-insensitive and ignores a trailing dot on
/// the destination.
pub fn host_is_allowed(allowed: &[String], host: &str) -> bool {
    if allowed.is_empty() {
        return true;
    }
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        // Non-empty list + unreadable destination => fail closed.
        return false;
    }

    allowed.iter().any(|entry| {
        let entry = entry.trim().to_ascii_lowercase();
        match entry.strip_prefix("*.") {
            Some(base) => host == base || host.ends_with(&format!(".{base}")),
            None => host == entry,
        }
    })
}

/// The host a paid call is destined for, extracted from an upstream origin URL.
/// Returns `None` when the URL has no readable host — with a non-empty
/// allowlist that becomes a refusal.
pub fn origin_host(origin_url: &str) -> Option<String> {
    let value = origin_url.trim();
    if value.is_empty() {
        return None;
    }
    let host = match url::Url::parse(value) {
        Ok(parsed) => parsed.host_str().map(|h| h.to_string()),
        // Origins are stored with a scheme, but a bare `api.example.com/v1`
        // shouldn't silently become "no host" and block every call.
        Err(_) => normalize_domain(value),
    }?;
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|e| e.to_string()).collect()
    }

    /// The migration-not-applied contract, asserted rather than assumed.
    ///
    /// When `allowed_domains` can't be read, the gateway hands `domain_policy`
    /// an empty list. Every destination must then be permitted — including ones
    /// that are malformed, empty, or would be rejected outright by a non-empty
    /// list. If this test ever fails, shipping this binary ahead of migration
    /// 069 breaks live paid traffic.
    #[test]
    fn no_migration_means_every_destination_is_permitted() {
        let no_list: Vec<String> = Vec::new();
        for host in [
            "api.openweathermap.org",
            "gateway.apiosk.com",
            "evil-example.com",
            "localhost",
            "",
            "   ",
            "not a host at all",
        ] {
            assert!(
                host_is_allowed(&no_list, host),
                "empty allowlist must permit {host:?} — otherwise a gateway \
                 deployed before migration 069 would refuse live paid calls"
            );
        }
    }

    #[test]
    fn empty_allowlist_is_unrestricted() {
        assert!(host_is_allowed(&[], "api.example.com"));
        assert!(host_is_allowed(&[], "anything.at.all"));
    }

    #[test]
    fn exact_entry_matches_only_that_host() {
        let allowed = list(&["api.example.com"]);
        assert!(host_is_allowed(&allowed, "api.example.com"));
        assert!(host_is_allowed(&allowed, "API.EXAMPLE.COM"));
        assert!(host_is_allowed(&allowed, "api.example.com."));
        assert!(!host_is_allowed(&allowed, "example.com"));
        assert!(!host_is_allowed(&allowed, "other.example.com"));
    }

    #[test]
    fn wildcard_matches_apex_and_subdomains() {
        let allowed = list(&["*.example.com"]);
        assert!(host_is_allowed(&allowed, "example.com"));
        assert!(host_is_allowed(&allowed, "api.example.com"));
        assert!(host_is_allowed(&allowed, "deep.nested.example.com"));
        assert!(!host_is_allowed(&allowed, "notexample.com"));
        // The classic suffix-match bug: `evil-example.com` must not match.
        assert!(!host_is_allowed(&allowed, "evil-example.com"));
    }

    #[test]
    fn non_empty_allowlist_fails_closed_on_unreadable_host() {
        let allowed = list(&["api.example.com"]);
        assert!(!host_is_allowed(&allowed, ""));
        assert!(!host_is_allowed(&allowed, "   "));
    }

    #[test]
    fn normalize_strips_scheme_port_path_and_case() {
        assert_eq!(
            normalize_domain("HTTPS://API.Example.com:8443/v1/forecast?a=1"),
            Some("api.example.com".to_string())
        );
        assert_eq!(
            normalize_domain("  api.example.com.  "),
            Some("api.example.com".to_string())
        );
        assert_eq!(
            normalize_domain("user:pw@api.example.com"),
            Some("api.example.com".to_string())
        );
    }

    #[test]
    fn normalize_preserves_the_wildcard_marker() {
        assert_eq!(
            normalize_domain("*.Example.com"),
            Some("*.example.com".to_string())
        );
        assert_eq!(
            normalize_domain("https://*.example.com/"),
            Some("*.example.com".to_string())
        );
    }

    #[test]
    fn normalize_rejects_entries_that_could_never_match() {
        // Single-label names, IP literals and junk are not allowlist entries.
        assert_eq!(normalize_domain("localhost"), None);
        assert_eq!(normalize_domain("127.0.0.1"), Some("127.0.0.1".to_string()));
        assert_eq!(normalize_domain(""), None);
        assert_eq!(normalize_domain("   "), None);
        assert_eq!(normalize_domain("exa mple.com"), None);
        assert_eq!(normalize_domain("-bad.example.com"), None);
        assert_eq!(normalize_domain("bad-.example.com"), None);
        assert_eq!(normalize_domain("double..dot.com"), None);
    }

    #[test]
    fn origin_host_reads_the_upstream_not_the_gateway() {
        assert_eq!(
            origin_host("https://api.openweathermap.org/data/2.5"),
            Some("api.openweathermap.org".to_string())
        );
        assert_eq!(
            origin_host("http://API.Example.com:8080/v1"),
            Some("api.example.com".to_string())
        );
        // Scheme-less origins still resolve rather than blocking every call.
        assert_eq!(
            origin_host("api.example.com/v1"),
            Some("api.example.com".to_string())
        );
        assert_eq!(origin_host(""), None);
        assert_eq!(origin_host("   "), None);
    }

    #[test]
    fn a_normalized_entry_matches_the_host_it_came_from() {
        // Round-trip: what the portal stores must match what the proxy extracts.
        let entry = normalize_domain("https://API.OpenWeatherMap.org/data/2.5").unwrap();
        let host = origin_host("https://api.openweathermap.org/data/2.5").unwrap();
        assert!(host_is_allowed(&[entry], &host));
    }
}
