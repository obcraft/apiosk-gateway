use crate::html::{escape_html, format_price, html_footer, html_head, Nav};

use super::models::{
    ApiDetailResponse, ApiInfoResponse, EndpointDocResponse, ProviderInfoResponse, TypesResponse,
};

fn render_inline_markdown_safe(value: &str) -> String {
    let mut html = String::new();
    for (index, part) in value.split('`').enumerate() {
        if index % 2 == 1 {
            html.push_str(&format!("<code>{}</code>", escape_html(part)));
        } else {
            html.push_str(&escape_html(part));
        }
    }
    html
}

/// Render the small, provider-authored Markdown field without allowing raw HTML.
/// Headings, paragraphs, line breaks and inline code cover the endpoint-docs UI
/// while keeping the public catalog safe from script/markup injection.
fn render_markdown_safe(value: &str) -> String {
    let mut html = String::new();
    let mut paragraph: Vec<&str> = Vec::new();

    let flush_paragraph = |html: &mut String, paragraph: &mut Vec<&str>| {
        if paragraph.is_empty() {
            return;
        }
        html.push_str("<p>");
        html.push_str(
            &paragraph
                .iter()
                .map(|line| render_inline_markdown_safe(line.trim()))
                .collect::<Vec<_>>()
                .join("<br>"),
        );
        html.push_str("</p>");
        paragraph.clear();
    };

    for line in value.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            flush_paragraph(&mut html, &mut paragraph);
            continue;
        }
        let heading = if let Some(text) = trimmed.strip_prefix("### ") {
            Some(("h3", text))
        } else if let Some(text) = trimmed.strip_prefix("## ") {
            Some(("h2", text))
        } else {
            trimmed.strip_prefix("# ").map(|text| ("h1", text))
        };
        if let Some((tag, text)) = heading {
            flush_paragraph(&mut html, &mut paragraph);
            html.push_str(&format!(
                "<{tag}>{text}</{tag}>",
                tag = tag,
                text = render_inline_markdown_safe(text)
            ));
        } else {
            paragraph.push(trimmed);
        }
    }
    flush_paragraph(&mut html, &mut paragraph);
    html
}

fn pricing_operator_label(operator: &str) -> &str {
    match operator {
        "equals" => "=",
        "not_equals" => "!=",
        "not_contains" => "does not contain",
        "starts_with" => "starts with",
        "ends_with" => "ends with",
        "not_exists" => "does not exist",
        other => other,
    }
}

fn json_price(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(|price| {
            price
                .as_f64()
                .or_else(|| price.as_str().and_then(|text| text.parse::<f64>().ok()))
        })
        .map(format_price)
        .unwrap_or_else(|| "Not set".to_string())
}

fn render_pricing(default_price: f64, rules: &serde_json::Value) -> String {
    let mut html = format!(
        r#"<div class="price-default">{} default</div>"#,
        format_price(default_price)
    );
    let Some(rules) = rules.as_array() else {
        return html;
    };
    for rule in rules {
        let parameter = rule
            .get("parameter_name")
            .and_then(|value| value.as_str())
            .unwrap_or("parameter");
        let operator = rule
            .get("operator")
            .and_then(|value| value.as_str())
            .unwrap_or("equals");
        let comparison = rule
            .get("value")
            .or_else(|| rule.get("comparison_value"))
            .and_then(|value| value.as_str());
        let value = if matches!(operator, "exists" | "not_exists") {
            String::new()
        } else {
            format!(" {}", comparison.unwrap_or("(empty)"))
        };
        let priority = rule
            .get("priority")
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        html.push_str(&format!(
            r#"<div class="pricing-rule"><span class="priority-pill">P{priority}</span>{parameter} {operator}{value} &rarr; {price}</div>"#,
            priority = priority,
            parameter = escape_html(parameter),
            operator = escape_html(pricing_operator_label(operator)),
            value = escape_html(&value),
            price = json_price(rule.get("price")),
        ));
    }
    html
}

fn render_json_detail(label: &str, value: Option<&serde_json::Value>) -> String {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return String::new();
    };
    let formatted = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    format!(
        r#"<div class="detail-block"><div class="detail-label">{}</div><pre class="json-example"><code>{}</code></pre></div>"#,
        escape_html(label),
        escape_html(&formatted)
    )
}

fn render_endpoint_detail(endpoint: &EndpointDocResponse, index: usize) -> String {
    let mut blocks = String::new();
    if let Some(description) = endpoint
        .description
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        blocks.push_str(&format!(
            r#"<div class="detail-block full"><div class="detail-label">Description</div><p class="detail-text">{}</p></div>"#,
            escape_html(description)
        ));
    }
    if let Some(markdown) = endpoint
        .documentation_markdown
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        blocks.push_str(&format!(
            r#"<div class="detail-block full"><div class="detail-label">Documentation</div><div class="doc-markdown">{}</div></div>"#,
            render_markdown_safe(markdown)
        ));
    }
    blocks.push_str(&format!(
        r#"<div class="detail-block full"><div class="detail-label">Pricing</div>{}</div>"#,
        render_pricing(endpoint.price_usd, &endpoint.pricing_rules)
    ));
    if let Some(notes) = endpoint
        .authentication_notes
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        blocks.push_str(&format!(
            r#"<div class="detail-block full"><div class="detail-label">Authentication</div><p class="detail-text">{}</p></div>"#,
            escape_html(notes)
        ));
    }
    blocks.push_str(&render_json_detail(
        "Parameters",
        endpoint.query_params.as_ref(),
    ));
    blocks.push_str(&render_json_detail(
        "Request schema",
        endpoint.request_body.as_ref(),
    ));
    blocks.push_str(&render_json_detail(
        "Response schema",
        endpoint.response_body.as_ref(),
    ));
    blocks.push_str(&render_json_detail("Headers", endpoint.headers.as_ref()));
    blocks.push_str(&render_json_detail(
        "Example request",
        endpoint.request_example.as_ref(),
    ));
    blocks.push_str(&render_json_detail(
        "Example response",
        endpoint.response_example.as_ref(),
    ));

    let mut links = String::new();
    if let Some(url) = endpoint
        .external_docs_url
        .as_deref()
        .filter(|value| value.trim().starts_with("https://"))
    {
        links.push_str(&format!(
            r#"<a href="{}" target="_blank" rel="noreferrer">External documentation &nearr;</a>"#,
            escape_html(url)
        ));
    }
    if let Some(url) = endpoint
        .openapi_url
        .as_deref()
        .filter(|value| value.trim().starts_with("https://"))
    {
        links.push_str(&format!(
            r#"<a href="{}" target="_blank" rel="noreferrer">OpenAPI / Swagger &nearr;</a>"#,
            escape_html(url)
        ));
    }
    if !links.is_empty() {
        blocks.push_str(&format!(
            r#"<div class="detail-block full"><div class="detail-label">Links</div><div class="doc-links">{}</div></div>"#,
            links
        ));
    }

    let method_upper = endpoint.method.to_uppercase();
    format!(
        r#"<article class="endpoint-detail" id="endpoint-{index}">
          <div class="endpoint-detail-head">
            <div class="endpoint-detail-title"><span class="method {method}">{method}</span><code>{path}</code></div>
            <span class="price-default">{price} default</span>
          </div>
          <div class="endpoint-detail-grid">{blocks}</div>
        </article>"#,
        index = index,
        method = escape_html(&method_upper),
        path = escape_html(&endpoint.path),
        price = format_price(endpoint.price_usd),
        blocks = blocks,
    )
}

/// Render the top-left "eyebrow" back-link shown on every page except home.
fn eyebrow(label: &str, href: &str) -> String {
    format!(
        r#"<div class="eyebrow"><a href="{}">&larr; {}</a></div>"#,
        href,
        escape_html(label)
    )
}

/// Provider logo/avatar markup shared by the provider directory and the
/// provider's own API-list page: the real avatar if set, else a single
/// letter fallback on a tinted circle.
///
/// An avatar_url that is set but does not load (dead link, hotlink blocked,
/// deleted upload) looks the same to a visitor as no avatar at all, so it gets
/// the same monogram. That needs the `onerror`: a server-rendered page cannot
/// know the fetch failed, so the fallback ships in the markup hidden, and the
/// img reveals it on the way out. Without it the browser paints its own
/// broken-image icon and the card reads as broken rather than plain.
///
/// The letter is always rendered and the avatar sits on top of it, opaque.
/// A logo URL we can't render — 404, or a host that serves it with
/// `Cross-Origin-Resource-Policy: same-origin` so the browser refuses it
/// cross-origin — drops out via `onerror` and uncovers the letter, instead
/// of leaving a broken-image icon in the card.
fn provider_avatar_html(name: &str, avatar_url: Option<&str>, size_px: u32) -> String {
    let initial = name.chars().next().unwrap_or('?');
    // `display` differs per call site: `none` while the img is still expected to
    // load, `flex` when the monogram is what we render from the start.
    let monogram = |display: &str| {
        format!(
            r#"<span style="display:{display};align-items:center;justify-content:center;width:{size}px;height:{size}px;padding:0;border:0;background:transparent;color:#6b7280;font-weight:600;font-size:{font}px;">{initial}</span>"#,
            display = display,
            size = size_px,
            font = size_px / 2,
            initial = escape_html(&initial.to_string())
        )
    };

    match avatar_url.filter(|url| !url.is_empty()) {
        // The monogram is the img's immediate next sibling in every caller, so
        // `nextElementSibling` resolves to it without needing an id per card.
        Some(url) => format!(
            r#"<img src="{url}" alt="" loading="lazy" onerror="this.style.display='none';this.nextElementSibling.style.display='flex';" style="display:block;width:{size}px;height:{size}px;padding:0;border:0;border-radius:12px;background:transparent;object-fit:cover;">{monogram}"#,
            url = escape_html(url),
            size = size_px,
            monogram = monogram("none"),
        ),
        None => monogram("flex"),
    }
}

/// One provider card in the directory grid: logo, name, sector, bio and how
/// much they publish. How a listing is served (proxied by the gateway or
/// federated straight from the provider) is an implementation detail of the
/// call, not something a visitor picks a provider on — it stays on the API
/// detail page.
fn provider_card_html(provider: &ProviderInfoResponse) -> String {
    // 42px inside the 44px framed box: the 1px border stays visible instead of
    // being covered by a logo that fills the frame edge to edge.
    let logo = provider_avatar_html(&provider.name, provider.avatar_url.as_deref(), 42);
    let bio = provider
        .bio
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("No bio yet.");
    let sector = provider
        .sector
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!(r#"<div class="p-sector">{}</div>"#, escape_html(value)))
        .unwrap_or_default();

    let pills = format!(
        r#"<span class="pill pill-count">{count} API{plural}</span>"#,
        count = provider.service_count,
        plural = if provider.service_count == 1 { "" } else { "s" },
    );

    format!(
        r#"<a href="/v1/providers/{username}" class="provider-card">
          <div class="p-head">
            <span class="p-logo">{logo}</span>
            <div style="min-width:0;">
              <h3>{name}</h3>
              {sector}
            </div>
          </div>
          <p>{bio}</p>
          <div class="p-meta">{pills}</div>
        </a>"#,
        username = escape_html(&provider.username),
        logo = logo,
        name = escape_html(&provider.name),
        sector = sector,
        bio = escape_html(bio),
        pills = pills,
    )
}

/// The provider grid itself, shared by the home page and `/v1/providers`.
fn provider_grid_html(providers: &[ProviderInfoResponse]) -> String {
    if providers.is_empty() {
        return r#"<div class="empty">No providers found matching your criteria.</div>"#
            .to_string();
    }
    let cards: String = providers.iter().map(provider_card_html).collect();
    format!(r#"<div class="grid api-grid">{}</div>"#, cards)
}

/// Home page (`GET /`) — the gateway is a payment gateway, so the hero says
/// what it does (402 → pay → forward) and the browse surfaces sit next to each
/// other: APIs, providers, types and the docs. Deliberately not the provider
/// directory itself; providers are one of four ways in, at `/v1/providers`.
///
/// `providers` is only used for the live counts, and is allowed to be empty:
/// the landing page must still render when the directory query is unavailable.
pub(crate) fn render_home_html(providers: &[ProviderInfoResponse]) -> String {
    let mut html = html_head(
        "Apiosk Gateway",
        Nav::Home,
        crate::gateway_routes::POSITIONING_SHORT,
        "/",
    );

    let provider_count = providers.len() as i64;
    let listing_count: i64 = providers.iter().map(|p| p.api_count).sum();

    html.push_str(&format!(
        r#"<div class="hero">
          <span class="badge">Gateway online</span>
          <h1>The payment gateway for pay-per-call APIs</h1>
          <p class="subtitle">Apiosk answers <code>402 Payment Required</code>, verifies the x402 payment and forwards the call. USDC on Base, settled per request — no accounts, no API keys, no subscriptions.</p>
          {stats}
        </div>"#,
        stats = if listing_count > 0 {
            format!(
                r#"<div class="hero-stats"><strong>{listing_count}</strong> API{listing_plural} from <strong>{provider_count}</strong> provider{provider_plural} &middot; priced per call in USDC on Base</div>"#,
                listing_count = listing_count,
                listing_plural = if listing_count == 1 { "" } else { "s" },
                provider_count = provider_count,
                provider_plural = if provider_count == 1 { "" } else { "s" },
            )
        } else {
            String::new()
        },
    ));

    html.push_str(&format!(
        r#"<div class="section">
          <div class="section-title">Explore</div>
          <div class="grid explore-grid">
            <a href="/v1/apis" class="link-card">
              <h3>Browse APIs</h3>
              <p>Every pay-per-call listing, with live pricing and endpoints.</p>
              <code>/v1/apis</code>
            </a>
            <a href="/v1/providers" class="link-card">
              <h3>Providers</h3>
              <p>{provider_line} publishing here, biggest catalog first.</p>
              <code>/v1/providers</code>
            </a>
            <a href="/types" class="link-card">
              <h3>Types</h3>
              <p>Browse by kind: API, dataset, service, connector or skill.</p>
              <code>/types</code>
            </a>
            <a href="https://apiosk.com/docs" class="link-card">
              <h3>Docs</h3>
              <p>How x402 works here: paying, verification and settlement.</p>
              <code>apiosk.com/docs</code>
            </a>
          </div>
        </div>"#,
        // Deliberately not a listing count: the catalog at /v1/apis includes
        // listings whose owner has no public provider profile, so a total
        // derived from the directory would disagree with the page this card
        // links to. The hero's "N APIs from M providers" is directory-scoped
        // and says so.
        provider_line = if provider_count > 0 {
            format!(
                "The {provider_count} provider{plural}",
                provider_count = provider_count,
                plural = if provider_count == 1 { "" } else { "s" },
            )
        } else {
            "The teams".to_string()
        },
    ));

    // Before "how a paid call works", because choosing what to call comes
    // before calling it, and because this is the half a provider's own API can
    // never answer about itself. A page that opens on payment mechanics
    // describes a toll booth; the comparison is what makes it a marketplace.
    html.push_str(
        r#"<div class="section">
          <div class="section-title">Choosing a provider</div>
          <div class="steps">
            <div class="step">
              <div class="step-n">1</div>
              <h3>Discover</h3>
              <p><code>GET /v1/discover?q=read+a+web+page</code> — what can perform this task? Returns candidates with stable ids, and sweeps the wider x402 ecosystem alongside the reviewed catalogue.</p>
            </div>
            <div class="step">
              <div class="step-n">2</div>
              <h3>Compare</h3>
              <p><code>GET /v1/compare?candidates=…</code> — how do they perform against your requirements? Price, measured latency, measured success rate and input fit, each scored 0-100 with the weights that produced the number.</p>
            </div>
            <div class="step">
              <div class="step-n">3</div>
              <h3>Decide</h3>
              <p><code>GET /v1/decide?candidates=…</code> — one provider back, the rule that picked it, every rejection with the constraint that caused it, and the runners-up in order.</p>
            </div>
          </div>
          <p class="section-note">State your constraints once and pass the same ones down the chain: <code>max_price</code>, <code>max_latency_ms</code>, <code>min_reliability</code>, <code>settlement</code>, <code>require_all_inputs</code>, <code>optimize_for</code>. Any step also takes a plain <code>q=</code> if you want to skip ahead. None of the three costs anything.</p>
        </div>

        <div class="section">
          <div class="section-title">How a paid call works</div>
          <div class="steps">
            <div class="step">
              <div class="step-n">1</div>
              <h3>Call the API</h3>
              <p><code>GET /{api-slug}/{path}</code> — the gateway answers <code>402</code> with the exact price and where to pay.</p>
            </div>
            <div class="step">
              <div class="step-n">2</div>
              <h3>Pay per request</h3>
              <p>The client signs a USDC payment on Base and retries with an <code>X-Payment</code> header. No account, no key.</p>
            </div>
            <div class="step">
              <div class="step-n">3</div>
              <h3>Get the response</h3>
              <p>The gateway verifies the payment, forwards the request untouched and settles to the provider.</p>
            </div>
          </div>
        </div>

        <div class="section">
          <div class="section-title">For agents</div>
          <div class="quicklinks">
            <a href="/v1/discover"><code>GET /v1/discover</code> <span>What can do this job</span></a>
            <a href="/v1/compare"><code>GET /v1/compare</code> <span>How the candidates measure up</span></a>
            <a href="/v1/decide"><code>GET /v1/decide</code> <span>Which one to call, and why</span></a>
            <a href="/.well-known/x402"><code>GET /.well-known/x402</code> <span>x402 discovery</span></a>
            <a href="/llms.txt"><code>GET /llms.txt</code> <span>How to discover, call and pay</span></a>
            <a href="/openapi.json"><code>GET /openapi.json</code> <span>OpenAPI contract</span></a>
            <a href="/v1/apis"><code>GET /v1/apis</code> <span>Catalog as JSON</span></a>
            <a href="/health"><code>GET /health</code> <span>Gateway status</span></a>
          </div>
        </div>

        <div class="section">
          <div class="section-title">For providers</div>
          <div class="quicklinks">
            <a href="/federated/new"><code>Publish an API</code> <span>Already x402-native? List it in a few minutes</span></a>
            <a href="https://apiosk.com/docs"><code>Read the docs</code> <span>Pricing, payouts and settlement</span></a>
          </div>
        </div>"#,
    );

    html.push_str(html_footer());
    html
}

/// API catalog (`GET /v1/apis` in a browser) — the flat listing grid: one card
/// per API with its provider, category and price. The gateway's browse surface
/// is the catalog first; `/v1/providers` is the same catalog grouped by who
/// publishes it.
pub(crate) fn render_api_list_html(
    apis: &[ApiInfoResponse],
    total: i64,
    limit: i64,
    offset: i64,
    search: Option<&str>,
) -> String {
    let mut html = html_head(
        "Browse APIs",
        Nav::Apis,
        "Browse every pay-per-call API on the Apiosk gateway — endpoints, per-call pricing, and how to pay. USDC on Base, no accounts or keys.",
        "/v1/apis",
    );

    html.push_str(&format!(
        r#"<div class="header-row">
          <div>
            <h1>Browse APIs</h1>
            <p class="subtitle">{count} pay-per-call listing{plural} on the gateway. Open one for its endpoints, prices and how to pay. USDC on Base, no accounts or keys.</p>
          </div>
          <div class="search-box">
            <form method="get" action="/v1/apis">
              <input type="text" name="search" placeholder="Search by name, category or keyword..." {value} />
            </form>
          </div>
        </div>"#,
        count = total,
        plural = if total == 1 { "" } else { "s" },
        value = search
            .map(|s| format!(r#"value="{}""#, escape_html(s)))
            .unwrap_or_default(),
    ));

    // Category filter pills, derived from the current page's categories.
    let mut categories: Vec<&str> = apis.iter().map(|api| api.category.as_str()).collect();
    categories.sort_unstable();
    categories.dedup();
    if !categories.is_empty() {
        html.push_str(r#"<div class="section"><div class="tags">"#);
        for category in categories.iter().take(8) {
            let is_active = search
                .map(|s| s.eq_ignore_ascii_case(category))
                .unwrap_or(false);
            html.push_str(&format!(
                r#"<a href="/v1/apis?search={}" class="tag{}">{}</a>"#,
                escape_html(category),
                if is_active { " is-active" } else { "" },
                escape_html(category)
            ));
        }
        html.push_str(r#"</div></div>"#);
    }

    html.push_str(r#"<div class="section"><div class="grid api-grid">"#);

    if apis.is_empty() {
        html.push_str(r#"<div class="empty">No APIs found matching your criteria.</div>"#);
    } else {
        for api in apis {
            html.push_str(&api_card_html(api));
        }
    }

    html.push_str(r#"</div></div>"#);

    if total > limit {
        html.push_str(r#"<div class="pagination">"#);
        let search_param = search
            .map(|s| {
                let encoded: String = url::form_urlencoded::byte_serialize(s.as_bytes()).collect();
                format!("&search={}", encoded)
            })
            .unwrap_or_default();
        if offset > 0 {
            html.push_str(&format!(
                r#"<a href="/v1/apis?offset={}{}">&larr; Previous</a>"#,
                (offset - limit).max(0),
                search_param
            ));
        }
        if offset + limit < total {
            html.push_str(&format!(
                r#"<a href="/v1/apis?offset={}{}">Next &rarr;</a>"#,
                offset + limit,
                search_param
            ));
        }
        html.push_str(r#"</div>"#);
    }

    html.push_str(html_footer());
    html
}

/// One API card in the catalog grid: provider chip, verified badge, name,
/// description, category and price.
fn api_card_html(api: &ApiInfoResponse) -> String {
    let provider = api
        .provider
        .as_ref()
        .map(|provider| {
            // Same broken-avatar handling as provider_avatar_html, in the chip's
            // own class-based markup: hidden monogram after the img, revealed by
            // onerror. `.provider-fallback` is display:grid, so restore that
            // rather than the flex the larger avatar uses.
            let initial = escape_html(&provider.name.chars().next().unwrap_or('?').to_string());
            let avatar = provider
                .avatar_url
                .as_deref()
                .filter(|url| !url.is_empty())
                .map_or_else(
                    || format!(r#"<span class="provider-logo provider-fallback">{initial}</span>"#),
                    |url| {
                        format!(
                            r#"<img class="provider-logo" src="{url}" alt="" loading="lazy" onerror="this.style.display='none';this.nextElementSibling.style.display='grid';"><span class="provider-logo provider-fallback" style="display:none;">{initial}</span>"#,
                            url = escape_html(url),
                        )
                    },
                );
            format!(
                r#"<span class="provider">{}<span class="provider-name">{}</span></span>"#,
                avatar,
                escape_html(&provider.name),
            )
        })
        .unwrap_or_default();

    format!(
        r#"<a href="/v1/apis/{slug}" class="api-card">
          <div class="card-top">
            {provider}
            {verified}
          </div>
          <h3>{name}</h3>
          <p>{description}</p>
          <div class="meta">
            <span class="category">{category}</span>
            <span class="price">{price}</span>
          </div>
        </a>"#,
        slug = escape_html(&api.slug),
        provider = provider,
        verified = if api.verified {
            r#"<span class="badge" style="font-size:9.5px;padding:2px 6px;">Verified</span>"#
        } else {
            ""
        },
        name = escape_html(&api.name),
        description = escape_html(&api.description),
        category = escape_html(&api.category),
        price = format_price(api.price_usd),
    )
}

/// Provider directory (`GET /v1/providers`) — the same grid as the home page,
/// reachable on its own URL; no individual APIs shown until a provider is
/// opened.
pub(crate) fn render_provider_list_html(
    providers: &[ProviderInfoResponse],
    search: Option<&str>,
) -> String {
    let mut html = html_head(
        "Browse Providers",
        Nav::Providers,
        "Every provider publishing pay-per-call APIs on the Apiosk gateway. Open a provider to see their APIs. USDC on Base, no accounts or keys.",
        "/v1/providers",
    );

    html.push_str(&format!(
        r#"<div class="header-row">
          <div>
            <h1>Browse Providers</h1>
            <p class="subtitle">Everyone publishing on the gateway, biggest catalog first. Open a provider to see their pay-per-call APIs. USDC on Base, no accounts or keys.</p>
          </div>
          <div class="search-box">
            <form method="get" action="/v1/providers">
              <input type="text" name="search" placeholder="Search providers or APIs..." {value} />
            </form>
          </div>
        </div>"#,
        value = search
            .map(|s| format!(r#"value="{}""#, escape_html(s)))
            .unwrap_or_default(),
    ));

    html.push_str(&format!(
        r#"<div class="section">{}</div>"#,
        provider_grid_html(providers)
    ));

    html.push_str(html_footer());
    html
}

/// A single provider's APIs (`GET /v1/providers/:username`) — the second
/// step of the browse flow: provider identity up top, then that provider's
/// API grid below, reusing the same card markup as the full catalog.
pub(crate) fn render_provider_apis_html(
    provider: &ProviderInfoResponse,
    apis: &[ApiInfoResponse],
    total: i64,
    limit: i64,
    offset: i64,
    search: Option<&str>,
) -> String {
    let provider_desc = provider
        .bio
        .as_deref()
        .filter(|b| !b.trim().is_empty())
        .map(|b| b.to_string())
        .unwrap_or_else(|| {
            format!(
                "{} publishes {} pay-per-call API{} on the Apiosk gateway. USDC on Base, no accounts or keys.",
                provider.name,
                provider.api_count,
                if provider.api_count == 1 { "" } else { "s" },
            )
        });
    let mut html = html_head(
        &provider.name,
        Nav::Providers,
        &provider_desc,
        &format!("/v1/providers/{}", provider.username),
    );

    html.push_str(&eyebrow("Providers", "/v1/providers"));

    let logo = provider_avatar_html(&provider.name, provider.avatar_url.as_deref(), 56);
    let bio = provider
        .bio
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(escape_html)
        .unwrap_or_default();
    let sector = provider
        .sector
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!(r#"<span class="tag">{}</span>"#, escape_html(value)))
        .unwrap_or_default();

    html.push_str(&format!(
        r#"<div class="header-row">
          <div style="display:flex;align-items:center;gap:14px;">
            {logo}
            <div>
              <h1 style="margin:0;">{name}</h1>
              <p class="subtitle" style="margin-top:4px;">{bio}</p>
              {sector}
            </div>
          </div>
          <div class="search-box">
            <form method="get" action="/v1/providers/{username}">
              <input type="text" name="search" placeholder="Search this provider's APIs..." {value} />
            </form>
          </div>
        </div>"#,
        logo = logo,
        name = escape_html(&provider.name),
        bio = bio,
        sector = sector,
        username = escape_html(&provider.username),
        value = search
            .map(|s| format!(r#"value="{}""#, escape_html(s)))
            .unwrap_or_default(),
    ));

    html.push_str(r#"<div class="section"><div class="grid api-grid">"#);

    if apis.is_empty() {
        html.push_str(
            r#"<div class="empty">This provider has no APIs matching your criteria.</div>"#,
        );
    } else {
        for api in apis {
            html.push_str(&format!(
                r#"<a href="/v1/apis/{}" class="api-card">
              <div class="card-top">
                {}
              </div>
              <h3>{}</h3>
              <p>{}</p>
              <div class="meta">
                <span class="category">{}</span>
                <span class="price">{}</span>
              </div>
            </a>"#,
                escape_html(&api.slug),
                if api.verified { r#"<span class="badge" style="font-size:9.5px;padding:2px 6px;">Verified</span>"# } else { "" },
                escape_html(&api.name),
                escape_html(&api.description),
                escape_html(&api.category),
                format_price(api.price_usd),
            ));
        }
    }

    html.push_str(r#"</div></div>"#);

    if total > limit {
        html.push_str(r#"<div class="pagination">"#);
        if offset > 0 {
            let prev_offset = (offset - limit).max(0);
            let search_param = search.map(|s| format!("&search={}", s)).unwrap_or_default();
            html.push_str(&format!(
                r#"<a href="/v1/providers/{}?offset={}{}">&larr; Previous</a>"#,
                provider.username, prev_offset, search_param
            ));
        }
        if offset + limit < total {
            let next_offset = offset + limit;
            let search_param = search.map(|s| format!("&search={}", s)).unwrap_or_default();
            html.push_str(&format!(
                r#"<a href="/v1/providers/{}?offset={}{}">Next &rarr;</a>"#,
                provider.username, next_offset, search_param
            ));
        }
        html.push_str(r#"</div>"#);
    }

    html.push_str(html_footer());
    html
}

/// How many federated resources to list before collapsing into a count. A
/// federated import can carry dozens (WebberSites ships 75); the page stays a
/// preview and the full set lives in `/.well-known/x402`.
const MAX_FEDERATED_RESOURCES_SHOWN: usize = 25;

/// The "Base URL" block's federated counterpart. A federated listing has no
/// gateway base URL — the agent calls the provider directly — so this shows the
/// provider's own resource URLs verbatim (that is what an agent must call, and
/// what it must see in the 402 challenge) while the human-facing "Open" link
/// goes through the tracked `/f/:slug` hop.
///
/// Deliberately asymmetric: the tracked hop is a DISCOVERY link only. Putting it
/// in the copyable resource URL would break the x402 flow — see
/// `crate::federated_traffic`.
fn render_federated_resources(api: &ApiDetailResponse) -> String {
    let slug_encoded: String = url::form_urlencoded::byte_serialize(api.slug.as_bytes()).collect();

    let mut html = String::from(
        r#"<div class="section">
          <div class="section-title">Hosted by the provider</div>
          <p style="color:#4b5563;line-height:1.6;font-size:14px;max-width:720px;margin-top:0;">
            Apiosk lists this API but does not proxy it. Call the provider's URL directly and pay them
            over x402 — Apiosk never sits between you and the response.
          </p>"#,
    );

    html.push_str(&format!(
        r#"<p style="margin:0 0 14px;"><a class="view-doc" href="/f/{slug}" rel="nofollow">Open provider &rarr;</a></p>"#,
        slug = escape_html(&slug_encoded),
    ));

    let resources: Vec<(&str, Option<&str>)> = api
        .external_resources
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|entry| {
                    let url = entry.get("resource").and_then(serde_json::Value::as_str)?;
                    let description = entry
                        .get("description")
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty());
                    Some((url, description))
                })
                .collect()
        })
        .unwrap_or_default();

    if resources.is_empty() {
        html.push_str(r#"<div class="empty">No resources published yet.</div></div>"#);
        return html;
    }

    html.push_str(
        r#"<table><thead><tr><th>Resource</th><th>Description</th><th></th></tr></thead><tbody>"#,
    );
    for (url, description) in resources.iter().take(MAX_FEDERATED_RESOURCES_SHOWN) {
        let encoded: String = url::form_urlencoded::byte_serialize(url.as_bytes()).collect();
        html.push_str(&format!(
            r#"<tr>
              <td class="path"><code>{url}</code></td>
              <td class="endpoint-desc">{description}</td>
              <td><a class="view-doc" href="/f/{slug}?r={encoded}" rel="nofollow">Open</a></td>
            </tr>"#,
            url = escape_html(url),
            description = description.map(escape_html).unwrap_or_default(),
            slug = escape_html(&slug_encoded),
            encoded = escape_html(&encoded),
        ));
    }
    html.push_str("</tbody></table>");

    if resources.len() > MAX_FEDERATED_RESOURCES_SHOWN {
        html.push_str(&format!(
            r#"<p style="color:#6b7280;font-size:13px;margin-top:10px;">Showing {shown} of {total} resources — the full set is in <a href="/.well-known/x402">/.well-known/x402</a>.</p>"#,
            shown = MAX_FEDERATED_RESOURCES_SHOWN,
            total = resources.len(),
        ));
    }

    html.push_str("</div>");
    html
}

pub(crate) fn render_api_detail_html(api: &ApiDetailResponse) -> String {
    let api_desc = if api.description.trim().is_empty() {
        format!(
            "{} — a pay-per-call API on the Apiosk gateway. Call it per request and settle in USDC on Base, no accounts or keys.",
            api.name
        )
    } else {
        api.description.clone()
    };
    let mut html = html_head(
        &api.name,
        Nav::Apis,
        &api_desc,
        &format!("/v1/apis/{}", api.slug),
    );

    html.push_str(&format!(
        r#"<div class="eyebrow"><a href="/v1/apis">&larr; APIs</a> / {}</div>"#,
        escape_html(&api.name)
    ));

    // Provider identity (logo + name + sector), joined from provider_profiles.
    let provider_html = match &api.provider {
        Some(provider) => {
            let logo = match provider.avatar_url.as_deref() {
                // No initial to fall back to on this line — the provider name
                // is already spelled out next to it — so a logo that won't
                // load just removes itself.
                Some(url) if !url.trim().is_empty() => format!(
                    r#"<img src="{}" alt="{}" onerror="this.remove()" style="width:22px;height:22px;border-radius:5px;object-fit:cover;border:1px solid #e5e7eb;">"#,
                    escape_html(url),
                    escape_html(&provider.name),
                ),
                _ => String::new(),
            };
            let sector = provider
                .sector
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(|value| format!(" &middot; {}", escape_html(value)))
                .unwrap_or_default();
            format!(
                r#"<div style="display:flex;align-items:center;gap:8px;margin:8px 0 16px;color:#6b7280;font-size:13.5px;">{}<span>by {}</span>{}</div>"#,
                logo,
                escape_html(&provider.name),
                sector,
            )
        }
        None => String::new(),
    };

    let tags = {
        let mut t = String::new();
        if let Some(cat) = api.category.as_deref().filter(|c| !c.is_empty()) {
            t.push_str(&format!(r#"<span class="tag">{}</span>"#, escape_html(cat)));
        }
        if let Some(lt) = api.listing_type.as_deref().filter(|c| !c.is_empty()) {
            t.push_str(&format!(r#"<span class="tag">{}</span>"#, escape_html(lt)));
        }
        t
    };

    html.push_str(&format!(
        r#"<div class="header-row">
          <div>
            <span class="badge{badge_class}">{badge_label}</span>
            <h1 style="margin-top:10px;">{name}</h1>
            {provider}
            <p class="subtitle">{description}</p>
            {tags_block}
          </div>
          <div class="stat-box">
            <div class="stat-label">Price per request</div>
            <div class="stat-value">{price}</div>
          </div>
        </div>"#,
        badge_class = if api.verified { "" } else { " badge-purple" },
        badge_label = if api.verified {
            "Verified"
        } else {
            "Community"
        },
        name = escape_html(&api.name),
        provider = provider_html,
        description = escape_html(&api.description),
        tags_block = if tags.is_empty() {
            String::new()
        } else {
            format!(
                r#"<div class="tags" style="margin-top:14px;">{}</div>"#,
                tags
            )
        },
        price = format_price(api.price_usd),
    ));

    if let Some(ref long_desc) = api.long_description {
        html.push_str(&format!(
            r#"<div class="section"><div class="section-title">About</div><p style="color:#4b5563;line-height:1.6;font-size:14px;max-width:720px;">{}</p></div>"#,
            escape_html(long_desc)
        ));
    }

    if api.hosted_externally {
        html.push_str(&render_federated_resources(api));
    } else {
        html.push_str(&format!(
            r#"<div class="section">
          <div class="section-title">Base URL</div>
          <code style="display:block;padding:12px;border:1px solid #e5e7eb;font-size:13.5px;">{}</code>
        </div>"#,
            escape_html(&api.gateway_base_url)
        ));
    }

    // Technical specs, derived from whatever authentication metadata exists.
    let auth_method = api
        .authentication
        .as_ref()
        .and_then(|a| a.get("type").or_else(|| a.get("method")))
        .and_then(|v| v.as_str())
        .unwrap_or("None (x402 payment proof only)");
    html.push_str(&format!(
        r#"<div class="section">
          <div class="section-title">Technical Specs</div>
          <div class="specs">
            <div class="spec-row"><span class="k">Auth Method</span><span class="v">{}</span></div>
            <div class="spec-row"><span class="k">Response Format</span><span class="v">JSON</span></div>
            <div class="spec-row"><span class="k">Endpoints</span><span class="v">{}</span></div>
          </div>
        </div>"#,
        escape_html(auth_method),
        api.endpoints.len()
    ));

    // Distinct HTTP methods present, used to build the filter tabs.
    let mut methods: Vec<String> = api
        .endpoints
        .iter()
        .map(|e| e.method.to_uppercase())
        .collect();
    methods.sort_unstable();
    methods.dedup();

    html.push_str(
        r#"<div class="section">
          <div class="header-row" style="margin-bottom:16px;align-items:center;">
            <div class="section-title" style="margin-bottom:0;">Endpoints</div>
            <div class="filter-tabs" id="method-filter">
              <button type="button" class="is-active" data-method="ALL">All</button>"#,
    );
    for m in &methods {
        html.push_str(&format!(
            r#"<button type="button" data-method="{m}">{m}</button>"#,
            m = escape_html(m)
        ));
    }
    html.push_str(r#"</div></div>"#);

    if api.endpoints.is_empty() {
        html.push_str(r#"<div class="empty">No endpoints documented yet.</div>"#);
    } else {
        html.push_str(
            r#"<table id="endpoint-table"><thead><tr>
              <th>Method</th><th>Path</th><th>Description</th><th>Cost</th><th></th>
            </tr></thead><tbody>"#,
        );
        for (index, endpoint) in api.endpoints.iter().enumerate() {
            let method_upper = endpoint.method.to_uppercase();
            html.push_str(&format!(
                r#"<tr data-method="{method_upper}">
              <td><span class="method {method_upper}">{method_upper}</span></td>
              <td class="path">{path}</td>
              <td class="endpoint-desc">{desc}</td>
              <td class="price-cell">{pricing}</td>
              <td>{docs}</td>
            </tr>"#,
                method_upper = escape_html(&method_upper),
                path = escape_html(&endpoint.path),
                desc = endpoint
                    .description
                    .as_deref()
                    .map(escape_html)
                    .unwrap_or_default(),
                pricing = render_pricing(endpoint.price_usd, &endpoint.pricing_rules),
                docs = format_args!(
                    r##"<a href="#endpoint-{}" class="view-doc">View details</a>"##,
                    index
                ),
            ));
        }
        html.push_str("</tbody></table>");

        html.push_str(
            r#"<div class="endpoint-details"><div class="section-title" style="margin:4px 0 0;">Endpoint documentation</div>"#,
        );
        for (index, endpoint) in api.endpoints.iter().enumerate() {
            html.push_str(&render_endpoint_detail(endpoint, index));
        }
        html.push_str("</div>");
    }

    html.push_str(r#"</div>"#);

    html.push_str(
        r#"<script>
          (function () {
            var buttons = document.querySelectorAll('#method-filter button');
            var rows = document.querySelectorAll('#endpoint-table tbody tr');
            buttons.forEach(function (btn) {
              btn.addEventListener('click', function () {
                buttons.forEach(function (b) { b.classList.remove('is-active'); });
                btn.classList.add('is-active');
                var method = btn.getAttribute('data-method');
                rows.forEach(function (row) {
                  row.style.display = (method === 'ALL' || row.getAttribute('data-method') === method) ? '' : 'none';
                });
              });
            });
          })();
        </script>"#,
    );

    html.push_str(html_footer());
    html
}

pub(crate) fn render_types_html(types: &TypesResponse) -> String {
    let mut html = html_head(
        "API Types",
        Nav::Types,
        "Browse Apiosk gateway listings by type — APIs, datasets, services, connectors, skills and products. Pay per call in USDC on Base.",
        "/types",
    );

    html.push_str(r#"
        <h1>API Types</h1>
        <p class="subtitle">Browse APIs by category. Each type represents a different kind of API or service.</p>
        <div class="section" style="margin-top:28px;">
          <div class="grid">"#);

    for item in &types.supported_types {
        html.push_str(&format!(
            r#"<a href="{}" class="api-card">
              <h3>{}</h3>
              <p>{}</p>
              <div class="meta"><code style="text-transform:none;font-weight:400;letter-spacing:0;font-family:ui-monospace,monospace;color:#7c3aed;font-size:12px;">GET {}</code></div>
            </a>"#,
            escape_html(&item.endpoint),
            escape_html(&item.key),
            escape_html(&item.description),
            escape_html(&item.endpoint)
        ));
    }

    html.push_str(r#"</div></div>"#);

    html.push_str(html_footer());
    html
}

pub(crate) fn render_type_list_html(
    listing_type: &str,
    apis: &[ApiInfoResponse],
    total: i64,
    limit: i64,
    offset: i64,
    search: Option<&str>,
) -> String {
    let type_title = match listing_type {
        "api" => "APIs",
        "dataset" => "Datasets",
        "service" => "Services",
        "connector" => "Connectors",
        "skill" => "Skills",
        "product" => "Products",
        _ => listing_type,
    };

    let mut html = html_head(
        type_title,
        Nav::Types,
        &format!(
            "Browse {} on the Apiosk gateway — pay-per-call listings settled in USDC on Base, no accounts or keys.",
            type_title.to_lowercase()
        ),
        &format!("/types/{listing_type}/v1"),
    );
    html.push_str(&eyebrow("Types", "/types"));
    html.push_str(&format!(
        r#"
        <div class="header-row">
          <div>
            <h1>{}</h1>
            <p class="subtitle">Browse all {} in this category.</p>
          </div>
          <div class="search-box">
            <form method="get" action="/types/{}/v1">
              <input type="text" name="search" placeholder="Search {}..." "#,
        escape_html(type_title),
        type_title.to_lowercase(),
        escape_html(listing_type),
        type_title.to_lowercase()
    ));
    if let Some(s) = search {
        html.push_str(&format!(r#"value="{}""#, escape_html(s)));
    }
    html.push_str(
        r#" />
            </form>
          </div>
        </div>
        <div class="section"><div class="grid">"#,
    );

    if apis.is_empty() {
        html.push_str(r#"<div class="empty">No items found matching your criteria.</div>"#);
    } else {
        for api in apis {
            html.push_str(&format!(
                r#"<a href="/v1/apis/{}" class="api-card">
              <h3>{}{}</h3>
              <p>{}</p>
              <div class="meta">
                <span class="category">{}</span>
                <span class="price">{}</span>
              </div>
            </a>"#,
                escape_html(&api.slug),
                escape_html(&api.name),
                if api.verified { r#" <span class="badge" style="font-size:10px;padding:2px 6px;">Verified</span>"# } else { "" },
                escape_html(&api.description),
                escape_html(&api.category),
                format_price(api.price_usd),
            ));
        }
    }

    html.push_str(r#"</div></div>"#);

    if total > limit {
        html.push_str(r#"<div class="pagination">"#);
        if offset > 0 {
            let prev_offset = (offset - limit).max(0);
            let search_param = search.map(|s| format!("&search={}", s)).unwrap_or_default();
            html.push_str(&format!(
                r#"<a href="/types/{}/v1?offset={}{}">&larr; Previous</a>"#,
                listing_type, prev_offset, search_param
            ));
        }
        if offset + limit < total {
            let next_offset = offset + limit;
            let search_param = search.map(|s| format!("&search={}", s)).unwrap_or_default();
            html.push_str(&format!(
                r#"<a href="/types/{}/v1?offset={}{}">Next &rarr;</a>"#,
                listing_type, next_offset, search_param
            ));
        }
        html.push_str(r#"</div>"#);
    }

    html.push_str(html_footer());
    html
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1_routes::models::ProviderPublic;
    use serde_json::json;

    fn federated_detail(resources: serde_json::Value) -> ApiDetailResponse {
        ApiDetailResponse {
            id: "1".to_string(),
            slug: "webbersites".to_string(),
            name: "WebberSites x402 Data API".to_string(),
            description: "Domain data".to_string(),
            long_description: None,
            category: Some("tools".to_string()),
            listing_type: Some("federated".to_string()),
            docs_url: None,
            gateway_base_url: String::new(),
            example_usage: None,
            authentication: None,
            active: true,
            verified: true,
            listing_quality: "production".to_string(),
            price_usd: 0.01,
            operations: Vec::new(),
            listing_metadata: json!({}),
            created_at: "2026-07-24T00:00:00Z".to_string(),
            endpoints: Vec::new(),
            hosted_externally: true,
            external_resources: resources,
            provider: None,
        }
    }

    fn provider(name: &str, managed: i64, federated: i64) -> ProviderInfoResponse {
        ProviderInfoResponse {
            name: name.to_string(),
            username: name.to_lowercase(),
            avatar_url: Some("https://cdn.example.com/logo.png".to_string()),
            bio: Some("Domain intelligence".to_string()),
            sector: Some("Data".to_string()),
            api_count: managed + federated,
            service_count: managed + federated,
            federated_count: federated,
            managed_count: managed,
            listing_slug: None,
            quality: None,
        }
    }

    fn api(name: &str, slug: &str, price: f64) -> ApiInfoResponse {
        ApiInfoResponse {
            id: "1".to_string(),
            slug: slug.to_string(),
            name: name.to_string(),
            description: "Domain data".to_string(),
            price_usd: price,
            buyer_price_usd: price,
            category: "Data".to_string(),
            raw_category: "data".to_string(),
            implementation_type: "proxy".to_string(),
            model_backed: false,
            listing_type: "api".to_string(),
            docs_url: String::new(),
            origin_url: String::new(),
            gateway_url: format!("https://gateway.apiosk.com/{slug}"),
            active: true,
            verified: true,
            endpoint_count: 1,
            operations: Vec::new(),
            endpoints: Vec::new(),
            listing_metadata: json!({}),
            created_at: "2026-07-24T00:00:00Z".to_string(),
            listing_quality: "production".to_string(),
            hosted_externally: false,
            external_resources: json!([]),
            provider: Some(ProviderPublic {
                name: "PulseNetwork".to_string(),
                username: Some("pulsenetwork".to_string()),
                avatar_url: Some("https://cdn.example.com/logo.png".to_string()),
                bio: None,
                sector: None,
            }),
        }
    }

    #[test]
    fn home_is_the_payment_gateway_not_the_provider_directory() {
        let rendered = render_home_html(&[provider("PulseNetwork", 0, 75)]);

        // What the gateway *is* leads the page...
        assert!(rendered.contains("The payment gateway for pay-per-call APIs"));
        assert!(rendered.contains("402 Payment Required"));
        assert!(rendered.contains("X-Payment"));

        // ...and every browse surface is one click away, docs included — not
        // just providers.
        assert!(rendered.contains(r#"href="/v1/apis""#));
        assert!(rendered.contains(r#"href="/v1/providers""#));
        assert!(rendered.contains(r#"href="/types""#));
        assert!(rendered.contains("https://apiosk.com/docs"));
        assert!(rendered.contains(r#"href="/federated/new""#));

        // The directory itself stays on its own page.
        assert!(!rendered.contains(r#"class="provider-card""#));
    }

    /// Writes the rendered home page to disk for eyeballing. `#[ignore]`d, so
    /// it runs only when asked for: `cargo test dump_home -- --ignored`.
    #[test]
    #[ignore]
    fn dump_home() {
        let html = render_home_html(&[provider("PulseNetwork", 12, 75)]);
        let path = std::env::var("HOME_DUMP").unwrap_or_else(|_| "/tmp/home.html".to_string());
        std::fs::write(&path, html).expect("write dump");
        println!("wrote {path}");
    }

    #[test]
    fn home_explains_the_comparison_layer_before_the_payment_flow() {
        let rendered = render_home_html(&[provider("PulseNetwork", 0, 75)]);

        // All three steps are named and each carries the endpoint that runs it,
        // so a reader can act on the section rather than only be told it exists.
        for endpoint in ["/v1/discover", "/v1/compare", "/v1/decide"] {
            assert!(
                rendered.contains(endpoint),
                "home page never mentions {endpoint}"
            );
        }
        assert!(rendered.contains("Choosing a provider"));

        // Order is the argument: deciding what to call comes before calling it,
        // and a page that opens on payment mechanics describes a toll booth
        // rather than a marketplace. Guarded because a later edit that moves a
        // section would otherwise silently invert it.
        let comparison = rendered
            .find("Choosing a provider")
            .expect("section present");
        let payment = rendered
            .find("How a paid call works")
            .expect("payment section present");
        assert!(
            comparison < payment,
            "the comparison layer must lead the payment flow, not follow it"
        );
    }

    #[test]
    fn home_counts_the_live_catalog() {
        let rendered = render_home_html(&[provider("Alpha", 1, 0), provider("Beta", 0, 4)]);

        assert!(rendered.contains("<strong>5</strong> APIs from <strong>2</strong> providers"));
        assert!(rendered.contains("The 2 providers publishing here"));
    }

    #[test]
    fn home_renders_without_the_directory_rather_than_claiming_zero() {
        // The directory query is allowed to fail — the landing page is not.
        let rendered = render_home_html(&[]);

        assert!(rendered.contains("The payment gateway for pay-per-call APIs"));
        assert!(rendered.contains("Every pay-per-call listing"));
        assert!(!rendered.contains("<strong>0</strong>"));
        assert!(!rendered.contains(r#"class="hero-stats""#));
    }

    #[test]
    fn every_page_carries_the_same_nav_and_marks_where_you_are() {
        let apis = render_api_list_html(&[api("Whois", "whois", 0.01)], 1, 500, 0, None);
        let providers = render_provider_list_html(&[provider("PulseNetwork", 0, 75)], None);

        for page in [&apis, &providers] {
            assert!(page.contains(r#"<nav class="nav">"#));
            assert!(page.contains(r#"<a href="/types""#));
            assert!(page.contains(r#"href="https://apiosk.com/docs""#));
        }
        assert!(apis.contains(r#"<a href="/v1/apis" class="is-current">"#));
        assert!(providers.contains(r#"<a href="/v1/providers" class="is-current">"#));
    }

    #[test]
    fn provider_cards_count_apis_without_the_managed_federated_split() {
        let rendered = render_provider_list_html(
            &[provider("PulseNetwork", 0, 75), provider("Finnhub", 1, 0)],
            None,
        );

        assert!(rendered.contains("75 APIs"));
        assert!(rendered.contains("1 API<"));
        assert!(!rendered.contains("pill-managed"));
        assert!(!rendered.contains("pill-federated"));
        assert!(!rendered.contains("75 federated"));
        assert!(!rendered.contains("1 managed"));

        // Row order is the query's (biggest catalog first); the renderer must
        // not resort it behind the query's back.
        let pulse = rendered.find("PulseNetwork").expect("pulse card");
        let finnhub = rendered.find("Finnhub").expect("finnhub card");
        assert!(pulse < finnhub);
    }

    #[test]
    fn directory_escapes_provider_supplied_text_and_keeps_the_search_term() {
        let mut hostile = provider("Evil", 1, 0);
        hostile.name = "<script>alert(1)</script>".to_string();
        hostile.bio = Some("<img onerror=x>".to_string());

        let rendered = render_provider_list_html(&[hostile], Some(r#""><script>"#));

        assert!(!rendered.contains("<script>alert(1)</script>"));
        assert!(!rendered.contains("<img onerror=x>"));
        assert!(rendered.contains(r#"value="&quot;&gt;&lt;script&gt;""#));
    }

    #[test]
    fn provider_cards_carry_a_monogram_for_avatars_that_do_not_load() {
        // Avatar set: the img still renders, but the monogram ships alongside it
        // hidden, so a dead URL degrades to the letter instead of the browser's
        // broken-image icon.
        let rendered = render_provider_list_html(&[provider("Sirenic", 1, 0)], None);

        assert!(rendered.contains("https://cdn.example.com/logo.png"));
        assert!(rendered.contains("this.nextElementSibling.style.display='flex';"));
        assert!(rendered.contains(r#"<span style="display:none;align-items:center"#));
        assert!(rendered.contains(">S</span>"));
    }

    #[test]
    fn provider_cards_without_an_avatar_show_the_monogram_outright() {
        let mut no_avatar = provider("Sirenic", 1, 0);
        no_avatar.avatar_url = None;

        let rendered = render_provider_list_html(&[no_avatar], None);

        // Nothing to fail over, so no img and no hidden state — the monogram is
        // simply what the card renders.
        assert!(!rendered.contains("nextElementSibling"));
        assert!(rendered.contains(r#"<span style="display:flex;align-items:center"#));
        assert!(rendered.contains(">S</span>"));
    }

    #[test]
    fn a_logo_that_wont_load_falls_back_to_the_initial_not_a_broken_image() {
        // Some provider hosts serve their logo with
        // `Cross-Origin-Resource-Policy: same-origin`, so the browser refuses
        // it on our pages even though the URL is live. The card must survive
        // that: the initial ships in the markup underneath the image, and the
        // image hands over to it on error instead of leaving a broken icon.
        //
        // Directory cards are covered by
        // `provider_cards_carry_a_monogram_for_avatars_that_do_not_load`; this
        // holds the same guarantee for the provider chip on API cards, which
        // renders its own class-based markup around `.provider-fallback`.
        let cards = render_api_list_html(&[api("Whois lookup", "whois", 0.01)], 1, 500, 0, None);
        assert!(cards.contains(r#"src="https://cdn.example.com/logo.png""#));
        assert!(cards.contains("this.nextElementSibling.style.display='grid';"));
        assert!(cards.contains(
            r#"<span class="provider-logo provider-fallback" style="display:none;">P</span>"#
        ));

        // No logo at all renders the chip as the monogram outright: nothing to
        // fail over, so no img and no hidden state.
        let mut bare = api("Whois lookup", "whois", 0.01);
        bare.provider.as_mut().unwrap().avatar_url = None;
        let without_logo = render_api_list_html(&[bare], 1, 500, 0, None);
        assert!(!without_logo.contains("onerror="));
        assert!(without_logo.contains(r#"<span class="provider-logo provider-fallback">P</span>"#));
    }

    #[test]
    fn empty_directory_says_so_instead_of_rendering_a_bare_page() {
        let rendered = render_provider_list_html(&[], Some("nothing"));

        assert!(rendered.contains("No providers found matching your criteria."));
    }

    #[test]
    fn api_list_lists_apis_with_their_provider_and_price() {
        let rendered = render_api_list_html(
            &[api("Whois lookup", "whois", 0.01), api("DNS", "dns", 0.0)],
            2,
            500,
            0,
            None,
        );

        assert!(rendered.contains("2 pay-per-call listings"));
        assert!(rendered.contains(r#"href="/v1/apis/whois""#));
        assert!(rendered.contains("Whois lookup"));
        assert!(rendered.contains("PulseNetwork"));
        assert!(rendered.contains("$0.01"));
        assert!(rendered.contains("Free"));
        // The catalog page is the catalog, not the directory.
        assert!(!rendered.contains(r#"class="provider-card""#));
    }

    #[test]
    fn api_list_escapes_listing_text_and_encodes_the_paged_search_term() {
        let mut hostile = api("<script>alert(1)</script>", "evil", 0.02);
        hostile.description = "<img onerror=x>".to_string();

        let rendered = render_api_list_html(&[hostile], 40, 20, 0, Some("a b&c"));

        assert!(!rendered.contains("<script>alert(1)</script>"));
        assert!(!rendered.contains("<img onerror=x>"));
        assert!(rendered.contains(r#"value="a b&amp;c""#));
        // The "next page" link must survive a term with a space or an
        // ampersand in it — unencoded, `&c` truncates the query string.
        assert!(rendered.contains(r#"href="/v1/apis?offset=20&search=a+b%26c""#));
    }

    #[test]
    fn federated_page_tracks_the_click_but_never_the_paid_url() {
        let rendered = render_federated_resources(&federated_detail(json!([{
            "resource": "https://api.webbersites.com/v1/whois?plan=pro",
            "description": "WHOIS lookup",
        }])));

        // The human-facing link goes through the tracked hop...
        assert!(rendered.contains(
            r#"href="/f/webbersites?r=https%3A%2F%2Fapi.webbersites.com%2Fv1%2Fwhois%3Fplan%3Dpro""#
        ));
        // ...while the URL an agent copies stays the provider's own, verbatim.
        // Rewriting this one would make the provider's 402 challenge advertise a
        // `resource` that disagrees with the URL the client called.
        assert!(rendered.contains("<code>https://api.webbersites.com/v1/whois?plan=pro</code>"));
        assert!(!rendered.contains("<code>/f/webbersites"));
        assert!(rendered.contains("WHOIS lookup"));
    }

    #[test]
    fn federated_page_escapes_provider_supplied_text() {
        let rendered = render_federated_resources(&federated_detail(json!([{
            "resource": "https://api.example.com/v1/x",
            "description": "<script>alert(1)</script>",
        }])));

        assert!(rendered.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!rendered.contains("<script>"));
    }

    #[test]
    fn federated_page_without_resources_still_offers_the_provider_hop() {
        let rendered = render_federated_resources(&federated_detail(json!([])));

        assert!(rendered.contains(r#"href="/f/webbersites""#));
        assert!(rendered.contains("No resources published yet."));
    }

    #[test]
    fn endpoint_markdown_is_readable_and_html_safe() {
        let rendered = render_markdown_safe(
            "## Analyze text\n\nPass `with_sentiment=true` here.\n<script>alert(1)</script>",
        );

        assert!(rendered.contains("<h2>Analyze text</h2>"));
        assert!(rendered.contains("<code>with_sentiment=true</code>"));
        assert!(rendered.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!rendered.contains("<script>"));
    }

    #[test]
    fn pricing_summary_shows_default_priority_condition_and_price() {
        let rendered = render_pricing(
            0.03,
            &json!([{
                "parameter_name": "with_sentiment",
                "operator": "equals",
                "value": "true",
                "price": 0.05,
                "priority": 0
            }]),
        );

        assert!(rendered.contains("$0.03 default"));
        assert!(rendered.contains("P0"));
        assert!(rendered.contains("with_sentiment = true"));
        assert!(rendered.contains("$0.05"));
    }

    #[test]
    fn endpoint_detail_keeps_internal_docs_and_external_links_separate() {
        let endpoint = EndpointDocResponse {
            candidate_id: None,
            method: "GET".to_string(),
            path: "/anything/analyze".to_string(),
            full_url: "https://gateway.apiosk.com/cory-httpbin-qa/anything/analyze".to_string(),
            description: Some("Analyze text".to_string()),
            price_usd: 0.03,
            payment_required: true,
            request_body: None,
            response_body: None,
            request_example: Some(json!({"text": "Amsterdam is beautiful"})),
            response_example: Some(json!({"sentiment": "positive"})),
            headers: None,
            query_params: Some(json!([{"name": "with_sentiment", "type": "boolean"}])),
            documentation_markdown: Some("## Analyze text".to_string()),
            external_docs_url: Some("https://httpbin.org/".to_string()),
            openapi_url: Some("https://httpbin.org/spec.json".to_string()),
            authentication_notes: Some("Payment uses x402.".to_string()),
            pricing_rules: json!([]),
        };

        let rendered = render_endpoint_detail(&endpoint, 0);
        assert!(rendered.contains("id=\"endpoint-0\""));
        assert!(rendered.contains("<h2>Analyze text</h2>"));
        assert!(rendered.contains("Payment uses x402."));
        assert!(rendered.contains("External documentation"));
        assert!(rendered.contains("OpenAPI / Swagger"));
        assert!(rendered.contains("Amsterdam is beautiful"));
    }
}
