//! Who may spend a model call on `/v1/discover`.
//!
//! Reading a job is the only paid step on the discovery path: every sentence
//! that reaches [`crate::query_plan::plan`] starts a Gumloop run, and a run
//! costs credits. `/v1/discover` is public and unauthenticated on purpose —
//! agents and crawlers must be able to reach it without an account, and that
//! is the whole proposition — so the endpoint cannot be closed. What can be
//! closed is the till behind it.
//!
//! Two things already blunt the cost and neither is enough on its own:
//! [`crate::query_plan::should_parse`] keeps short keyword queries away from
//! the model, so crawlers never pay for one; and the model-output cache makes
//! a repeated sentence free. Both are trivially defeated by a caller sending a
//! thousand slightly different sentences, which is a thousand runs.
//!
//! So the parse gets a per-caller budget, and running out of it is NOT an
//! error. The caller still gets a full answer — their words searched as
//! written, which is exactly what `/v1/discover` did before any parser
//! existed. Degrading to a blunter reading is the same fallback every other
//! failure on this path takes, and it is the only one that keeps a public
//! endpoint public while a runaway caller pays for its own noise in accuracy
//! rather than in our credits.
//!
//! ---------------------------------------------------------------------------
//! The model runs for people we can name, and for nobody else
//! ---------------------------------------------------------------------------
//!
//! An anonymous caller used to get a small budget — six runs a minute, keyed on
//! IP. That is still an AI function anyone on the internet can start, and an IP
//! is a bucket, not an identity: it says who to slow down, never who spent the
//! credits. So the tier is gone. The model call now needs a NAMED
//! person, reached one of two ways — see [`naming`]:
//!
//!   1. `x-apiosk-user-jwt`, verified HERE against Supabase. No shared secret
//!      involved: a session this gateway checked itself is a stronger claim
//!      than a key, and it is how a browser reaches the parser directly.
//!
//!   2. `PARSE_KEY` + [`CALLER_USER_HEADER`], for server-side callers with no
//!      Supabase session — the MCP server, an n8n node. A connect token cannot
//!      stand in for the key: `/v1/discover` never verifies one
//!      ([`crate::auth::extract_agent_connect_token`] matches on SHAPE, and the
//!      real check happens in the paid path, which discovery is not), so a
//!      budget keyed on an unverified token is worse than none — anyone can
//!      write one, and by varying it mint a fresh bucket per request.
//!
//! Either way it ends in a name, because credits are spent per run and a run
//! nobody can attribute is a cost with no owner.
//!
//! Everyone else still gets `/v1/discover`, still gets every candidate, and
//! gets their words searched as written. The endpoint stays open — crawlers and
//! agents must reach it without an account, that is the whole proposition — and
//! what closed is the till behind it.

use std::sync::Arc;

use axum::http::HeaderMap;

use crate::portal_auth::PortalAuthError;
use crate::rate_limit::{RateLimitDecision, RateLimitPolicy};
use crate::AppState;

/// Header carrying [`crate::config::Config::parse_key`].
///
/// A header rather than a query parameter: a key in a URL is a key in access
/// logs, proxy logs and browser history.
pub const PARSE_KEY_HEADER: &str = "x-apiosk-parse-key";

/// Header a trusted caller uses to name the visitor it is calling FOR.
///
/// Honoured ONLY alongside a valid [`PARSE_KEY_HEADER`]. Read from anyone it
/// would be a free bucket generator — write a different value each request and
/// the budget is gone.
pub const CALLER_IP_HEADER: &str = "x-apiosk-caller-ip";

/// Header a trusted caller uses to name the SIGNED-IN person it is calling for.
///
/// Honoured only alongside a valid [`PARSE_KEY_HEADER`], for the same reason as
/// [`CALLER_IP_HEADER`], and required for the model to run at all: an
/// unattributable run is a credit nobody owns. The value is opaque here — the
/// surface that holds the key is the one that verified the session, and this
/// gateway never sees the JWT.
pub const CALLER_USER_HEADER: &str = "x-apiosk-caller-user";

/// The ceiling on everything the key is used for, whoever it is used for.
/// High enough to be no constraint on our own surfaces, low enough that a
/// leaked key cannot drain the account before anyone notices.
const TRUSTED: RateLimitPolicy = RateLimitPolicy::per_minute(600);

/// One visitor arriving through a trusted surface — the signed-in person, and
/// the address they came from when the surface names it. Bounded because a
/// proxy that spent its whole ceiling on one reloading tab would leave nothing
/// for everyone else on the site.
const TRUSTED_VISITOR: RateLimitPolicy = RateLimitPolicy::per_minute(30);

/// The ceiling on OUTBOUND identity checks, across everybody.
///
/// `/v1/discover` needs no credential, so anyone can make this gateway call
/// Supabase `GET /auth/v1/user` simply by attaching a header. A repeated token
/// costs one call and is then cached either way, but a caller varying the token
/// every request defeats that — and the auth server they would be loading is
/// the same one the credits top-up and the Connect owner endpoints depend on.
/// An outage there is a paying user unable to add funds, caused from an
/// endpoint that asks for no account at all.
///
/// So the outbound call gets its own bucket, before it is made. Generous
/// against real traffic — a signed-in visitor costs one verification a minute,
/// so this is hundreds of concurrent people — and a hard stop against a
/// caller inventing tokens. Running out degrades exactly like every other
/// limit here: the search is answered from the index instead of being read.
const VERIFICATIONS: RateLimitPolicy = RateLimitPolicy::per_minute(300);

/// Whether this request carries the configured key.
///
/// No key configured means nobody is trusted, rather than everybody: a gateway
/// that has not been given one must not behave as though every caller had it.
fn holds_key(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(expected) = state.config.parse_key.as_deref() else {
        return false;
    };
    headers
        .get(PARSE_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .is_some_and(|given| !given.is_empty() && given == expected)
}

/// The buckets this request counts against, and how big each is.
///
/// Only ever built for a request that already named a person — an unattributable
/// one does not reach here, it simply gets no model call. See [`allows_parse`].
fn buckets(headers: &HeaderMap, user: &str) -> Vec<(String, RateLimitPolicy)> {
    // A trusted proxy stands in front of many people. Budgeting it as one
    // caller means the first visitor to reload a page can spend what the rest
    // of the site's traffic needs, so the person it names is budgeted too — and
    // the key's own ceiling still applies over the top. Narrow buckets first: a
    // visitor already over their limit must not also consume a slot from the
    // ceiling on the way to being told so.
    let mut out = vec![(format!("parse:user:{user}"), TRUSTED_VISITOR)];
    if let Some(address) = forwarded_caller(headers) {
        out.push((format!("parse:visitor:{address}"), TRUSTED_VISITOR));
    }
    out.push(("parse:trusted".to_string(), TRUSTED));
    out
}

/// The visitor a trusted caller says it is acting for, if it said.
fn forwarded_caller(headers: &HeaderMap) -> Option<String> {
    read_short(headers, CALLER_IP_HEADER)
}

/// The signed-in person a trusted caller is acting for.
///
/// Whatever id that surface authenticates people by — a Supabase user id, in
/// practice. Bounded in length for the same reason the address is: it becomes
/// part of a rate-limit key.
pub fn forwarded_user(headers: &HeaderMap) -> Option<String> {
    read_short(headers, CALLER_USER_HEADER)
}

/// A stable, non-reversible handle for one session token.
///
/// Hex-encoded SHA-256, prefixed so an entry can never be confused with
/// another cache's key. The raw token never becomes a key, never reaches a log
/// line, and never leaves this function.
fn token_fingerprint(jwt: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"parse-identity:v1:");
    hasher.update(jwt.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn read_short(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 64)
        .map(str::to_string)
}

/// WHO this request is for, or nothing.
///
/// Two ways to arrive at a name, and they are not equally strong:
///
///   1. A SESSION THIS GATEWAY VERIFIED ITSELF. `x-apiosk-user-jwt` is handed
///      to Supabase `GET /auth/v1/user` by [`crate::portal_auth`], the same
///      call the credits path and the Connect endpoints already make, and
///      nothing the token claims is trusted. This needs no shared secret at
///      all: a verified session is a stronger claim than a key, because it
///      cannot be lifted out of a JavaScript bundle, it names one person, and
///      the budget below is already keyed per person.
///
///   2. A TRUSTED SURFACE NAMING SOMEBODY FOR US. `PARSE_KEY` plus
///      [`CALLER_USER_HEADER`], for callers with no Supabase session to
///      present — the MCP server, an n8n node, anything server-side. Here the
///      key is doing real work: it is the only thing making the forwarded name
///      mean anything.
///
/// The browser used to have to go through (2) via an Edge Function, because
/// `PARSE_KEY` cannot ship in a bundle. That put a second, separately-written
/// identity check in front of the one this gateway already had — and a
/// signed-in user who failed it silently lost the reading of their job. (1)
/// removes the proxy, the secret and the duplicate implementation together.
///
/// Order matters: the self-verified session is checked first, so a request
/// carrying both is judged on the claim this gateway can prove.
async fn naming(state: &Arc<AppState>, headers: &HeaderMap) -> Option<String> {
    if let Some(jwt) = crate::auth::extract_credits_user_jwt(headers) {
        // Hashed, never held. The token is a live credential and this key ends
        // up in memory; a digest answers "is this the same caller as a moment
        // ago" without keeping the thing that would let anyone be them.
        let fingerprint = token_fingerprint(&jwt);
        let verified = state
            .cache
            .parse_identity(&fingerprint, async {
                // Inside the init future, so it is paid only on a cache MISS
                // and only once per coalesced group. Refusing here returns an
                // `Err`, which `try_get_with` does not store — so the next
                // request once the bucket refills asks properly rather than
                // inheriting a refusal that was never about this caller.
                if !matches!(
                    state
                        .rate_limiter
                        .check("parse:verify", VERIFICATIONS)
                        .await,
                    RateLimitDecision::Allowed { .. }
                ) {
                    return Err(PortalAuthError::Unavailable);
                }
                match crate::portal_auth::resolve_portal_user_id(state, &jwt, "discover").await {
                    Ok(id) => Ok(Some(id.to_string())),
                    // The auth server LOOKED and said no. A settled answer, and
                    // one worth remembering — see `EndpointCache::parse_identity`.
                    Err(PortalAuthError::Invalid) => Ok(None),
                    // We could not ask. A fact about us, not about the caller:
                    // it must not be cached, and `try_get_with` does not store
                    // errors.
                    Err(other) => Err(other),
                }
            })
            .await;
        // Every path that is not a verified id ends the same way — no name, so
        // the deterministic reading. An unreachable auth server is not a reason
        // to fail a public search.
        return verified.ok().flatten();
    }

    if holds_key(state, headers) {
        return forwarded_user(headers);
    }
    None
}

/// Whether this caller may spend a model call on this request.
///
/// `false` never means "refused" — see the module header. It means the caller
/// gets the deterministic reading instead of the model's.
pub async fn allows_parse(state: &Arc<AppState>, headers: &HeaderMap) -> bool {
    let Some(user) = naming(state, headers).await else {
        return false;
    };

    for (key, policy) in buckets(headers, &user) {
        if !matches!(
            state.rate_limiter.check(&key, policy).await,
            RateLimitDecision::Allowed { .. }
        ) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        map
    }

    /* -------------------------------------------------------------------
    Which credential names the caller

    `naming` needs an AppState to reach Supabase, so these test the routing
    decision it makes BEFORE that call — which of the two credentials a
    given set of headers selects. That routing is the part that changed, and
    the part a future edit could quietly undo: dropping the JWT branch would
    send every browser back through a shared secret it cannot hold, and the
    symptom would be a perfectly ordinary-looking search with the job
    unread.
    ------------------------------------------------------------------ */

    /// Which branch of `naming` a request takes, without the network call.
    #[derive(Debug, PartialEq, Eq)]
    enum Route {
        /// Verified here against Supabase.
        Session,
        /// A trusted surface asserting a name, on the strength of the key.
        ForwardedByKey,
        /// Nobody we can name; the deterministic reading.
        Anonymous,
    }

    fn route(has_valid_key: bool, headers: &HeaderMap) -> Route {
        if crate::auth::extract_credits_user_jwt(headers).is_some() {
            return Route::Session;
        }
        if has_valid_key && forwarded_user(headers).is_some() {
            return Route::ForwardedByKey;
        }
        Route::Anonymous
    }

    #[test]
    fn a_browser_session_needs_no_shared_secret() {
        // The change in one line: a signed-in visitor reaches the parser with
        // their own token and nothing else. No PARSE_KEY, no proxy.
        let h = headers(&[("x-apiosk-user-jwt", "a.real.jwt")]);
        assert_eq!(route(false, &h), Route::Session);
    }

    #[test]
    fn a_session_wins_over_a_forwarded_name() {
        // A request carrying both is judged on the claim this gateway can
        // prove itself, not on one it is asked to take on trust.
        let h = headers(&[
            ("x-apiosk-user-jwt", "a.real.jwt"),
            (CALLER_USER_HEADER, "someone-else"),
        ]);
        assert_eq!(route(true, &h), Route::Session);
    }

    #[test]
    fn a_server_side_caller_still_names_someone_through_the_key() {
        // The MCP server and the n8n node have no Supabase session to present.
        let h = headers(&[(CALLER_USER_HEADER, "user-1")]);
        assert_eq!(route(true, &h), Route::ForwardedByKey);
    }

    #[test]
    fn a_forwarded_name_without_the_key_is_worth_nothing() {
        // Otherwise the header is a free bucket generator: write a different
        // value each request and the budget is gone.
        let h = headers(&[(CALLER_USER_HEADER, "user-1")]);
        assert_eq!(route(false, &h), Route::Anonymous);
    }

    #[test]
    fn a_key_with_nobody_named_is_still_nobody() {
        let h = headers(&[]);
        assert_eq!(route(true, &h), Route::Anonymous);
    }

    #[test]
    fn an_anonymous_crawler_is_named_by_neither() {
        let h = headers(&[]);
        assert_eq!(route(false, &h), Route::Anonymous);
    }

    /* -------------------------------------------------------------------
    The token never becomes a cache key
    ------------------------------------------------------------------ */

    #[test]
    fn the_cache_key_is_a_digest_and_never_the_token() {
        let jwt = "eyJhbGciOi.THIS-IS-A-LIVE-CREDENTIAL.signature";
        let key = token_fingerprint(jwt);

        assert!(
            !key.contains(jwt),
            "the raw token must never appear in a key"
        );
        assert!(!key.contains("THIS-IS-A-LIVE-CREDENTIAL"));
        // Hex SHA-256, so it is fixed width whatever the token's length.
        assert_eq!(key.len(), 64);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn one_token_is_one_key_and_two_tokens_are_two() {
        let a = token_fingerprint("token-a");
        assert_eq!(a, token_fingerprint("token-a"), "stable across calls");
        assert_ne!(a, token_fingerprint("token-b"));
        // A token differing by one character must not collide onto another
        // person's cached identity.
        assert_ne!(token_fingerprint("token-a"), token_fingerprint("token-A"));
    }

    /// The two fields `bucket` reads, without building a whole AppState.
    fn key_matches(configured: Option<&str>, presented: Option<&str>) -> bool {
        let Some(expected) = configured else {
            return false;
        };
        presented
            .map(str::trim)
            .is_some_and(|given| !given.is_empty() && given == expected)
    }

    #[test]
    fn the_key_is_checked_against_the_configured_value() {
        assert!(key_matches(Some("s3cret"), Some("s3cret")));
        assert!(!key_matches(Some("s3cret"), Some("guess")));
        assert!(!key_matches(Some("s3cret"), None));
        // Whitespace is not a key.
        assert!(!key_matches(Some("s3cret"), Some("   ")));
    }

    #[test]
    fn no_configured_key_trusts_nobody() {
        // The inverse — treating "unset" as "everyone is trusted" — turns a
        // forgotten deploy variable into an open till.
        assert!(!key_matches(None, Some("anything")));
        assert!(!key_matches(None, None));
    }

    #[test]
    fn a_request_that_names_nobody_gets_no_model_call() {
        // The whole rule: an address is a bucket, not an identity, and an AI
        // function anyone can start is one anyone can spend. Without a named
        // person there is no bucket to build at all.
        assert!(forwarded_user(&headers(&[("fly-client-ip", "203.0.113.9")])).is_none());
        assert!(forwarded_user(&HeaderMap::new()).is_none());
    }

    #[test]
    fn the_signed_in_person_is_the_bucket_the_run_counts_against() {
        let map = headers(&[
            (
                "x-apiosk-caller-user",
                "bf085fe8-41c4-4699-978f-e75e89518bf7",
            ),
            ("x-apiosk-caller-ip", "203.0.113.9"),
        ]);
        let user = forwarded_user(&map).unwrap();
        let keys: Vec<String> = buckets(&map, &user).into_iter().map(|(k, _)| k).collect();
        assert_eq!(
            keys,
            vec![
                "parse:user:bf085fe8-41c4-4699-978f-e75e89518bf7".to_string(),
                "parse:visitor:203.0.113.9".to_string(),
                "parse:trusted".to_string(),
            ]
        );
    }

    #[test]
    fn a_forwarded_user_is_bounded_like_every_other_key_part() {
        // A key is a cache key, and this one is written by the caller.
        let long = "x".repeat(200);
        assert!(forwarded_user(&headers(&[("x-apiosk-caller-user", long.as_str())])).is_none());
        assert!(forwarded_user(&headers(&[("x-apiosk-caller-user", "  ")])).is_none());
    }

    #[test]
    fn a_forwarded_visitor_is_read_and_bounded() {
        assert_eq!(
            forwarded_caller(&headers(&[("x-apiosk-caller-ip", "203.0.113.9")])).as_deref(),
            Some("203.0.113.9")
        );
        assert!(forwarded_caller(&headers(&[("x-apiosk-caller-ip", "   ")])).is_none());
        assert!(forwarded_caller(&HeaderMap::new()).is_none());

        // A key is a cache key. An unbounded one is an unbounded cache, and
        // the value comes from a caller.
        let long = "x".repeat(200);
        assert!(forwarded_caller(&headers(&[("x-apiosk-caller-ip", long.as_str())])).is_none());
    }

    #[test]
    fn a_trusted_proxy_is_budgeted_per_visitor_and_overall() {
        // Both, not either. Per-visitor alone lets a botnet through one proxy
        // spend without limit; the ceiling alone lets one reloading tab spend
        // what the whole site needs.
        assert!(TRUSTED_VISITOR.max_requests < TRUSTED.max_requests);
    }

    #[test]
    fn a_connect_token_buys_nothing_here() {
        // It is matched on shape and never verified on this path, so a budget
        // keyed on it could be minted at will. Carrying one names nobody, and
        // naming nobody is now the end of it.
        let map = headers(&[
            ("x-apiosk-connect-token", "aw_a_b_cccccccccccccccc"),
            ("fly-client-ip", "203.0.113.9"),
        ]);
        assert!(forwarded_user(&map).is_none());
    }
}
