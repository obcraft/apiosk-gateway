/// Attach to the v2 process's one tracing subscriber. Keep the commerce
/// runtime's existing error/breadcrumb reporting without exporting new v2
/// application events or installing another global subscriber.
pub fn error_reporting_layer<S>() -> sentry::integrations::tracing::SentryLayer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    use sentry::integrations::tracing::EventFilter;
    sentry::integrations::tracing::layer()
        .event_filter(|metadata| {
            if !commerce_metadata(metadata) {
                return EventFilter::Ignore;
            }
            match *metadata.level() {
                tracing::Level::ERROR => EventFilter::Event,
                tracing::Level::WARN | tracing::Level::INFO => EventFilter::Breadcrumb,
                tracing::Level::DEBUG | tracing::Level::TRACE => EventFilter::Ignore,
            }
        })
        .span_filter(|metadata| {
            commerce_metadata(metadata)
                && matches!(
                    *metadata.level(),
                    tracing::Level::ERROR | tracing::Level::WARN | tracing::Level::INFO
                )
        })
}

fn commerce_metadata(metadata: &tracing::Metadata<'_>) -> bool {
    metadata.target().starts_with("commerce_runtime")
        || metadata
            .module_path()
            .is_some_and(|module| module.starts_with("commerce_runtime"))
        || metadata.target() == "apiosk::pay::receipt"
}

/// Initializes Sentry from env. Returns a guard that must live for the whole
/// process (held by the sole v2 entrypoint). When `SENTRY_DSN` is unset/empty the client is
/// disabled — a complete no-op (no events, negligible overhead), so local dev
/// and DSN-less deploys neither error nor spam.
pub fn init_error_reporting() -> sentry::ClientInitGuard {
    let dsn = std::env::var("SENTRY_DSN").ok().and_then(|v| {
        let t = v.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    });

    let environment = std::env::var("SENTRY_ENVIRONMENT")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(std::borrow::Cow::Owned);

    // Prefer an explicit deploy release (git SHA); fall back to crate name@version.
    let release = std::env::var("SENTRY_RELEASE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(std::borrow::Cow::Owned)
        .or_else(|| sentry::release_name!());

    let traces_sample_rate = std::env::var("SENTRY_TRACES_SAMPLE_RATE")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .unwrap_or(0.1);

    sentry::init((
        dsn,
        sentry::ClientOptions {
            release,
            environment,
            traces_sample_rate,
            // Non-custodial payment proxy that forwards arbitrary bytes: never
            // auto-collect client IPs, cookies, request bodies, or identifying
            // headers.
            send_default_pii: false,
            before_send: Some(std::sync::Arc::new(scrub_event)),
            before_breadcrumb: Some(std::sync::Arc::new(scrub_breadcrumb)),
            ..Default::default()
        },
    ))
}

/// Scrubs error events before they leave the process. Runs for ERROR events
/// only (not transactions); transactions carry no bodies/headers here.
fn scrub_event(
    mut event: sentry::protocol::Event<'static>,
) -> Option<sentry::protocol::Event<'static>> {
    if let Some(request) = event.request.as_mut() {
        // Never ship request/response bodies or cookies.
        request.data = None;
        request.cookies = None;
        // Query strings can carry provider API keys (?token=…), GA secrets, etc.
        request.query_string = None;
        if let Some(url) = request.url.as_mut() {
            url.set_query(None);
        }
        // Deny-list credential / wallet / PII headers.
        request.headers.retain(|name, _| !is_sensitive_header(name));
    }
    Some(event)
}

/// Strips query strings from breadcrumb URLs (the GA call embeds `?api_secret=`,
/// proxied upstreams use `?token=<provider key>`).
fn scrub_breadcrumb(mut crumb: sentry::Breadcrumb) -> Option<sentry::Breadcrumb> {
    if let Some(serde_json::Value::String(url)) = crumb.data.get_mut("url") {
        if let Some(idx) = url.find('?') {
            url.truncate(idx);
        }
    }
    Some(crumb)
}

/// Deny-list of headers that must never reach Sentry. Mirrors the auth/payment
/// sensitive-header surfaces in src/auth.rs and src/payment.rs.
fn is_sensitive_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "set-cookie"
            | "x-apiosk-user-jwt"
            | "x-apiosk-connect-token"
            | "x-apio-connect-token"
            | "x-connect-token"
            | "x-apiosk-session-id"
            | "x-apiosk-wallet-used"
            | "x-apiosk-tx-hash"
            | "x-forwarded-for"
            | "x-real-ip"
            | "fly-client-ip"
            | "x-api-key"
            | "api-key"
            | "x-goog-api-key"
    ) || n.starts_with("x-payment")
        || n.starts_with("x-wallet-")
}
