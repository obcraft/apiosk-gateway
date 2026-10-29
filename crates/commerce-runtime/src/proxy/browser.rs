use super::*;

pub(super) fn is_browser_html_request(method: &Method, headers: &HeaderMap<HeaderValue>) -> bool {
    // Only allow browser mode for safe methods (GET/HEAD) and form-submitted methods (POST/PUT/DELETE)
    let allowed_methods = matches!(
        *method,
        Method::GET | Method::HEAD | Method::POST | Method::PUT | Method::DELETE | Method::PATCH
    );
    if !allowed_methods {
        return false;
    }

    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Check content-type for form submissions (POST/PUT/DELETE from forms)
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let is_form_submission = content_type.contains("application/x-www-form-urlencoded")
        || content_type.contains("multipart/form-data");

    // Browser request if: Accept contains text/html, or is a form submission from a browser
    accept.contains("text/html")
        || (accept.contains("*/*") && user_agent.contains("Mozilla"))
        || (is_form_submission && user_agent.contains("Mozilla"))
}

pub(crate) fn json_error_response(status: StatusCode, code: &str, message: &str) -> Response<Body> {
    let body = serde_json::json!({
        "error": code,
        "message": message
    });

    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .header("X-Apiosk-Gateway", "true")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Response::new(Body::from(r#"{"error":"response_build_error"}"#)))
}

// `external_listing_response` lived here: the non-payable 404 a federated slug
// used to get, telling the caller to "pay the provider at its own x402
// resource". Both the divert and the message are gone — a federated listing is
// proxied through the gateway and paid for by the resell leg, and publishing
// the provider's address was a route around the buyer's 15%.

pub(super) fn extract_preset_auth_headers(metadata: &serde_json::Value) -> Vec<(String, String)> {
    extract_preset_auth_kv(metadata, "auth_headers")
}

/// Provider-configured upstream credentials to inject as query params
/// (`listing_metadata.auth_query`), e.g. `?token=<key>`. Same shape as
/// `auth_headers`: an array of `{ name, value }`.
pub(super) fn extract_preset_auth_query(metadata: &serde_json::Value) -> Vec<(String, String)> {
    extract_preset_auth_kv(metadata, "auth_query")
}

fn extract_preset_auth_kv(metadata: &serde_json::Value, key: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(items) = metadata.get(key).and_then(|v| v.as_array()) else {
        return out;
    };

    for item in items {
        let Some(name) = item.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(value) = item.get("value").and_then(|v| v.as_str()) else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || value.is_empty() {
            continue;
        }
        out.push((name.to_string(), value.to_string()));
    }

    out
}

#[allow(clippy::too_many_arguments)]
pub(super) fn branded_status_page(
    status: StatusCode,
    title: &str,
    description: &str,
    api_slug: &str,
    path: &str,
    requested_method: &Method,
    allowed_methods: &[String],
    extra_meta: &[(&str, String)],
    price_usdc: Option<BigDecimal>,
) -> Response<Body> {
    let full_path = if path.starts_with('/') {
        format!("/{api_slug}{path}")
    } else {
        format!("/{api_slug}/{path}")
    };

    let status_text = format!(
        "{} {}",
        status.as_u16(),
        status.canonical_reason().unwrap_or("Unknown")
    );
    let allowed_text = if allowed_methods.is_empty() {
        "Not specified".to_string()
    } else {
        allowed_methods.join(", ")
    };

    let mut meta_rows = vec![
        ("Provider".to_string(), api_slug.to_string()),
        ("Route".to_string(), full_path),
        ("Requested method".to_string(), requested_method.to_string()),
        ("Allowed methods".to_string(), allowed_text),
        ("Gateway status".to_string(), status_text.clone()),
    ];
    for (k, v) in extra_meta {
        meta_rows.push(((*k).to_string(), v.clone()));
    }

    let metadata_html = meta_rows
        .into_iter()
        .map(|(k, v)| {
            format!(
                "<div class=\"row\"><span>{}</span><strong>{}</strong></div>",
                escape_html(&k),
                escape_html(&v)
            )
        })
        .collect::<Vec<_>>()
        .join("");

    let price_html = match price_usdc {
        Some(ref p) if p > &BigDecimal::from(0) => format!(
            r#"<div class="price-block"><span class="price-label">Endpoint price</span><span class="price-value">{} USDC</span></div>"#,
            p
        ),
        _ => String::new(),
    };

    let escaped_title = escape_html(title);
    let escaped_description = escape_html(description);
    let html = format!(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <link rel="icon" type="image/svg+xml" href="/favicon.svg?v=20260918" />
    <link rel="apple-touch-icon" sizes="180x180" href="/apple-touch-icon.png?v=20260918" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>{} | Apiosk Gateway</title>
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
      body {{ margin:0; font-family: ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, Helvetica, Arial, sans-serif; background:#fff; color:#1a1a2e; }}
      .wrap {{ min-height:100vh; display:flex; align-items:center; justify-content:center; padding:24px; }}
      .card {{ width:min(720px, 100%); background:#fff; border:1px solid #e5e7eb; border-radius:16px; padding:32px; box-shadow:0 4px 24px rgba(0,0,0,.06); }}
      .header {{ display:flex; align-items:center; gap:16px; margin-bottom:20px; }}
      .logo {{ height:28px; }}
      .badge {{ display:inline-block; padding:5px 10px; border-radius:6px; font-size:11px; font-weight:600; letter-spacing:.03em; text-transform:uppercase; color:#dc2626; background:#fef2f2; border:1px solid #fecaca; }}
      h1 {{ margin:0 0 8px; font-size:28px; font-weight:600; line-height:1.25; color:#111; }}
      p {{ margin:0; color:#6b7280; font-size:15px; line-height:1.5; }}
      .price-block {{ margin-top:20px; padding:16px 20px; background:linear-gradient(135deg, #f0fdf4 0%, #ecfdf5 100%); border:1px solid #bbf7d0; border-radius:12px; display:flex; justify-content:space-between; align-items:center; }}
      .price-label {{ color:#166534; font-size:14px; font-weight:500; }}
      .price-value {{ color:#15803d; font-size:24px; font-weight:700; }}
      .meta {{ margin-top:20px; border:1px solid #e5e7eb; border-radius:10px; overflow:hidden; background:#fafafa; }}
      .row {{ display:flex; justify-content:space-between; gap:16px; padding:12px 16px; border-top:1px solid #e5e7eb; }}
      .row:first-child {{ border-top:none; }}
      .row span {{ color:#6b7280; font-size:14px; }}
      .row strong {{ color:#111; font-size:14px; text-align:right; word-break:break-all; }}
      .links {{ margin-top:24px; padding-top:20px; border-top:1px solid #e5e7eb; display:flex; flex-wrap:wrap; gap:12px; }}
      .links a {{ display:inline-flex; align-items:center; gap:6px; padding:8px 14px; font-size:13px; font-weight:500; color:#374151; background:#f3f4f6; border-radius:8px; text-decoration:none; transition:background .15s; }}
      .links a:hover {{ background:#e5e7eb; }}
      .footer {{ margin-top:24px; text-align:center; color:#9ca3af; font-size:12px; }}
      .footer a {{ color:#6b7280; text-decoration:none; }}
    </style>
  </head>
  <body>
    <div class="wrap">
      <div class="card">
        <div class="header">
          <img src="/logo.png?v=20260918" alt="Apiosk" class="logo" />
          <span class="badge">{} {}</span>
        </div>
        <h1>{}</h1>
        <p>{}</p>{}
        <div class="meta">{}</div>
        <div class="links">
          <a href="/">Gateway Home</a>
          <a href="/v1/apis">Browse APIs</a>
          <a href="/types">Explore Types</a>
          <a href="https://apiosk.com/docs" target="_blank">Documentation</a>
        </div>
        <div class="footer">Powered by <a href="https://apiosk.com">Apiosk</a></div>
      </div>
    </div>
  </body>
</html>"#,
        escaped_title,
        status.as_u16(),
        status.canonical_reason().unwrap_or("Error"),
        escaped_title,
        escaped_description,
        price_html,
        metadata_html
    );

    Response::builder()
        .status(status)
        .header("Content-Type", "text/html; charset=utf-8")
        .header("Cache-Control", "no-store")
        .header("X-Apiosk-Gateway", "true")
        .body(Body::from(html))
        .unwrap_or_else(|_| Response::new(Body::from("Gateway response error")))
}

pub(super) fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&#39;")
}
