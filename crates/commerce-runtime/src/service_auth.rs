//! Service-key auth: Apiosk's own surfaces spending from the treasury.
//!
//! The app's `use-selection` edge function pays for every call out of the
//! treasury — the user's balance is debited in the app's own database, and
//! the gateway is asked to pay the provider. It used to authenticate like any
//! agent would, with a connect token minted in the buyer portal and copied
//! into the function's secrets. That put a buyer credential for the treasury
//! wallet in a second place, and meant somebody had to mint and paste it.
//!
//! Now the function presents a **service key** (`X-Apiosk-Service-Key`), a
//! secret the operator sets on both sides (`APIOSK_APP_SERVICE_KEY` here,
//! the same value in the function's secrets) and nowhere else. The gateway
//! resolves that to the treasury's connect token on its own, by the token's
//! id or visible prefix (`APIOSK_TREASURY_CONNECT_TOKEN_REF`) — the raw token
//! is never needed by anyone. Everything downstream — wallet selection, caps,
//! idempotency, settlement, the request log — runs exactly as it does for a
//! connect-token caller, because what comes out of here IS that token.
//!
//! Fail closed: a presented key that does not match, a gateway with no key
//! configured, or no treasury reference, all answer 401 with a code that
//! says which. A key that is absent is simply not this path.

use std::sync::Arc;

use axum::http::HeaderMap;

use crate::db::AgentConnectTokenLookup;
use crate::AppState;

/// The header the edge function sends. Never accepted on `Authorization`,
/// so it cannot be confused with a bearer connect token or an API key.
pub const HEADER_SERVICE_KEY: &str = "x-apiosk-service-key";

/// Beside a valid service key, the App asks for a non-default buyer markup on
/// this call (`2000` for a workflow run priced at list + 20%). Read from no
/// other principal: a connect token, an x402 buyer or a browser sending it is
/// ignored and priced at the standard 15%.
pub const HEADER_BUYER_MARKUP_BPS: &str = "x-apiosk-buyer-markup-bps";

/// The service key as presented, if any.
pub fn presented(headers: &HeaderMap) -> Option<String> {
    headers
        .get(HEADER_SERVICE_KEY)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Constant-time comparison, so a wrong key costs the same as a right one
/// however many leading bytes it got right. Only the length is allowed to
/// short-circuit, and the length of a random key is not a secret.
pub fn key_matches(expected: &str, presented: &str) -> bool {
    let (a, b) = (expected.as_bytes(), presented.as_bytes());
    if a.is_empty() || a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The markup the request asks for, parsed and normalised through
/// [`crate::fees::allowed_markup`]. **Untrusted**: says nothing about
/// who asked — use [`buyer_markup_bps`] for anything that prices money.
fn requested_buyer_markup_bps(headers: &HeaderMap) -> i128 {
    headers
        .get(HEADER_BUYER_MARKUP_BPS)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<i128>().ok())
        .map(crate::fees::allowed_markup)
        .unwrap_or(crate::fees::BUYER_MARKUP_BPS)
}

/// The buyer markup, in basis points, this request is priced at.
///
/// `X-Apiosk-Buyer-Markup-Bps` is honoured **only** when the request carries an
/// `X-Apiosk-Service-Key` that matches the configured app service key
/// (`expected_key`, i.e. `config.app_service_key`) — the same constant-time
/// check [`resolve`] makes. Every other principal, a gateway with no key
/// configured, a wrong key, a missing or unparsable header, and any value other
/// than 1500/2000 all price at [`crate::fees::BUYER_MARKUP_BPS`].
///
/// Deliberately synchronous and free of the treasury lookup: the paid path
/// separately refuses any purchase whose service key does not resolve to the
/// treasury (`treasury_purchase_required`), so a valid key is the whole trust
/// question for the markup.
pub fn buyer_markup_bps(expected_key: Option<&str>, headers: &HeaderMap) -> i128 {
    let standard = crate::fees::BUYER_MARKUP_BPS;
    let (Some(expected), Some(key)) = (expected_key, presented(headers)) else {
        return standard;
    };
    if !key_matches(expected, &key) {
        return standard;
    }
    requested_buyer_markup_bps(headers)
}

/// What presenting a service key resolved to.
pub enum ServiceAuth {
    /// No service key on the request — not this path.
    NotPresented,
    /// A key was presented and refused; `(code, message)` for the 401.
    Rejected(&'static str, &'static str),
    /// The key matched; this is the treasury's connect token, looked up the
    /// same way a presented token would be, so revoked/expired read the same.
    Resolved(AgentConnectTokenLookup),
}

pub async fn resolve(state: &Arc<AppState>, headers: &HeaderMap) -> ServiceAuth {
    let Some(key) = presented(headers) else {
        return ServiceAuth::NotPresented;
    };
    let Some(expected) = state.config.app_service_key.as_deref() else {
        return ServiceAuth::Rejected(
            "service_key_not_configured",
            "This gateway accepts no service key. Set APIOSK_APP_SERVICE_KEY on the gateway.",
        );
    };
    if !key_matches(expected, &key) {
        return ServiceAuth::Rejected("service_key_invalid", "The service key is not recognized.");
    }
    let Some(reference) = state.config.treasury_connect_token_ref.as_deref() else {
        return ServiceAuth::Rejected(
            "treasury_not_configured",
            "No treasury connect token is configured. Set APIOSK_TREASURY_CONNECT_TOKEN_REF on the gateway to the treasury token's id or prefix.",
        );
    };
    match state.db.lookup_agent_connect_token_by_ref(reference).await {
        Ok(lookup) => ServiceAuth::Resolved(lookup),
        Err(err) => {
            tracing::error!("treasury connect token lookup failed: {err}");
            ServiceAuth::Rejected(
                "treasury_lookup_failed",
                "The treasury connect token could not be resolved.",
            )
        }
    }
}

/// How a paying caller identified itself — the thing `/v1/do` has to carry
/// onto its loopback request unchanged, so the proxy sees the same caller.
pub enum Credential {
    ConnectToken(String),
    ServiceKey {
        key: String,
        /// The markup the service asked for (1500 or 2000). Only meaningful
        /// once the key has been verified — `/v1/do` refuses an unverified key
        /// before it reads this — and the proxy re-derives it on loopback with
        /// [`buyer_markup_bps`], so this value never prices money on its own.
        buyer_markup_bps: i128,
    },
}

impl Credential {
    /// The service key wins when both are present: a request that carries one
    /// is Apiosk's own surface, and a stray connect token beside it is noise.
    pub fn from_headers(headers: &HeaderMap) -> Option<Self> {
        if let Some(key) = presented(headers) {
            return Some(Self::ServiceKey {
                key,
                buyer_markup_bps: requested_buyer_markup_bps(headers),
            });
        }
        crate::auth::extract_agent_connect_token(headers).map(Self::ConnectToken)
    }

    /// The header name and value to forward.
    pub fn header(&self) -> (&'static str, &str) {
        match self {
            Self::ConnectToken(token) => ("x-apiosk-connect-token", token),
            Self::ServiceKey { key, .. } => (HEADER_SERVICE_KEY, key),
        }
    }

    /// The buyer markup this caller's calls are priced at. A connect token is
    /// always the standard 15%.
    pub fn buyer_markup_bps(&self) -> i128 {
        match self {
            Self::ConnectToken(_) => crate::fees::BUYER_MARKUP_BPS,
            Self::ServiceKey {
                buyer_markup_bps, ..
            } => crate::fees::allowed_markup(*buyer_markup_bps),
        }
    }

    /// The markup header to carry onto a loopback call, when it differs from
    /// the default. Only a service key ever forwards one.
    pub fn markup_header(&self) -> Option<(&'static str, String)> {
        let bps = self.buyer_markup_bps();
        match self {
            Self::ServiceKey { .. } if bps != crate::fees::BUYER_MARKUP_BPS => {
                Some((HEADER_BUYER_MARKUP_BPS, bps.to_string()))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn the_key_is_read_from_its_own_header_only() {
        let mut headers = HeaderMap::new();
        assert!(presented(&headers).is_none());
        headers.insert("authorization", HeaderValue::from_static("Bearer svc_abc"));
        assert!(
            presented(&headers).is_none(),
            "a bearer value is never a service key"
        );
        headers.insert(HEADER_SERVICE_KEY, HeaderValue::from_static("  svc_abc  "));
        assert_eq!(presented(&headers).as_deref(), Some("svc_abc"));
        headers.insert(HEADER_SERVICE_KEY, HeaderValue::from_static("   "));
        assert!(presented(&headers).is_none(), "blank is absent");
    }

    #[test]
    fn matching_is_exact() {
        assert!(key_matches("svc_abc", "svc_abc"));
        assert!(!key_matches("svc_abc", "svc_abd"));
        assert!(!key_matches("svc_abc", "svc_ab"));
        assert!(!key_matches("svc_abc", ""));
        assert!(!key_matches("", "svc_abc"));
    }

    #[test]
    fn the_service_key_outranks_a_connect_token_and_forwards_as_itself() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-apiosk-connect-token",
            HeaderValue::from_static("aw_live_token"),
        );
        match Credential::from_headers(&headers) {
            Some(Credential::ConnectToken(t)) => assert_eq!(t, "aw_live_token"),
            _ => panic!("a connect token alone is a connect token"),
        }
        headers.insert(HEADER_SERVICE_KEY, HeaderValue::from_static("svc_abc"));
        let credential = Credential::from_headers(&headers).expect("credential");
        assert!(matches!(credential, Credential::ServiceKey { .. }));
        assert_eq!(credential.header(), (HEADER_SERVICE_KEY, "svc_abc"));
    }

    fn with(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(*name, HeaderValue::from_static(value));
        }
        headers
    }

    #[test]
    fn a_valid_service_key_may_ask_for_the_workflow_markup() {
        let headers = with(&[
            (HEADER_SERVICE_KEY, "svc_abc"),
            (HEADER_BUYER_MARKUP_BPS, "2000"),
        ]);
        assert_eq!(buyer_markup_bps(Some("svc_abc"), &headers), 2_000);
        let padded = with(&[
            (HEADER_SERVICE_KEY, "svc_abc"),
            (HEADER_BUYER_MARKUP_BPS, " 2000 "),
        ]);
        assert_eq!(buyer_markup_bps(Some("svc_abc"), &padded), 2_000);
        let standard = with(&[
            (HEADER_SERVICE_KEY, "svc_abc"),
            (HEADER_BUYER_MARKUP_BPS, "1500"),
        ]);
        assert_eq!(buyer_markup_bps(Some("svc_abc"), &standard), 1_500);
    }

    #[test]
    fn the_markup_header_is_ignored_without_a_valid_service_key() {
        let markup = (HEADER_BUYER_MARKUP_BPS, "2000");
        // No key at all.
        assert_eq!(buyer_markup_bps(Some("svc_abc"), &with(&[markup])), 1_500);
        // A connect token, or a bearer value, is not a service key.
        assert_eq!(
            buyer_markup_bps(
                Some("svc_abc"),
                &with(&[("x-apiosk-connect-token", "aw_live_token"), markup])
            ),
            1_500
        );
        assert_eq!(
            buyer_markup_bps(
                Some("svc_abc"),
                &with(&[("authorization", "Bearer svc_abc"), markup])
            ),
            1_500
        );
        // A wrong key.
        assert_eq!(
            buyer_markup_bps(
                Some("svc_abc"),
                &with(&[(HEADER_SERVICE_KEY, "svc_abd"), markup])
            ),
            1_500
        );
        // A gateway with no key configured trusts nobody.
        assert_eq!(
            buyer_markup_bps(None, &with(&[(HEADER_SERVICE_KEY, "svc_abc"), markup])),
            1_500
        );
        // A valid key and no header.
        assert_eq!(
            buyer_markup_bps(Some("svc_abc"), &with(&[(HEADER_SERVICE_KEY, "svc_abc")])),
            1_500
        );
    }

    #[test]
    fn only_1500_and_2000_are_markups_even_for_the_service() {
        for value in [
            "0", "1999", "2001", "2500", "10000", "-2000", "20%", "2000.0", "", "abc",
        ] {
            let mut headers = with(&[(HEADER_SERVICE_KEY, "svc_abc")]);
            headers.insert(
                HEADER_BUYER_MARKUP_BPS,
                HeaderValue::from_str(value).expect("header value"),
            );
            assert_eq!(
                buyer_markup_bps(Some("svc_abc"), &headers),
                1_500,
                "{value:?} must price at 15%"
            );
        }
    }

    #[test]
    fn the_credential_carries_and_forwards_the_markup_only_for_a_service_key() {
        let token = Credential::from_headers(&with(&[
            ("x-apiosk-connect-token", "aw_live_token"),
            (HEADER_BUYER_MARKUP_BPS, "2000"),
        ]))
        .expect("credential");
        assert_eq!(token.buyer_markup_bps(), 1_500);
        assert!(token.markup_header().is_none());

        let service = Credential::from_headers(&with(&[
            (HEADER_SERVICE_KEY, "svc_abc"),
            (HEADER_BUYER_MARKUP_BPS, "2000"),
        ]))
        .expect("credential");
        assert_eq!(service.buyer_markup_bps(), 2_000);
        assert_eq!(
            service.markup_header(),
            Some((HEADER_BUYER_MARKUP_BPS, "2000".to_string()))
        );

        let default = Credential::from_headers(&with(&[(HEADER_SERVICE_KEY, "svc_abc")]))
            .expect("credential");
        assert_eq!(default.buyer_markup_bps(), 1_500);
        assert!(default.markup_header().is_none());

        let bogus = Credential::from_headers(&with(&[
            (HEADER_SERVICE_KEY, "svc_abc"),
            (HEADER_BUYER_MARKUP_BPS, "3000"),
        ]))
        .expect("credential");
        assert_eq!(bogus.buyer_markup_bps(), 1_500);
        assert!(bogus.markup_header().is_none());
    }
}
