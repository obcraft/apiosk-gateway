use super::*;

pub(super) fn branded_api_manifest_page(
    api_slug: &str,
    endpoints: &[crate::db::Endpoint],
) -> Response<Body> {
    let endpoint_rows = if endpoints.is_empty() {
        "<tr><td colspan=\"4\">No endpoints configured yet.</td></tr>".to_string()
    } else {
        endpoints
            .iter()
            .map(|ep| {
                let path = normalize_path(&ep.path);
                let cost = if ep.payment_required {
                    format!("{} USDC", ep.price)
                } else {
                    "FREE".to_string()
                };
                let call_url = format!("https://gateway.apiosk.com/{}{}", api_slug, path);
                let method_upper = ep.method.to_uppercase();

                // For GET, use a simple link. For other methods, use a form.
                let action_cell = if method_upper == "GET" {
                    format!(
                        r#"<a href="{}" target="_blank" rel="noreferrer" class="try-btn">Try</a>"#,
                        escape_html(&call_url)
                    )
                } else {
                    // Use a form for POST/PUT/DELETE/PATCH - browsers can only submit GET/POST via forms,
                    // so we use POST and include the actual method as a hidden field for documentation.
                    // The gateway will receive a POST request which triggers the payment page.
                    format!(
                        r#"<form method="post" action="{}" target="_blank" style="display:inline;margin:0;">
                            <button type="submit" class="try-btn">Try {}</button>
                        </form>"#,
                        escape_html(&call_url),
                        escape_html(&method_upper)
                    )
                };

                format!(
                    "<tr>\
                        <td><code>{}</code></td>\
                        <td><code>{}</code></td>\
                        <td>{}</td>\
                        <td>{}</td>\
                     </tr>",
                    escape_html(&method_upper),
                    escape_html(&path),
                    escape_html(&cost),
                    action_cell
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };

    let html = format!(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>{api_slug} | Apiosk Gateway</title>
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
      .card {{ width:min(900px, 100%); background:#fff; border:1px solid #e5e7eb; border-radius:16px; padding:28px; box-shadow:0 4px 24px rgba(0,0,0,.06); }}
      .badge {{ display:inline-block; padding:5px 10px; border-radius:6px; font-size:11px; font-weight:600; letter-spacing:.03em; text-transform:uppercase; color:#1d4ed8; background:#eff6ff; border:1px solid #bfdbfe; }}
      h1 {{ margin:14px 0 8px; font-size:30px; font-weight:650; line-height:1.2; color:#111; }}
      p {{ margin:0; color:#6b7280; font-size:15px; line-height:1.5; }}
      table {{ width:100%; border-collapse:collapse; margin-top:22px; border:1px solid #e5e7eb; border-radius:10px; overflow:hidden; }}
      th, td {{ border-bottom:1px solid #e5e7eb; padding:10px 12px; text-align:left; font-size:14px; }}
      th {{ background:#f9fafb; color:#6b7280; text-transform:uppercase; font-size:11px; letter-spacing:.04em; }}
      tr:last-child td {{ border-bottom:none; }}
      code {{ font-size:12px; color:#111827; background:#f3f4f6; border-radius:4px; padding:2px 6px; }}
      a {{ color:#1d4ed8; text-decoration:none; }}
      a:hover {{ text-decoration:underline; }}
      .links {{ margin-top:20px; display:flex; gap:10px; flex-wrap:wrap; }}
      .links a {{ display:inline-flex; align-items:center; gap:6px; padding:8px 12px; font-size:13px; border:1px solid #d1d5db; border-radius:8px; background:#f9fafb; color:#111827; text-decoration:none; }}
      .links a:hover {{ background:#f3f4f6; }}
      .try-btn {{ display:inline-block; padding:6px 14px; font-size:13px; font-weight:500; color:#fff; background:#1d4ed8; border:none; border-radius:6px; cursor:pointer; text-decoration:none; }}
      .try-btn:hover {{ background:#1e40af; text-decoration:none; }}
    </style>
  </head>
  <body>
    <div class="wrap">
      <div class="card">
        <span class="badge">Provider Ready</span>
        <h1>{api_slug}</h1>
        <p>This provider is live. Use one of the configured routes below instead of the root path.</p>
        <table>
          <thead>
            <tr>
              <th>Method</th>
              <th>Path</th>
              <th>Price</th>
              <th>Action</th>
            </tr>
          </thead>
          <tbody>
            {endpoint_rows}
          </tbody>
        </table>
        <div class="links">
          <a href="/v1/apis?search={api_slug}">Browse API Metadata</a>
          <a href="/types">Explore Types</a>
          <a href="https://apiosk.com/docs" target="_blank" rel="noreferrer">Documentation</a>
        </div>
      </div>
    </div>
  </body>
</html>"#,
        api_slug = escape_html(api_slug),
        endpoint_rows = endpoint_rows
    );

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/html; charset=utf-8")
        .header("Cache-Control", "no-store")
        .header("X-Apiosk-Gateway", "true")
        .body(Body::from(html))
        .unwrap_or_else(|_| Response::new(Body::from("Gateway response error")))
}
