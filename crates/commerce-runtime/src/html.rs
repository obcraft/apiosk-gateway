use axum::http::header;

/// Check if the request is from a browser (wants HTML)
pub fn is_browser_request(headers: &axum::http::HeaderMap) -> bool {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    accept.contains("text/html") || (accept.contains("*/*") && user_agent.contains("Mozilla"))
}

/// Which browse surface the current page belongs to, so the shared top nav can
/// mark it. Every gateway HTML page belongs to exactly one — `Home` marks
/// nothing, since the brand logo is the home link.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Home,
    Apis,
    Providers,
    Types,
    Publish,
}

impl Nav {
    fn current(self, item: Nav) -> &'static str {
        if self == item {
            r#" class="is-current""#
        } else {
            ""
        }
    }
}

/// The top nav shared by every HTML page: the gateway is a payment gateway,
/// not a provider directory, so APIs, providers, types and the docs all sit one
/// click away from wherever the visitor landed.
fn html_nav(nav: Nav) -> String {
    format!(
        r#"<nav class="nav">
        <a class="nav-brand" href="/"><img src="/logo.png?v=20260918" alt="Apiosk" /></a>
        <a href="/v1/apis"{apis}>APIs</a>
        <a href="/v1/providers"{providers}>Providers</a>
        <a href="/types"{types}>Types</a>
        <a href="https://apiosk.com/docs">Docs</a>
        <span class="nav-spacer"></span>
        <a class="nav-cta" href="/federated/new"{publish}>Publish an API</a>
      </nav>"#,
        apis = nav.current(Nav::Apis),
        providers = nav.current(Nav::Providers),
        types = nav.current(Nav::Types),
        publish = nav.current(Nav::Publish),
    )
}

/// Canonical public origin, used to build absolute canonical/Open Graph URLs so
/// crawlers index one stable host regardless of the Host header they arrive on.
/// Kept in sync with `GATEWAY_BASE_URL` in `gateway_routes.rs`.
pub const SITE_BASE_URL: &str = "https://gateway.apiosk.com";

/// Common HTML head with styles.
///
/// `description` is the per-page meta description (search snippet + Open Graph);
/// keep it under ~155 chars. `canonical_path` is this page's own path
/// (e.g. `/v1/apis/weather-api`); it is resolved against [`SITE_BASE_URL`] into
/// an absolute `<link rel="canonical">` + `og:url` so duplicate query-string
/// variants collapse to one indexed URL.
pub fn html_head(title: &str, nav: Nav, description: &str, canonical_path: &str) -> String {
    let desc = escape_html(description);
    let canonical = format!("{SITE_BASE_URL}{canonical_path}");
    format!(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <link rel="icon" type="image/png" href="/favicon-96x96.png?v=20260918" sizes="96x96" media="(prefers-color-scheme: light)" />
    <link rel="icon" type="image/png" href="/favicon-dark-96x96.png?v=20260918" sizes="96x96" media="(prefers-color-scheme: dark)" />
    <link rel="icon" type="image/svg+xml" href="/favicon.svg?v=20260918" />
    <link rel="icon" href="/favicon.ico?v=20260918" media="(prefers-color-scheme: light)" />
    <link rel="icon" href="/favicon-dark.ico?v=20260918" media="(prefers-color-scheme: dark)" />
    <link rel="apple-touch-icon" sizes="180x180" href="/apple-touch-icon.png?v=20260918" />
    <link rel="manifest" href="/site.webmanifest?v=20260918" />
    <title>{title} | Apiosk Gateway</title>
    <meta name="description" content="{desc}" />
    <meta name="robots" content="index, follow" />
    <link rel="canonical" href="{canonical}" />
    <meta property="og:type" content="website" />
    <meta property="og:site_name" content="Apiosk Gateway" />
    <meta property="og:title" content="{title} | Apiosk Gateway" />
    <meta property="og:description" content="{desc}" />
    <meta property="og:url" content="{canonical}" />
    <meta property="og:image" content="https://apiosk.com/og-image-v10.png" />
    <meta property="og:image:width" content="1200" />
    <meta property="og:image:height" content="630" />
    <meta name="twitter:card" content="summary_large_image" />
    <meta name="twitter:image" content="https://apiosk.com/og-image-v10.png" />
    <meta name="twitter:title" content="{title} | Apiosk Gateway" />
    <meta name="twitter:description" content="{desc}" />
    <!-- WebMCP: on a browser that supports it, an agent landing on any catalog
         page gets search/lookup tools over the catalog instead of having to
         scrape this HTML. Catalog reads are free, so no payment path is
         reachable here. On every other browser the script exits immediately
         without fetching or logging, which is nearly all of them today. -->
    <script src="/webmcp.js" data-apiosk-catalog="1" data-apiosk-base="{SITE_BASE_URL}" async></script>
    <!-- Google tag (gtag.js) -->
    <script async src="https://www.googletagmanager.com/gtag/js?id=G-4K2P95VDWT"></script>
    <script>
      window.dataLayer = window.dataLayer || [];
      function gtag(){{dataLayer.push(arguments);}}
      gtag('js', new Date());

      gtag('config', 'G-4K2P95VDWT');
    </script>
    <style>
      :root {{ color-scheme: light; }}
      * {{ box-sizing: border-box; }}
      html, body {{ max-width:100%; overflow-x:hidden; }}
      body {{ margin:0; font-family: ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, Helvetica, Arial, sans-serif; background:#fff; color:#1a1a2e; }}
      .wrap {{ min-height:100vh; width:100%; padding:22px 40px 48px; }}

      /* Shared top nav */
      .nav {{ display:flex; align-items:center; gap:22px; flex-wrap:wrap; margin:0 0 30px; padding-bottom:14px; border-bottom:1px solid #e5e7eb; }}
      .nav a {{ color:#6b7280; font-size:13.5px; text-decoration:none; }}
      .nav a:hover {{ color:#111; }}
      .nav a.is-current {{ color:#111; font-weight:600; }}
      .nav .nav-brand {{ display:flex; align-items:center; margin-right:4px; }}
      .nav .nav-brand img {{ display:block; height:20px; }}
      .nav .nav-spacer {{ flex:1 1 auto; }}
      .nav .nav-cta {{ padding:6px 12px; border:1px solid #e5e7eb; border-radius:8px; color:#111; font-size:13px; font-weight:500; }}
      .nav .nav-cta:hover {{ background:#fafafa; border-color:#d1d5db; }}

      .eyebrow {{ margin:0 0 20px; font-size:12px; color:#9ca3af; }}
      .eyebrow a {{ color:#6b7280; text-decoration:none; }}
      .eyebrow a:hover {{ color:#111; }}
      .header-row {{ display:flex; align-items:flex-start; justify-content:space-between; gap:24px; flex-wrap:wrap; margin-bottom:28px; }}
      .badge {{ display:inline-block; padding:3px 9px; border-radius:999px; font-size:11px; font-weight:500; color:#059669; background:#ecfdf5; border:1px solid #a7f3d0; }}
      .badge-purple {{ color:#7c3aed; background:#f5f3ff; border-color:#ddd6fe; }}
      h1 {{ margin:0 0 6px; font-size:22px; font-weight:600; color:#111; letter-spacing:-.01em; }}
      .subtitle {{ margin:0; color:#6b7280; font-size:14px; line-height:1.5; max-width:640px; }}
      .section {{ margin-bottom:32px; }}
      .section-title {{ font-size:11px; font-weight:600; text-transform:uppercase; letter-spacing:.06em; color:#9ca3af; margin-bottom:10px; }}

      /* Search */
      .search-box {{ width:100%; max-width:340px; }}
      .search-box input {{ width:100%; padding:9px 14px; border:1px solid #e5e7eb; border-radius:8px; font-size:13.5px; color:#111; }}
      .search-box input:focus {{ outline:none; border-color:#a1a1aa; }}
      .search-box input::placeholder {{ color:#9ca3af; }}

      /* Tag pills (filters + detail tags) */
      .tags {{ display:flex; gap:8px; flex-wrap:wrap; }}
      .tag {{ display:inline-flex; align-items:center; padding:5px 12px; border-radius:999px; font-size:11px; font-weight:600; letter-spacing:.03em; text-transform:uppercase; color:#4b5563; background:#f9fafb; border:1px solid #e5e7eb; text-decoration:none; }}
      .tag:hover {{ background:#f3f4f6; border-color:#d1d5db; }}
      .tag.is-active {{ color:#111; background:#f3f4f6; border-color:#d1d5db; }}

      /* Hairline card grid, shared by listings and link groups. The hairlines
         are drawn by the cards themselves (not by a gap over a grey backdrop),
         so a half-filled last row ends in white rather than a grey block. */
      .grid {{ display:grid; grid-template-columns:repeat(auto-fill, minmax(min(260px, 100%), 1fr)); overflow:hidden; background:#fff; border:1px solid #e5e7eb; border-radius:12px; }}
      .grid.api-grid {{ grid-template-columns:repeat(5, minmax(0, 1fr)); }}
      .grid.explore-grid {{ grid-template-columns:repeat(4, minmax(0, 1fr)); }}
      /* Leading edges, not trailing ones: a card paints after its neighbour, so
         only a border on its own left/top survives the overlap. The negative
         margin lays that border over the neighbour (or over the container
         border, for the first row and column). */
      .grid > * {{ border-left:1px solid #e5e7eb; border-top:1px solid #e5e7eb; margin:-1px 0 0 -1px; }}
      .api-card, .link-card {{ display:flex; flex-direction:column; height:100%; padding:18px 20px; background:#fff; text-decoration:none; min-width:0; }}
      .api-card:hover, .link-card:hover {{ background:#fafafa; }}
      .api-card h3, .link-card h3 {{ margin:0 0 5px; font-size:13.5px; line-height:1.3; font-weight:600; color:#111; }}
      .api-card h3 {{ display:-webkit-box; min-height:35px; -webkit-line-clamp:2; -webkit-box-orient:vertical; overflow:hidden; }}
      .api-card p, .link-card p {{ margin:0 0 12px; font-size:13px; color:#6b7280; line-height:1.45; display:-webkit-box; -webkit-line-clamp:2; -webkit-box-orient:vertical; overflow:hidden; }}
      .api-card .card-top {{ display:flex; align-items:center; justify-content:space-between; gap:8px; margin-bottom:10px; min-width:0; }}
      .api-card .provider {{ display:flex; align-items:center; gap:7px; min-width:0; color:#6b7280; font-size:11.5px; }}
      /* The frame holds the provider initial; the logo (if any) is layered
         over it and removes itself when it fails to load, so a dead or
         cross-origin-blocked logo URL degrades to the initial. */
      .api-card .provider-logo {{ position:relative; display:grid; place-items:center; width:20px; height:20px; flex:0 0 20px; border-radius:5px; overflow:hidden; border:1px solid #e5e7eb; background:#f9fafb; color:#6b7280; font-size:10px; font-weight:600; }}
      .api-card .provider-logo img {{ position:absolute; inset:0; width:100%; height:100%; object-fit:cover; background:#f9fafb; }}
      .api-card .provider-name {{ overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }}
      .api-card .meta {{ display:flex; align-items:center; justify-content:space-between; gap:10px; width:100%; margin-top:auto; padding-top:12px; border-top:1px solid #f3f4f6; }}
      .api-card .category {{ font-size:10.5px; font-weight:600; letter-spacing:.04em; text-transform:uppercase; color:#9ca3af; }}
      .api-card .price {{ font-size:12.5px; color:#059669; font-weight:600; }}
      .api-card .view-link, .link-card code {{ font-size:11.5px; font-weight:600; letter-spacing:.03em; text-transform:uppercase; color:#7c3aed; }}
      .link-card code {{ margin-top:auto; text-transform:none; font-weight:400; letter-spacing:0; font-family:ui-monospace, monospace; }}
      .api-card .view-link {{ display:inline-flex; align-items:center; gap:4px; }}

      .breadcrumb {{ margin-bottom:16px; font-size:13px; color:#6b7280; }}
      .breadcrumb a {{ color:#7c3aed; text-decoration:none; }}
      .breadcrumb a:hover {{ text-decoration:underline; }}

      /* Endpoint table (API detail page) */
      .filter-tabs {{ display:flex; gap:6px; }}
      .filter-tabs button {{ appearance:none; border:1px solid #e5e7eb; background:#fff; color:#6b7280; font-size:12px; font-weight:600; padding:6px 14px; border-radius:999px; cursor:pointer; font-family:inherit; }}
      .filter-tabs button:hover {{ border-color:#d1d5db; color:#111; }}
      .filter-tabs button.is-active {{ background:#111; border-color:#111; color:#fff; }}
      table {{ width:100%; border-collapse:collapse; }}
      th, td {{ text-align:left; padding:12px; border-bottom:1px solid #e5e7eb; font-size:13.5px; }}
      th {{ font-weight:600; color:#9ca3af; font-size:11px; text-transform:uppercase; letter-spacing:.05em; }}
      .method {{ display:inline-block; padding:2px 8px; border-radius:4px; font-size:11px; font-weight:600; }}
      .method.GET {{ background:#f5f3ff; color:#6d28d9; }}
      .method.POST {{ background:#eff6ff; color:#1d4ed8; }}
      .method.PUT {{ background:#fefce8; color:#a16207; }}
      .method.DELETE {{ background:#fef2f2; color:#b91c1c; }}
      .method.PATCH {{ background:#f5f3ff; color:#7c3aed; }}
      .path {{ font-family:ui-monospace, monospace; font-size:13px; color:#111; }}
      .endpoint-desc {{ color:#6b7280; }}
      .price-cell {{ color:#059669; font-weight:600; white-space:nowrap; }}
      .price-default {{ color:#059669; font-weight:600; white-space:nowrap; }}
      .pricing-rule {{ margin-top:4px; color:#4b5563; font-size:11.5px; font-family:ui-monospace, monospace; white-space:nowrap; }}
      .priority-pill {{ display:inline-block; margin-right:5px; padding:1px 5px; border-radius:4px; color:#6d28d9; background:#f5f3ff; font-size:10px; font-weight:600; }}
      .view-doc {{ font-size:11.5px; font-weight:600; letter-spacing:.03em; text-transform:uppercase; color:#7c3aed; text-decoration:none; white-space:nowrap; }}
      .view-doc:hover {{ text-decoration:underline; }}

      .endpoint-details {{ display:grid; gap:16px; margin-top:20px; }}
      .endpoint-detail {{ border:1px solid #e5e7eb; padding:20px; scroll-margin-top:20px; }}
      .endpoint-detail-head {{ display:flex; align-items:center; justify-content:space-between; gap:16px; flex-wrap:wrap; margin-bottom:18px; }}
      .endpoint-detail-title {{ display:flex; align-items:center; gap:10px; min-width:0; }}
      .endpoint-detail-title code {{ color:#111; font-size:14px; overflow-wrap:anywhere; }}
      .endpoint-detail-grid {{ display:grid; grid-template-columns:repeat(2, minmax(0, 1fr)); gap:18px; }}
      .detail-block {{ min-width:0; }}
      .detail-block.full {{ grid-column:1 / -1; }}
      .detail-label {{ margin-bottom:7px; color:#9ca3af; font-size:10.5px; font-weight:600; letter-spacing:.05em; text-transform:uppercase; }}
      .detail-text {{ margin:0; color:#4b5563; font-size:13.5px; line-height:1.6; white-space:pre-wrap; }}
      .doc-markdown {{ color:#374151; font-size:13.5px; line-height:1.65; }}
      .doc-markdown h1, .doc-markdown h2, .doc-markdown h3 {{ margin:16px 0 7px; color:#111; font-weight:600; line-height:1.3; }}
      .doc-markdown h1:first-child, .doc-markdown h2:first-child, .doc-markdown h3:first-child {{ margin-top:0; }}
      .doc-markdown h1 {{ font-size:19px; }}
      .doc-markdown h2 {{ font-size:17px; }}
      .doc-markdown h3 {{ font-size:15px; }}
      .doc-markdown p {{ margin:0 0 9px; }}
      .doc-markdown code {{ padding:1px 4px; border-radius:4px; color:#6d28d9; background:#f5f3ff; font-family:ui-monospace, monospace; font-size:12px; }}
      .json-example {{ margin:0; padding:12px; overflow:auto; border:1px solid #e5e7eb; background:#fafafa; color:#374151; font-family:ui-monospace, monospace; font-size:12px; line-height:1.55; white-space:pre; }}
      .doc-links {{ display:flex; gap:12px; flex-wrap:wrap; }}
      .doc-links a {{ color:#7c3aed; font-size:12px; font-weight:600; text-decoration:none; }}
      .doc-links a:hover {{ text-decoration:underline; }}

      .stat-box {{ border:1px solid #e5e7eb; padding:14px 20px; min-width:140px; }}
      .stat-box .stat-label {{ font-size:10.5px; font-weight:600; letter-spacing:.05em; text-transform:uppercase; color:#9ca3af; margin-bottom:4px; }}
      .stat-box .stat-value {{ font-size:19px; font-weight:600; color:#111; font-family:ui-monospace, monospace; }}

      .specs {{ border:1px solid #e5e7eb; }}
      .specs .spec-row {{ display:flex; justify-content:space-between; gap:12px; padding:11px 20px; font-size:13px; border-top:1px solid #f3f4f6; }}
      .specs .spec-row:first-child {{ border-top:none; }}
      .specs .spec-row .k {{ color:#6b7280; }}
      .specs .spec-row .v {{ color:#111; font-weight:500; font-family:ui-monospace, monospace; }}

      .pagination {{ display:flex; gap:8px; margin-top:20px; }}
      .pagination a {{ padding:8px 14px; border:1px solid #e5e7eb; text-decoration:none; color:#6b7280; font-size:13px; }}
      .pagination a:hover {{ background:#f9fafb; }}
      .empty {{ text-align:center; padding:60px 20px; color:#9ca3af; border:1px solid #e5e7eb; }}

      /* Provider directory cards (home + /v1/providers) */
      .provider-card {{ display:flex; flex-direction:column; gap:10px; height:100%; padding:20px; background:#fff; text-decoration:none; min-width:0; }}
      .provider-card:hover {{ background:#fafafa; }}
      .provider-card .p-head {{ display:flex; align-items:center; gap:11px; min-width:0; }}
      .provider-card .p-logo {{ display:grid; place-items:center; width:44px; height:44px; flex:0 0 44px; border-radius:12px; border:1px solid #e5e7eb; background:#f9fafb; overflow:hidden; }}
      .provider-card h3 {{ margin:0; font-size:14px; font-weight:600; color:#111; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }}
      .provider-card .p-sector {{ font-size:11px; color:#9ca3af; text-transform:uppercase; letter-spacing:.04em; }}
      .provider-card p {{ margin:0; font-size:12.5px; color:#6b7280; line-height:1.45; display:-webkit-box; -webkit-line-clamp:2; -webkit-box-orient:vertical; overflow:hidden; }}
      .provider-card .p-meta {{ display:flex; align-items:center; gap:6px; flex-wrap:wrap; margin-top:auto; padding-top:12px; border-top:1px solid #f3f4f6; }}
      .pill {{ display:inline-flex; align-items:center; padding:3px 9px; border-radius:999px; font-size:10.5px; font-weight:600; letter-spacing:.03em; text-transform:uppercase; border:1px solid #e5e7eb; background:#f9fafb; color:#4b5563; }}
      .pill-count {{ color:#111; background:#fff; }}

      /* Home hero */
      .hero {{ margin-bottom:34px; }}
      .hero h1 {{ margin:12px 0 8px; font-size:30px; line-height:1.2; letter-spacing:-.02em; max-width:640px; }}
      .hero .subtitle {{ font-size:15px; line-height:1.6; max-width:600px; }}
      .hero-stats {{ margin-top:14px; color:#6b7280; font-size:13px; }}
      .hero-stats strong {{ color:#111; font-weight:600; font-family:ui-monospace, monospace; }}

      /* Numbered "how a paid call works" strip */
      .steps {{ display:grid; grid-template-columns:repeat(3, minmax(0, 1fr)); gap:1px; overflow:hidden; background:#e5e7eb; border:1px solid #e5e7eb; border-radius:12px; }}
      .step {{ padding:18px 20px; background:#fff; }}
      .step .step-n {{ display:grid; place-items:center; width:20px; height:20px; margin-bottom:10px; border-radius:999px; background:#f5f3ff; color:#6d28d9; font-size:11px; font-weight:600; }}
      .step h3 {{ margin:0 0 5px; font-size:13.5px; font-weight:600; color:#111; }}
      .step p {{ margin:0; font-size:12.5px; line-height:1.5; color:#6b7280; }}
      .step code {{ font-family:ui-monospace, monospace; font-size:12px; color:#6d28d9; }}

      /* The caveat under a step strip: the parameters and limits that apply to
         all three steps at once, and so belong to none of them individually. */
      .section-note {{ margin:10px 2px 0; font-size:12.5px; line-height:1.55; color:#6b7280; }}
      .section-note code {{ font-family:ui-monospace, monospace; font-size:12px; color:#6d28d9; }}

      .quicklinks {{ display:flex; gap:8px; flex-wrap:wrap; }}
      .quicklinks a {{ display:inline-flex; align-items:center; gap:8px; padding:8px 13px; border:1px solid #e5e7eb; border-radius:8px; text-decoration:none; background:#fff; }}
      .quicklinks a:hover {{ background:#fafafa; border-color:#d1d5db; }}
      .quicklinks code {{ font-family:ui-monospace, monospace; font-size:12px; color:#7c3aed; }}
      .quicklinks span {{ font-size:12.5px; color:#6b7280; }}

      .footer {{ margin-top:40px; padding-top:20px; border-top:1px solid #e5e7eb; color:#9ca3af; font-size:12px; }}
      .footer a {{ color:#6b7280; text-decoration:none; }}

      @media (max-width: 1280px) {{
        .grid.api-grid {{ grid-template-columns:repeat(3, minmax(0, 1fr)); }}
      }}
      @media (max-width: 840px) {{
        .grid.api-grid, .grid.explore-grid {{ grid-template-columns:repeat(2, minmax(0, 1fr)); }}
        .steps {{ grid-template-columns:1fr; }}
      }}
      @media (max-width: 640px) {{
        .wrap {{ padding:16px 16px 32px; }}
        h1 {{ font-size:19px; }}
        .hero h1 {{ font-size:24px; }}
        .nav {{ gap:14px; margin-bottom:22px; }}
        .nav .nav-spacer {{ display:none; }}
        .grid, .grid.api-grid, .grid.explore-grid {{ grid-template-columns:1fr; }}
        .header-row {{ flex-direction:column; }}
        .search-box {{ max-width:100%; }}
        table, thead, tbody, th, td, tr {{ display:block; }}
        thead {{ display:none; }}
        tr {{ border-bottom:1px solid #e5e7eb; padding:10px 0; }}
        td {{ border-bottom:none; padding:3px 0; }}
        .endpoint-detail-grid {{ grid-template-columns:1fr; }}
        .detail-block.full {{ grid-column:auto; }}
      }}
    </style>
  </head>
  <body>
    <div class="wrap">
      {nav}"#,
        title = title,
        nav = html_nav(nav),
    )
}

/// Common HTML footer
pub fn html_footer() -> &'static str {
    r#"
      <div class="footer">
        <a href="/">Home</a> &middot;
        <a href="https://apiosk.com/docs" target="_blank">Documentation</a> &middot;
        <a href="https://apiosk.com" target="_blank">Apiosk.com</a>
      </div>
    </div>
  </body>
</html>"#
}

/// Escape HTML special characters
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Format price for display
pub fn format_price(price: f64) -> String {
    if price == 0.0 {
        "Free".to_string()
    } else if price < 0.01 {
        format!("${:.4}", price)
    } else {
        format!("${:.2}", price)
    }
}
