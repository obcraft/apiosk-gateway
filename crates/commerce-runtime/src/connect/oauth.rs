//! Handing a connect token to the platform the buyer came from.
//!
//! The buyer arrives at the portal from n8n (or Make, or Zapier), finishes the
//! setup, and the platform needs the resulting `aw_live_…` token. The obvious
//! implementation — put it in the redirect URL — puts a live spending credential
//! into browser history, the referrer header, and every proxy log on the way.
//!
//! So this is OAuth 2.0 authorization code (RFC 6749 §4.1) with PKCE (RFC 7636),
//! which is the handshake all of those platforms already speak:
//!
//!   1. The portal finishes the flow and calls [`park_connect_token`] with the
//!      fresh token. It gets back a single-use code. (Portal session auth.)
//!   2. The portal redirects the browser to `redirect_uri?code=…&state=…`.
//!   3. The platform's SERVER calls [`exchange_code`] with the code and its PKCE
//!      verifier, and receives the token. (No auth — the code and the verifier
//!      are the proof.)
//!
//! The token crosses the network twice, both times in a POST body over TLS, and
//! never once through the browser.
//!
//! Two things do the security work here. The code is bound at issue time to the
//! exact `redirect_uri` and to a PKCE challenge, so observing the redirect is not
//! enough to redeem it. And redemption is atomic and destructive (see
//! `db::connect_oauth`), so a replayed code is an `invalid_grant`, not a second
//! copy of a credential.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::Response,
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL;
use base64::Engine as _;
use chrono::{DateTime, Duration, Utc};
use ring::digest;
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use serde_json::json;
use tracing::{error, warn};
use url::Url;
use uuid::Uuid;

use super::owner_auth::resolve_owner_user_id;
use super::{json_error, json_response};
use crate::db::connect_oauth::{ConnectOauthClient, ParkConnectToken};
use crate::AppState;

/// How long a parked code stays redeemable. RFC 6749 §4.1.2 asks for a maximum
/// of ten minutes; the exchange happens within seconds of the redirect in
/// practice, so this is slack for a slow platform, not a window to plan around.
const CODE_TTL_SECONDS: i64 = 600;

/// Prefix on the raw code. Purely so a leaked value is identifiable on sight —
/// it carries no meaning and is not parsed.
const CODE_PREFIX: &str = "aoc_";

/// The scopes an Apiosk connect token carries. Fixed: the token's real authority
/// is the wallet and the caps bound to it at mint time, not this string.
const TOKEN_SCOPE: &str = "x402.verify x402.settle";

// ---------------------------------------------------------------------------
// 1. Park — called by the buyer portal, with the buyer's session
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ParkRequest {
    pub client_id: String,
    pub redirect_uri: String,
    /// The freshly-minted `aw_live_…` token. Write-only: it goes straight into
    /// the encrypting INSERT and is never echoed back or logged.
    pub connect_token: String,
    pub gateway_url: String,
    #[serde(default)]
    pub connection_id: Option<Uuid>,
    #[serde(default)]
    pub connection_label: Option<String>,
    #[serde(default)]
    pub token_expires_at: Option<DateTime<Utc>>,
    /// BASE64URL(SHA256(code_verifier)), from the platform's authorize request.
    #[serde(default)]
    pub code_challenge: Option<String>,
    /// What the connection is bound to, as the buyer was shown it at consent.
    ///
    /// Optional, and absent from every client that predates migration 082. A
    /// client that sends them gets them back on exchange and can say which
    /// wallet and which rules it is working under without a second call — which
    /// is what `apiosk_connect` needs on the first turn of a conversation.
    #[serde(default)]
    pub wallet_id: Option<Uuid>,
    #[serde(default)]
    pub policy_id: Option<Uuid>,
    #[serde(default)]
    pub limits: Option<serde_json::Value>,
}

pub async fn park_connect_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Result<axum::Json<ParkRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let axum::Json(request) = match body {
        Ok(json) => json,
        Err(_) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "Expected a JSON body.",
            )
        }
    };

    // The buyer authorizing the handoff must be the one signed in. Without this
    // anybody could park a token they minted against somebody else's redirect.
    let user_id = match resolve_owner_user_id(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let client = match load_client(&state, &request.client_id).await {
        Ok(Some(client)) => client,
        Ok(None) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "unknown_client",
                "That application is not registered for Apiosk Connect.",
            )
        }
        Err(response) => return response,
    };

    if request.connect_token.trim().is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "connect_token is required.",
        );
    }

    let redirect_uri = match parse_redirect_uri(&request.redirect_uri) {
        Ok(url) => url,
        Err(message) => {
            return json_error(StatusCode::BAD_REQUEST, "invalid_redirect_uri", message)
        }
    };

    if !redirect_uri_allowed(&client.redirect_uri_patterns, &redirect_uri) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_redirect_uri",
            "That callback address is not allowed for this application.",
        );
    }

    // A client whose callback can be any host (self-hosted n8n) has nothing but
    // PKCE standing between the redirect and a stolen credential.
    let challenge = request
        .code_challenge
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    if client.requires_pkce && challenge.is_none() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "This application must start the handoff with a PKCE code_challenge.",
        );
    }
    if let Some(challenge) = challenge {
        if !is_plausible_challenge(challenge) {
            return json_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "code_challenge must be BASE64URL(SHA256(code_verifier)).",
            );
        }
    }

    let code = match generate_code() {
        Some(code) => code,
        None => {
            error!("connect oauth: system RNG failed");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "code_generation_failed",
                "Could not start the handoff. Try again.",
            );
        }
    };
    let expires_at = Utc::now() + Duration::seconds(CODE_TTL_SECONDS);

    let park = ParkConnectToken {
        code_hash: &sha256_hex(code.as_bytes()),
        client_id: &client.client_id,
        user_id,
        redirect_uri: redirect_uri.as_str(),
        connect_token: request.connect_token.trim(),
        gateway_url: request.gateway_url.trim(),
        connection_id: request.connection_id,
        connection_label: request.connection_label.as_deref(),
        token_expires_at: request.token_expires_at,
        code_challenge: challenge,
        expires_at,
        wallet_id: request.wallet_id,
        policy_id: request.policy_id,
        limits_snapshot: request.limits.as_ref(),
    };

    if let Err(err) = state.db.park_connect_oauth_token(park).await {
        error!("connect oauth: could not park token: {err}");
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "park_failed",
            "Could not hand the connection over. Try again.",
        );
    }

    json_response(
        StatusCode::OK,
        json!({
            "code": code,
            "expires_in": CODE_TTL_SECONDS,
            "redirect_uri": redirect_uri.as_str(),
            "client_name": client.display_name,
        }),
    )
}

// ---------------------------------------------------------------------------
// 2. Exchange — called by the platform's server
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct ExchangeRequest {
    #[serde(default)]
    pub grant_type: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub redirect_uri: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub code_verifier: Option<String>,
}

/// `POST /v1/connect/oauth/token`.
///
/// Public by design: the code plus the PKCE verifier are the credential. Errors
/// follow RFC 6749 §5.2 (`invalid_grant`, `invalid_request`, …) because that is
/// what the platforms' OAuth clients parse — a friendlier shape here would be
/// rendered to the user as "undefined".
///
/// The body arrives form-encoded from most clients and as JSON from a few, so
/// both are accepted rather than making the integration guess.
pub async fn exchange_code(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let request = match parse_exchange_body(&headers, &body) {
        Some(request) => request,
        None => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "Could not read the request body as form-encoded or JSON.",
            )
        }
    };

    // Absent grant_type is tolerated (some clients omit it on a single-grant
    // endpoint); a *wrong* one is not, because it means the client believes it
    // is doing something this endpoint will not do.
    if let Some(grant_type) = request.grant_type.as_deref() {
        if grant_type != "authorization_code" {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "unsupported_grant_type",
                "Only the authorization_code grant is supported.",
            );
        }
    }

    let code = match request
        .code
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        Some(code) => code,
        None => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "code is required.",
            )
        }
    };

    let parked = match state
        .db
        .redeem_connect_oauth_code(&sha256_hex(code.as_bytes()))
        .await
    {
        Ok(Some(parked)) => parked,
        Ok(None) => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "That code is unknown, expired, or already used.",
            )
        }
        Err(err) => {
            error!("connect oauth: redeem failed: {err}");
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "Could not complete the exchange.",
            );
        }
    };

    // From here the code is spent whatever happens. Every remaining check that
    // fails burns it — which is the correct outcome for a code presented with
    // the wrong verifier or from the wrong client: it is under suspicion, and it
    // must not survive to be presented again correctly.

    if let Some(client_id) = request.client_id.as_deref().map(str::trim) {
        if !client_id.is_empty() && client_id != parked.client_id {
            warn!(
                "connect oauth: client_id mismatch on exchange (code issued to {})",
                parked.client_id
            );
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "That code was not issued to this client.",
            );
        }
    }

    // RFC 6749 §4.1.3: when the code was issued against a redirect_uri, the
    // exchange must present the identical one.
    if let Some(redirect_uri) = request.redirect_uri.as_deref().map(str::trim) {
        if !redirect_uri.is_empty() && redirect_uri != parked.redirect_uri {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "redirect_uri does not match the one the code was issued for.",
            );
        }
    }

    if let Some(challenge) = parked.code_challenge.as_deref() {
        let verifier = request.code_verifier.as_deref().unwrap_or_default();
        if !pkce_matches(challenge, verifier) {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "code_verifier does not match the code_challenge.",
            );
        }
    }

    // `expires_in` is what OAuth clients read to decide when to refresh. There
    // is no refresh grant here — a connect token is reissued from the portal, on
    // purpose, because reissuing one is a spending decision.
    let expires_in = parked
        .token_expires_at
        .map(|at| (at - Utc::now()).num_seconds().max(0));

    json_response(
        StatusCode::OK,
        json!({
            "access_token": parked.connect_token,
            "token_type": "bearer",
            "scope": TOKEN_SCOPE,
            "expires_in": expires_in,
            "expires_at": parked.token_expires_at,
            // Everything the platform needs to make its first call, so the
            // integration has no second place to look.
            "gateway_url": parked.gateway_url,
            "auth_header": TOKEN_HEADER,
            "connection_id": parked.connection_id,
            "connection_label": parked.connection_label,
            // Present only for a client that parked them (migration 082). Null
            // for every client that did not, which is every client that worked
            // before this existed.
            "wallet_id": parked.wallet_id,
            "policy_id": parked.policy_id,
            "limits": parked.limits_snapshot,
        }),
    )
}

/// The header every paid call carries. Mirrors `TOKEN_HEADER` in the buyer
/// portal's `connection-recipes.ts` — handed back with the token so an
/// integration never has to hardcode it.
pub const TOKEN_HEADER: &str = "X-Apiosk-Connect-Token";

// ---------------------------------------------------------------------------
// 3. Describe — so the portal's consent screen can name the destination
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct DescribeQuery {
    #[serde(default)]
    pub redirect_uri: Option<String>,
}

/// `GET /v1/connect/oauth/clients/:client_id?redirect_uri=…`
///
/// Public, and deliberately says nothing secret: a name to put on the consent
/// screen, and whether the callback the portal was handed is one this client is
/// allowed to use. Letting the portal find that out *before* the buyer spends a
/// minute setting up an agent is the difference between a clear "that link is
/// wrong" and a dead end at the last step.
pub async fn describe_client(
    State(state): State<Arc<AppState>>,
    Path(client_id): Path<String>,
    Query(query): Query<DescribeQuery>,
) -> Response {
    let client = match load_client(&state, &client_id).await {
        Ok(Some(client)) => client,
        Ok(None) => {
            return json_error(
                StatusCode::NOT_FOUND,
                "unknown_client",
                "That application is not registered for Apiosk Connect.",
            )
        }
        Err(response) => return response,
    };

    let redirect_uri_allowed_result = query.redirect_uri.as_deref().map(|raw| {
        parse_redirect_uri(raw)
            .map(|url| redirect_uri_allowed(&client.redirect_uri_patterns, &url))
            .unwrap_or(false)
    });

    json_response(
        StatusCode::OK,
        json!({
            "client_id": client.client_id,
            "display_name": client.display_name,
            "homepage_url": client.homepage_url,
            "requires_pkce": client.requires_pkce,
            "redirect_uri_allowed": redirect_uri_allowed_result,
        }),
    )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn load_client(
    state: &Arc<AppState>,
    client_id: &str,
) -> Result<Option<ConnectOauthClient>, Response> {
    state
        .db
        .get_connect_oauth_client(client_id.trim())
        .await
        .map_err(|err| {
            error!("connect oauth: client lookup failed: {err}");
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "lookup_failed",
                "Could not load the application.",
            )
        })
}

/// RFC 6749 §5.2 error body. Distinct from the gateway's usual
/// `{error, message}` because OAuth clients read `error`/`error_description`
/// and will render anything else as blank.
fn oauth_error(status: StatusCode, code: &str, description: &str) -> Response {
    json_response(
        status,
        json!({ "error": code, "error_description": description, "message": description }),
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(digest::digest(&digest::SHA256, bytes))
}

fn generate_code() -> Option<String> {
    let mut raw = [0u8; 32];
    SystemRandom::new().fill(&mut raw).ok()?;
    Some(format!("{CODE_PREFIX}{}", BASE64URL.encode(raw)))
}

/// A challenge is SHA-256 in base64url — 43 characters, no padding. Checking the
/// shape here turns "silently never matches" at exchange time into a clear error
/// while the integrator is still looking at their own code.
fn is_plausible_challenge(challenge: &str) -> bool {
    challenge.len() == 43
        && challenge
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Constant-time-ish comparison of `BASE64URL(SHA256(verifier))` to the stored
/// challenge. Both sides are public-ish values bound to one short-lived code, so
/// the comparison is on fixed-length digests rather than the strings.
fn pkce_matches(challenge: &str, verifier: &str) -> bool {
    // RFC 7636 §4.1 — a verifier shorter than this has too little entropy to be
    // worth accepting, and one longer is a client bug.
    if !(43..=128).contains(&verifier.len()) {
        return false;
    }
    let Ok(expected) = BASE64URL.decode(challenge) else {
        return false;
    };
    let actual = digest::digest(&digest::SHA256, verifier.as_bytes());
    bytes_equal_constant_time(&expected, actual.as_ref())
}

/// Compare two byte strings without an early return on the first differing byte.
/// Both operands here are 32-byte digests, so the length check leaks nothing an
/// attacker doesn't already know.
fn bytes_equal_constant_time(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Parse and sanity-check a callback address before it is stored or matched.
///
/// The checks are the ones that stop a redirect from becoming an exfiltration
/// channel regardless of what the allowlist says: TLS unless the callback
/// cannot leave the buyer's own network, and no fragment, since a fragment
/// would silently swallow the query we append.
fn parse_redirect_uri(raw: &str) -> Result<Url, &'static str> {
    let url = Url::parse(raw.trim()).map_err(|_| "That callback address is not a valid URL.")?;

    if url.fragment().is_some() {
        return Err("A callback address may not contain a fragment.");
    }

    match url.scheme() {
        "https" => Ok(url),
        "http" if is_private_host(url.host_str().unwrap_or_default()) => Ok(url),
        _ => Err("A callback address must use https, unless it is on a private network."),
    }
}

/// Is this host reachable only from inside the buyer's own network?
///
/// The reason to insist on https is that an authorization code must not cross
/// the public internet in the clear. A callback that cannot leave the LAN never
/// does — so loopback is not the only safe case, and treating it as the only
/// one broke the most ordinary self-hosted n8n there is: one on a home server
/// at `http://something.local:5678`, which is exactly what n8n puts in its own
/// OAuth Redirect URL field. Those buyers reached the portal, signed in, and
/// then met a portal behaving as though no platform had sent them.
///
/// What counts, and why none of them can be a public address:
///   - loopback, by name or literal
///   - a single-label host (`n8n`, `jarvis`) — no public TLD to resolve against
///   - `.local` (mDNS, RFC 6762), `.internal`, `.home.arpa` (RFC 8375) — all
///     reserved for local use and never delegated
///   - RFC 1918, CGNAT (RFC 6598) and link-local IPv4 literals
///   - IPv6 loopback, unique-local (fc00::/7) and link-local (fe80::/10)
///
/// Kept in step with `redirectProblem` in the portal's `lib/connect-handoff.ts`,
/// which applies the same rule before the buyer invests a minute in the flow.
fn is_private_host(host: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    // `Url::host_str` keeps an IPv6 literal in its brackets.
    let bare = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host.as_str());

    if bare == "localhost" {
        return true;
    }

    if let Ok(addr) = bare.parse::<std::net::Ipv6Addr>() {
        let segments = addr.segments();
        return addr.is_loopback()
            // fc00::/7 (unique-local) and fe80::/10 (link-local). Neither has a
            // stable predicate on stable Rust, so match the prefix directly.
            || (segments[0] & 0xfe00) == 0xfc00
            || (segments[0] & 0xffc0) == 0xfe80;
    }

    if let Ok(addr) = bare.parse::<std::net::Ipv4Addr>() {
        let [a, b, ..] = addr.octets();
        return addr.is_loopback()
            || addr.is_private()
            || addr.is_link_local()
            || (a == 100 && (64..=127).contains(&b));
    }

    // A name, not a literal. The fully-qualified form of a name must not get a
    // different answer from the same name without its trailing dot.
    let name = bare.strip_suffix('.').unwrap_or(bare);
    if !name.contains('.') {
        return true;
    }
    name.ends_with(".local") || name.ends_with(".internal") || name.ends_with(".home.arpa")
}

/// Does this callback match anything the client registered?
///
/// The pattern language is deliberately tiny — a leading `*.` on the host, a
/// trailing `*` on the path, or a bare `*` for clients that cannot be pinned to
/// a host at all. Anything richer is a place for a bypass to hide, and every
/// case we actually have fits in these three.
///
/// The query string is not compared. Zapier and Pipedream both append their own,
/// and it is carried through to the redirect untouched either way.
pub fn redirect_uri_allowed(patterns: &[String], candidate: &Url) -> bool {
    patterns
        .iter()
        .any(|pattern| pattern_matches(pattern.trim(), candidate))
}

fn pattern_matches(pattern: &str, candidate: &Url) -> bool {
    if pattern.is_empty() {
        return false;
    }
    // A client that may be self-hosted anywhere. `parse_redirect_uri` has
    // already required https-or-loopback; PKCE and the portal's consent screen
    // carry the rest of the weight.
    if pattern == "*" {
        return true;
    }

    let Some((scheme, rest)) = pattern.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case(candidate.scheme()) {
        return false;
    }

    let (authority_pattern, path_pattern) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };

    // Compare host:port as one string so a pattern can pin a port when it needs
    // to, and matches the default when it doesn't.
    let candidate_authority = match candidate.port() {
        Some(port) => format!("{}:{port}", candidate.host_str().unwrap_or_default()),
        None => candidate.host_str().unwrap_or_default().to_string(),
    };

    if !authority_matches(authority_pattern, &candidate_authority) {
        return false;
    }

    match path_pattern.strip_suffix('*') {
        Some(prefix) => candidate.path().starts_with(prefix),
        None => candidate.path() == path_pattern,
    }
}

fn authority_matches(pattern: &str, candidate: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let candidate = candidate.to_ascii_lowercase();

    let Some(suffix) = pattern.strip_prefix("*.") else {
        return pattern == candidate;
    };

    // `*.example.com` covers `app.example.com`, never `example.com` itself and
    // never `notexample.com`. The label being non-empty is what stops the
    // second case; requiring the dot stops the third.
    let Some(label) = candidate.strip_suffix(&format!(".{suffix}")) else {
        return false;
    };
    !label.is_empty() && !label.contains('/')
}

/// Accept either encoding of the exchange body. Most OAuth clients send
/// `application/x-www-form-urlencoded`; a few send JSON; a couple send form data
/// with no content type at all, which is why the fallback tries both rather than
/// trusting the header.
fn parse_exchange_body(headers: &HeaderMap, body: &str) -> Option<ExchangeRequest> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    if content_type.contains("json") {
        return serde_json::from_str(body).ok();
    }

    let parsed = parse_form(body);
    // An all-empty result is not a successful parse of a form — it is what a
    // JSON body looks like to a form parser. Fall through rather than returning
    // a request with every field missing.
    if parsed.code.is_some() || parsed.grant_type.is_some() {
        return Some(parsed);
    }
    serde_json::from_str(body).ok()
}

/// `application/x-www-form-urlencoded` → [`ExchangeRequest`], via the `url`
/// crate's parser (already a dependency) rather than pulling in another one for
/// five known field names.
fn parse_form(body: &str) -> ExchangeRequest {
    let mut request = ExchangeRequest::default();
    for (key, value) in url::form_urlencoded::parse(body.as_bytes()) {
        let value = value.into_owned();
        match key.as_ref() {
            "grant_type" => request.grant_type = Some(value),
            "code" => request.code = Some(value),
            "redirect_uri" => request.redirect_uri = Some(value),
            "client_id" => request.client_id = Some(value),
            "code_verifier" => request.code_verifier = Some(value),
            _ => {}
        }
    }
    request
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(raw: &str) -> Url {
        Url::parse(raw).expect("valid test url")
    }

    fn patterns(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn wildcard_client_accepts_any_checked_uri() {
        let any = patterns(&["*"]);
        assert!(redirect_uri_allowed(
            &any,
            &url("https://n8n.internal.example.com/rest/oauth2-credential/callback")
        ));
    }

    #[test]
    fn host_wildcard_matches_one_subdomain_only() {
        let make = patterns(&["https://*.make.com/oauth/cb/*"]);

        assert!(redirect_uri_allowed(
            &make,
            &url("https://eu1.make.com/oauth/cb/apiosk")
        ));
        // The bare apex is not a subdomain of itself.
        assert!(!redirect_uri_allowed(
            &make,
            &url("https://make.com/oauth/cb/apiosk")
        ));
        // A lookalike host that merely ends in the same letters.
        assert!(!redirect_uri_allowed(
            &make,
            &url("https://evilmake.com/oauth/cb/apiosk")
        ));
        // The suffix has to be a real suffix, not a prefix of the path.
        assert!(!redirect_uri_allowed(
            &make,
            &url("https://attacker.com/?x=https://eu1.make.com/oauth/cb/apiosk")
        ));
    }

    #[test]
    fn path_must_match_when_the_pattern_pins_it() {
        let zapier = patterns(&["https://zapier.com/dashboard/auth/oauth/return/*"]);

        assert!(redirect_uri_allowed(
            &zapier,
            &url("https://zapier.com/dashboard/auth/oauth/return/ApioskCLIAPI/")
        ));
        assert!(!redirect_uri_allowed(
            &zapier,
            &url("https://zapier.com/somewhere/else")
        ));
    }

    #[test]
    fn scheme_and_fragment_rules_are_enforced_before_matching() {
        assert!(parse_redirect_uri("http://example.com/cb").is_err());
        assert!(parse_redirect_uri("https://example.com/cb#frag").is_err());
        assert!(parse_redirect_uri("ftp://example.com/cb").is_err());
        assert!(parse_redirect_uri("not a url").is_err());

        // Loopback over plain http is the local-development case, and allowed.
        assert!(parse_redirect_uri("http://localhost:5678/rest/callback").is_ok());
        assert!(parse_redirect_uri("http://127.0.0.1:5678/rest/callback").is_ok());
    }

    #[test]
    fn plain_http_is_allowed_only_where_the_code_cannot_leave_the_network() {
        // The case that broke real buyers: n8n on a home server, advertising the
        // callback n8n generated for itself.
        for allowed in [
            "http://jarvis.local:5678/rest/oauth2-credential/callback",
            "http://n8n:5678/rest/callback", // docker service name, single label
            "http://192.168.1.20:5678/cb",
            "http://10.0.0.4:5678/cb",
            "http://172.16.3.9:5678/cb",
            "http://169.254.10.1:5678/cb",
            "http://100.100.0.1:5678/cb", // tailscale / CGNAT
            "http://[::1]:5678/cb",
            "http://[fd00::1]:5678/cb",
            "http://[fe80::1]:5678/cb",
            "http://box.internal/cb",
            "http://box.home.arpa/cb",
        ] {
            assert!(
                parse_redirect_uri(allowed).is_ok(),
                "{allowed} should be allowed"
            );
        }

        // Anything a code could reach the open internet over still needs TLS.
        for refused in [
            "http://n8n.example.com/cb",
            "http://8.8.8.8/cb",
            "http://11.0.0.1/cb",
            "http://172.32.0.1/cb", // just outside 172.16/12
            "http://[2001:db8::1]/cb",
            "http://notlocal.com/cb",
        ] {
            assert!(
                parse_redirect_uri(refused).is_err(),
                "{refused} should be refused"
            );
        }
    }

    #[test]
    fn pkce_round_trips_and_rejects_the_wrong_verifier() {
        // 43 chars, the RFC 7636 minimum.
        let verifier = "a".repeat(43);
        let challenge = BASE64URL.encode(digest::digest(&digest::SHA256, verifier.as_bytes()));

        assert!(is_plausible_challenge(&challenge));
        assert!(pkce_matches(&challenge, &verifier));
        assert!(!pkce_matches(&challenge, &"b".repeat(43)));
        // Too short to carry the entropy the RFC requires.
        assert!(!pkce_matches(&challenge, "short"));
    }

    #[test]
    fn exchange_body_is_read_as_form_or_json() {
        let mut form_headers = HeaderMap::new();
        form_headers.insert(
            header::CONTENT_TYPE,
            "application/x-www-form-urlencoded".parse().unwrap(),
        );
        let form = parse_exchange_body(
            &form_headers,
            "grant_type=authorization_code&code=aoc_abc&code_verifier=xyz",
        )
        .expect("form parses");
        assert_eq!(form.code.as_deref(), Some("aoc_abc"));

        let mut json_headers = HeaderMap::new();
        json_headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        let parsed = parse_exchange_body(
            &json_headers,
            r#"{"grant_type":"authorization_code","code":"aoc_abc"}"#,
        )
        .expect("json parses");
        assert_eq!(parsed.code.as_deref(), Some("aoc_abc"));

        // No content type at all: still readable, because some clients send none.
        let bare =
            parse_exchange_body(&HeaderMap::new(), "code=aoc_abc").expect("bare form parses");
        assert_eq!(bare.code.as_deref(), Some("aoc_abc"));
    }

    #[test]
    fn generated_codes_are_prefixed_and_unique() {
        let first = generate_code().expect("rng");
        let second = generate_code().expect("rng");
        assert!(first.starts_with(CODE_PREFIX));
        assert_ne!(first, second);
    }
}
