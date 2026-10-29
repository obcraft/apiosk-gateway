use super::*;

#[derive(Debug, Clone)]
pub(super) struct CheckoutBanner {
    pub tone: CheckoutBannerTone,
    pub title: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum CheckoutBannerTone {
    Info,
    Warning,
    Danger,
}

#[derive(Debug, Clone)]
pub(super) struct PaymentCheckoutOptions {
    pub dashboard_url: String,
    pub allow_credits: bool,
    pub allow_x402: bool,
    pub credits_actions_enabled: bool,
    pub wallet_actions_enabled: bool,
    pub credits_hint: Option<String>,
    pub wallet_hint: Option<String>,
    pub banner: Option<CheckoutBanner>,
}

impl PaymentCheckoutOptions {
    pub(super) fn new(dashboard_url: impl Into<String>) -> Self {
        Self {
            dashboard_url: dashboard_url.into(),
            allow_credits: true,
            allow_x402: true,
            credits_actions_enabled: true,
            wallet_actions_enabled: true,
            credits_hint: None,
            wallet_hint: None,
            banner: None,
        }
    }
}

pub(super) fn payment_checkout_page(
    requirement: &PaymentRequirement,
    api_slug: &str,
    path: &str,
    options: &PaymentCheckoutOptions,
) -> Response<Body> {
    let full_path = if path.starts_with('/') {
        format!("/{api_slug}{path}")
    } else {
        format!("/{api_slug}/{path}")
    };

    let amount_atomic = requirement.max_amount_required.parse::<u128>().unwrap_or(0);
    let amount_usdc = amount_atomic as f64 / 1_000_000.0;
    let amount_display = if amount_usdc == amount_usdc.floor() {
        format!("{:.0}", amount_usdc)
    } else {
        format!("{:.6}", amount_usdc)
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    };

    let chain_id = if requirement.network == "base-sepolia" {
        84532
    } else {
        8453
    };

    let rail_label = match (options.allow_credits, options.allow_x402) {
        (true, true) => "Credits or wallet",
        (true, false) => "Credits only",
        (false, true) => "Wallet only",
        (false, false) => "Payment unavailable",
    };

    let rail_copy = match (options.allow_credits, options.allow_x402) {
        (true, true) => {
            "Use your Apiosk credits wallet or pay directly with an x402-compatible wallet."
        }
        (true, false) => "This request can only be paid through your Apiosk credits wallet.",
        (false, true) => "This request can only be paid with an x402-compatible wallet.",
        (false, false) => "No supported payment rail is currently configured for this request.",
    };

    let credits_section_html = if options.allow_credits {
        let disabled_class = if options.credits_actions_enabled {
            ""
        } else {
            " option-card-disabled"
        };
        let credits_note = escape_html(options.credits_hint.as_deref().unwrap_or(
            "Link your dashboard session to bill this request from your credits wallet, or top up and retry.",
        ));
        let credits_actions_html = if options.credits_actions_enabled {
            r#"
            <div class="option-actions">
              <button class="btn btn-secondary" id="use-credits-btn">Use credits</button>
              <button class="btn btn-ghost" id="topup-btn">Top up and retry</button>
            </div>
            "#
            .to_string()
        } else {
            r#"
            <div class="disabled-copy">
              Credits are enabled for this API, but this exact request cannot be billed in credits right now.
            </div>
            "#
            .to_string()
        };

        format!(
            r#"
            <section class="option-card{}">
              <div class="option-label">Credits</div>
              <h2>Use your Apiosk balance</h2>
              <p>{}</p>
              <div class="status-row" id="credits-status">
                <span class="status-dot"></span>
                <span class="status-copy">Not linked to a dashboard session</span>
              </div>
              {}
            </section>
            "#,
            disabled_class, credits_note, credits_actions_html
        )
    } else {
        String::new()
    };

    let wallet_section_html = if options.allow_x402 {
        let disabled_class = if options.wallet_actions_enabled {
            ""
        } else {
            " option-card-disabled"
        };
        let wallet_note = escape_html(options.wallet_hint.as_deref().unwrap_or(
            "Connect a browser wallet, sign the x402 payment proof, and replay the request immediately.",
        ));
        let wallet_actions_html = if options.wallet_actions_enabled {
            format!(
                r#"
                <div class="status-row" id="wallet-status">
                  <span class="status-dot"></span>
                  <span class="status-copy">Wallet not connected</span>
                </div>
                <div class="option-actions">
                  <button class="btn btn-primary" id="connect-btn">Connect wallet</button>
                  <button class="btn btn-success hidden" id="pay-btn" disabled>Pay {} USDC</button>
                </div>
                "#,
                escape_html(&amount_display)
            )
        } else {
            format!(
                r#"
                <div class="disabled-copy">
                  {}
                </div>
                "#,
                wallet_note
            )
        };

        format!(
            r#"
            <section class="option-card{}">
              <div class="option-label">x402 Wallet</div>
              <h2>Pay from your wallet</h2>
              <p>{}</p>
              {}
            </section>
            "#,
            disabled_class, wallet_note, wallet_actions_html
        )
    } else {
        String::new()
    };

    let banner_html = options
        .banner
        .as_ref()
        .map(|banner| {
            format!(
                r#"<div class="banner banner-{}"><strong>{}</strong><span>{}</span></div>"#,
                banner.tone.css_class(),
                escape_html(&banner.title),
                escape_html(&banner.message)
            )
        })
        .unwrap_or_default();

    let page_config_json = serde_json::json!({
        "amountDisplay": amount_display,
        "allowCredits": options.allow_credits,
        "allowX402": options.allow_x402,
        "creditsActionsEnabled": options.credits_actions_enabled,
        "walletActionsEnabled": options.wallet_actions_enabled,
        "dashboardUrl": options.dashboard_url.as_str(),
        "topUpAmountEur": format!("{:.2}", amount_usdc.max(0.01)),
        "requirement": {
            "scheme": requirement.scheme.as_str(),
            "network": requirement.network.as_str(),
            "maxAmountRequired": requirement.max_amount_required.as_str(),
            "resource": requirement.resource.as_str(),
            "payTo": requirement.pay_to.as_str(),
            "asset": requirement.asset.as_str(),
            "chainId": chain_id,
            // Pass through verbatim. The page's wallet JS only runs on EVM
            // networks (hosted checkout is gated to Base upstream).
            "extra": requirement.extra.clone(),
        }
    })
    .to_string();

    let html = format!(
        r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
    <link rel="icon" type="image/svg+xml" href="/favicon.svg?v=20260918" />
    <link rel="apple-touch-icon" sizes="180x180" href="/apple-touch-icon.png?v=20260918" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Payment Required | Apiosk Gateway</title>
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
    body {{ margin:0; font-family: ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, Helvetica, Arial, sans-serif; background:linear-gradient(180deg, #f8fafc 0%, #eef2ff 100%); color:#111827; }}
    .wrap {{ min-height:100vh; display:flex; align-items:center; justify-content:center; padding:24px; }}
    .card {{ width:min(920px, 100%); background:#fff; border:1px solid #dbe4ff; border-radius:24px; padding:32px; box-shadow:0 18px 56px rgba(15, 23, 42, .12); }}
    .header {{ display:flex; align-items:center; justify-content:space-between; gap:16px; margin-bottom:24px; }}
    .brand {{ display:flex; align-items:center; gap:14px; }}
    .logo {{ height:28px; }}
    .badge {{ display:inline-flex; align-items:center; justify-content:center; padding:7px 12px; border-radius:999px; font-size:11px; font-weight:700; letter-spacing:.06em; text-transform:uppercase; color:#1d4ed8; background:#dbeafe; border:1px solid #93c5fd; }}
    .headline h1 {{ margin:0 0 8px; font-size:32px; line-height:1.15; font-weight:700; color:#0f172a; }}
    .headline p {{ margin:0; color:#475569; font-size:15px; line-height:1.6; max-width:680px; }}
    .banner {{ margin-top:20px; display:grid; gap:6px; padding:14px 16px; border-radius:14px; border:1px solid transparent; }}
    .banner strong {{ font-size:14px; color:#0f172a; }}
    .banner span {{ font-size:14px; line-height:1.5; }}
    .banner-info {{ background:#eff6ff; border-color:#bfdbfe; color:#1d4ed8; }}
    .banner-warning {{ background:#fffbeb; border-color:#fcd34d; color:#b45309; }}
    .banner-danger {{ background:#fef2f2; border-color:#fecaca; color:#b91c1c; }}
    .price-block {{ margin-top:24px; padding:18px 20px; background:linear-gradient(135deg, #ecfeff 0%, #eff6ff 100%); border:1px solid #bfdbfe; border-radius:16px; display:flex; justify-content:space-between; align-items:center; gap:16px; }}
    .price-label {{ color:#0f766e; font-size:14px; font-weight:600; }}
    .price-value {{ color:#1d4ed8; font-size:30px; font-weight:800; }}
    .meta {{ margin-top:20px; border:1px solid #e2e8f0; border-radius:16px; overflow:hidden; background:#f8fafc; }}
    .row {{ display:flex; justify-content:space-between; gap:16px; padding:14px 18px; border-top:1px solid #e2e8f0; }}
    .row:first-child {{ border-top:none; }}
    .row span {{ color:#64748b; font-size:14px; }}
    .row strong {{ color:#0f172a; font-size:14px; text-align:right; word-break:break-all; }}
    .payment-grid {{ margin-top:24px; display:grid; grid-template-columns:repeat(auto-fit, minmax(280px, 1fr)); gap:18px; }}
    .option-card {{ border:1px solid #dbe4ff; border-radius:18px; padding:20px; background:#f8fbff; display:flex; flex-direction:column; gap:12px; }}
    .option-card-disabled {{ opacity:.7; background:#f8fafc; border-color:#e2e8f0; }}
    .option-label {{ display:inline-flex; align-self:flex-start; padding:6px 10px; border-radius:999px; background:#e0f2fe; color:#0369a1; font-size:11px; font-weight:700; letter-spacing:.05em; text-transform:uppercase; }}
    .option-card h2 {{ margin:0; font-size:22px; line-height:1.2; color:#0f172a; }}
    .option-card p {{ margin:0; font-size:14px; line-height:1.6; color:#475569; }}
    .option-actions {{ display:grid; gap:10px; margin-top:auto; }}
    .status-row {{ display:flex; align-items:center; gap:10px; min-height:24px; color:#475569; font-size:14px; }}
    .status-dot {{ width:10px; height:10px; border-radius:50%; background:#94a3b8; flex:none; }}
    .status-row.connected .status-dot {{ background:#16a34a; }}
    .status-row.connected {{ color:#166534; }}
    .wallet-address {{ font-family: ui-monospace, SFMono-Regular, SFMono-Regular, Consolas, monospace; font-size:13px; color:#0f172a; background:#e2e8f0; padding:4px 8px; border-radius:999px; }}
    .btn {{ display:inline-flex; align-items:center; justify-content:center; gap:8px; width:100%; border:none; border-radius:12px; padding:13px 16px; font-size:15px; font-weight:700; cursor:pointer; transition:transform .15s ease, opacity .15s ease, background .15s ease; }}
    .btn:hover {{ transform:translateY(-1px); }}
    .btn:disabled {{ cursor:not-allowed; opacity:.65; transform:none; }}
    .btn-primary {{ background:linear-gradient(135deg, #2563eb 0%, #1d4ed8 100%); color:#fff; }}
    .btn-secondary {{ background:linear-gradient(135deg, #0f766e 0%, #0f766e 100%); color:#fff; }}
    .btn-success {{ background:linear-gradient(135deg, #16a34a 0%, #15803d 100%); color:#fff; }}
    .btn-ghost {{ background:#e2e8f0; color:#0f172a; }}
    .messages {{ margin-top:18px; display:grid; gap:12px; }}
    .message {{ padding:12px 14px; border-radius:12px; font-size:14px; line-height:1.5; display:none; }}
    .message-error {{ background:#fef2f2; border:1px solid #fecaca; color:#b91c1c; }}
    .message-success {{ background:#f0fdf4; border:1px solid #bbf7d0; color:#166534; }}
    .disabled-copy {{ margin-top:auto; padding:12px 14px; border-radius:12px; background:#f8fafc; border:1px dashed #cbd5e1; color:#475569; font-size:13px; line-height:1.5; }}
    .response-section {{ margin-top:24px; display:none; }}
    .response-section h3 {{ margin:0 0 10px; font-size:16px; font-weight:700; color:#0f172a; }}
    .response-content {{ background:#0f172a; border-radius:16px; padding:18px; overflow:auto; max-height:360px; }}
    .response-content pre {{ margin:0; color:#e2e8f0; font-size:13px; line-height:1.6; white-space:pre-wrap; word-break:break-word; }}
    .spinner {{ display:inline-block; width:16px; height:16px; border:2px solid rgba(255,255,255,.35); border-top-color:#fff; border-radius:50%; animation:spin 1s linear infinite; }}
    .footer {{ margin-top:24px; text-align:center; color:#94a3b8; font-size:12px; }}
    .footer a {{ color:#64748b; text-decoration:none; }}
    .hidden {{ display:none !important; }}
    @keyframes spin {{ to {{ transform:rotate(360deg); }} }}
    @media (max-width: 640px) {{
      .card {{ padding:24px; border-radius:20px; }}
      .header {{ flex-direction:column; align-items:flex-start; }}
      .price-block {{ align-items:flex-start; flex-direction:column; }}
      .row {{ flex-direction:column; }}
      .row strong {{ text-align:left; }}
    }}
  </style>
</head>
<body>
  <div class="wrap">
    <div class="card">
      <div class="header">
        <div class="brand">
          <img src="/logo.png?v=20260918" alt="Apiosk" class="logo" />
          <span class="badge">{rail_label}</span>
        </div>
      </div>

      <div class="headline">
        <h1>Choose how to complete this request</h1>
        <p>{description} {rail_copy}</p>
      </div>

      {banner_html}

      <div class="price-block">
        <span class="price-label">Endpoint price</span>
        <span class="price-value">{amount_display} USDC</span>
      </div>

      <div class="meta">
        <div class="row"><span>Provider</span><strong>{slug}</strong></div>
        <div class="row"><span>Route</span><strong>{path}</strong></div>
        <div class="row"><span>Network</span><strong>{network}</strong></div>
        <div class="row"><span>Recipient</span><strong>{pay_to}</strong></div>
      </div>

      <div class="payment-grid">
        {credits_section_html}
        {wallet_section_html}
      </div>

      <div class="messages">
        <div class="message message-error" id="error-msg"></div>
        <div class="message message-success" id="success-msg"></div>
      </div>

      <div class="response-section" id="response-section">
        <h3>API response</h3>
        <div class="response-content">
          <pre id="response-content"></pre>
        </div>
      </div>

      <div class="footer">Powered by <a href="https://apiosk.com">Apiosk</a> · <a href="https://x402.org" target="_blank" rel="noreferrer">x402 Protocol</a></div>
    </div>
  </div>

  <script type="module">
    const PAGE_CONFIG = {page_config_json};
    const DASHBOARD_URL = PAGE_CONFIG.dashboardUrl;
    const DASHBOARD_ORIGIN = new URL(DASHBOARD_URL).origin;

    let provider = null;
    let signer = null;
    let walletAddress = null;
    let creditsSessionToken = null;
    let pendingBridgeState = null;

    const connectBtn = document.getElementById("connect-btn");
    const payBtn = document.getElementById("pay-btn");
    const useCreditsBtn = document.getElementById("use-credits-btn");
    const topUpBtn = document.getElementById("topup-btn");
    const walletStatus = document.getElementById("wallet-status");
    const creditsStatus = document.getElementById("credits-status");
    const errorMsg = document.getElementById("error-msg");
    const successMsg = document.getElementById("success-msg");
    const responseSection = document.getElementById("response-section");
    const responseContent = document.getElementById("response-content");

    function showError(message) {{
      errorMsg.textContent = message;
      errorMsg.style.display = "block";
      successMsg.style.display = "none";
    }}

    function showSuccess(message) {{
      successMsg.textContent = message;
      successMsg.style.display = "block";
      errorMsg.style.display = "none";
    }}

    function clearMessages() {{
      errorMsg.style.display = "none";
      successMsg.style.display = "none";
    }}

    function setButtonBusy(button, label) {{
      if (!button) return;
      button.disabled = true;
      button.dataset.originalLabel = button.dataset.originalLabel || button.textContent || "";
      button.innerHTML = `<span class="spinner"></span> ${{label}}`;
    }}

    function restoreButton(button) {{
      if (!button) return;
      button.innerHTML = button.dataset.originalLabel || button.textContent || "";
      button.disabled = button.dataset.locked === "true";
    }}

    function updateWalletStatus(connected, address) {{
      if (!walletStatus) return;
      if (connected && address) {{
        walletStatus.classList.add("connected");
        const shortAddr = `${{address.slice(0, 6)}}...${{address.slice(-4)}}`;
        walletStatus.innerHTML = `
          <span class="status-dot"></span>
          <span class="status-copy">Connected wallet</span>
          <span class="wallet-address">${{shortAddr}}</span>
        `;
        if (connectBtn) connectBtn.classList.add("hidden");
        if (payBtn) {{
          payBtn.classList.remove("hidden");
          payBtn.disabled = !PAGE_CONFIG.walletActionsEnabled;
        }}
      }} else {{
        walletStatus.classList.remove("connected");
        walletStatus.innerHTML = `
          <span class="status-dot"></span>
          <span class="status-copy">Wallet not connected</span>
        `;
        if (connectBtn) {{
          connectBtn.classList.remove("hidden");
          connectBtn.disabled = !PAGE_CONFIG.walletActionsEnabled;
        }}
        if (payBtn) {{
          payBtn.classList.add("hidden");
          payBtn.disabled = true;
        }}
      }}
    }}

    function updateCreditsStatus() {{
      if (!creditsStatus) return;
      if (creditsSessionToken) {{
        creditsStatus.classList.add("connected");
        creditsStatus.innerHTML = `
          <span class="status-dot"></span>
          <span class="status-copy">Dashboard session linked. Retry will bill your credits wallet.</span>
        `;
        if (useCreditsBtn) {{
          useCreditsBtn.textContent = "Retry with credits";
        }}
      }} else {{
        creditsStatus.classList.remove("connected");
        creditsStatus.innerHTML = `
          <span class="status-dot"></span>
          <span class="status-copy">Not linked to a dashboard session</span>
        `;
        if (useCreditsBtn) {{
          useCreditsBtn.textContent = "Use credits";
        }}
      }}
    }}

    function disableCreditsActions(message) {{
      if (useCreditsBtn) {{
        useCreditsBtn.dataset.locked = "true";
        useCreditsBtn.disabled = true;
      }}
      if (topUpBtn) {{
        topUpBtn.dataset.locked = "true";
        topUpBtn.disabled = true;
      }}
      if (message) showError(message);
    }}

    function generateNonce() {{
      const bytes = new Uint8Array(32);
      crypto.getRandomValues(bytes);
      return "0x" + Array.from(bytes).map((byte) => byte.toString(16).padStart(2, "0")).join("");
    }}

    function normalizeTopUpAmount() {{
      const parsed = Number.parseFloat(PAGE_CONFIG.topUpAmountEur || "");
      if (!Number.isFinite(parsed) || parsed <= 0) {{
        return "1.00";
      }}
      return Math.max(0.01, Math.round(parsed * 100) / 100).toFixed(2);
    }}

    function createBridgeUrl(mode) {{
      const bridgeUrl = new URL("/handle-payment/credits/bridge", DASHBOARD_URL);
      const bridgeState = crypto.randomUUID ? crypto.randomUUID() : String(Date.now());
      bridgeUrl.searchParams.set("mode", mode);
      bridgeUrl.searchParams.set("origin", window.location.origin);
      bridgeUrl.searchParams.set("state", bridgeState);
      if (mode === "topup") {{
        bridgeUrl.searchParams.set("amount_eur", normalizeTopUpAmount());
      }}
      pendingBridgeState = {{ mode, state: bridgeState }};
      return bridgeUrl.toString();
    }}

    function openBridgePopup(mode) {{
      clearMessages();
      const popup = window.open(
        createBridgeUrl(mode),
        `apiosk-${{mode}}-checkout`,
        "popup=yes,width=520,height=760,menubar=no,toolbar=no,status=no"
      );

      if (!popup) {{
        showError("Popup blocked. Allow popups for this site and try again.");
        return false;
      }}

      popup.focus();
      if (mode === "session") {{
        showSuccess("Finish the dashboard sign-in flow in the popup to use credits.");
      }} else {{
        showSuccess("Complete the top-up in the popup. This request will retry automatically when credits land.");
      }}
      return true;
    }}

    async function executeOriginalRequest(extraHeaders) {{
      const response = await fetch(window.location.href, {{
        method: "GET",
        headers: {{
          Accept: "application/json",
          ...extraHeaders,
        }},
      }});

      const contentType = response.headers.get("content-type") || "";
      const rawText = await response.text();
      let payload = null;
      if (contentType.includes("application/json")) {{
        try {{
          payload = JSON.parse(rawText);
        }} catch {{
          payload = null;
        }}
      }}
      return {{ response, contentType, rawText, payload }};
    }}

    function renderSuccessResponse(contentType, rawText, payload, successMessage) {{
      clearMessages();
      showSuccess(successMessage);
      responseSection.style.display = "block";
      responseContent.textContent = contentType.includes("application/json")
        ? JSON.stringify(payload, null, 2)
        : rawText;
    }}

    function payloadMessage(payload, fallback) {{
      if (!payload || typeof payload !== "object") return fallback;
      return payload.message || payload.error || fallback;
    }}

    async function attemptCreditsPayment() {{
      if (!PAGE_CONFIG.creditsActionsEnabled) {{
        disableCreditsActions("Credits are not available for this exact request.");
        return;
      }}

      if (!creditsSessionToken) {{
        openBridgePopup("session");
        return;
      }}

      clearMessages();
      setButtonBusy(useCreditsBtn, "Retrying...");

      try {{
        const {{ response, contentType, rawText, payload }} = await executeOriginalRequest({{
          "x-apiosk-user-jwt": creditsSessionToken,
        }});

        if (response.ok) {{
          renderSuccessResponse(contentType, rawText, payload, "Request paid with credits.");
          return;
        }}

        const errorCode = payload && typeof payload === "object" ? payload.error : null;
        const message = payloadMessage(payload, `Request failed (${{response.status}}).`);

        if (errorCode === "credits_auth_invalid" || errorCode === "credits_session_required") {{
          creditsSessionToken = null;
          updateCreditsStatus();
          showError("Your dashboard session expired. Sign in again to use credits.");
          return;
        }}

        if (errorCode === "credits_price_not_supported") {{
          disableCreditsActions(message);
          return;
        }}

        showError(message);
      }} catch (error) {{
        showError(error instanceof Error ? error.message : "Credits retry failed.");
      }} finally {{
        restoreButton(useCreditsBtn);
      }}
    }}

    async function connectWallet() {{
      clearMessages();
      setButtonBusy(connectBtn, "Connecting...");

      try {{
        if (!window.ethereum) {{
          throw new Error("No wallet detected. Install MetaMask or another Web3 wallet.");
        }}

        const {{ ethers }} = await import("https://esm.sh/ethers@6.11.1");
        const accounts = await window.ethereum.request({{ method: "eth_requestAccounts" }});
        if (!accounts || accounts.length === 0) {{
          throw new Error("No wallet account is available.");
        }}

        const chainIdHex = await window.ethereum.request({{ method: "eth_chainId" }});
        const currentChainId = Number.parseInt(chainIdHex, 16);

        if (currentChainId !== PAGE_CONFIG.requirement.chainId) {{
          try {{
            await window.ethereum.request({{
              method: "wallet_switchEthereumChain",
              params: [{{ chainId: "0x" + PAGE_CONFIG.requirement.chainId.toString(16) }}],
            }});
          }} catch (switchError) {{
            if (switchError && switchError.code === 4902) {{
              const CHAIN_PARAMS = {{
                8453: {{ chainName: "Base", nativeCurrency: {{ name: "ETH", symbol: "ETH", decimals: 18 }}, rpcUrls: ["https://mainnet.base.org"], blockExplorerUrls: ["https://basescan.org"] }},
                137: {{ chainName: "Polygon", nativeCurrency: {{ name: "POL", symbol: "POL", decimals: 18 }}, rpcUrls: ["https://polygon-rpc.com"], blockExplorerUrls: ["https://polygonscan.com"] }},
                42161: {{ chainName: "Arbitrum One", nativeCurrency: {{ name: "ETH", symbol: "ETH", decimals: 18 }}, rpcUrls: ["https://arb1.arbitrum.io/rpc"], blockExplorerUrls: ["https://arbiscan.io"] }},
                84532: {{ chainName: "Base Sepolia", nativeCurrency: {{ name: "ETH", symbol: "ETH", decimals: 18 }}, rpcUrls: ["https://sepolia.base.org"], blockExplorerUrls: ["https://sepolia.basescan.org"] }},
              }};
              const params = CHAIN_PARAMS[PAGE_CONFIG.requirement.chainId];
              if (!params) throw new Error("Unsupported chain: " + PAGE_CONFIG.requirement.chainId);
              await window.ethereum.request({{
                method: "wallet_addEthereumChain",
                params: [{{ chainId: "0x" + PAGE_CONFIG.requirement.chainId.toString(16), ...params }}],
              }});
            }} else {{
              throw switchError;
            }}
          }}
        }}

        provider = new ethers.BrowserProvider(window.ethereum);
        signer = await provider.getSigner();
        walletAddress = await signer.getAddress();
        updateWalletStatus(true, walletAddress);
        showSuccess("Wallet connected. Sign the proof to execute the request.");
      }} catch (error) {{
        walletAddress = null;
        signer = null;
        provider = null;
        updateWalletStatus(false, null);
        showError(error instanceof Error ? error.message : "Failed to connect wallet.");
      }} finally {{
        restoreButton(connectBtn);
      }}
    }}

    async function signAndPay() {{
      if (!signer || !walletAddress) {{
        showError("Connect a wallet before paying.");
        return;
      }}

      clearMessages();
      setButtonBusy(payBtn, "Signing...");

      try {{
        const domain = {{
          name: PAGE_CONFIG.requirement.extra.name,
          version: PAGE_CONFIG.requirement.extra.version,
          chainId: PAGE_CONFIG.requirement.chainId,
          verifyingContract: PAGE_CONFIG.requirement.asset,
        }};

        const types = {{
          TransferWithAuthorization: [
            {{ name: "from", type: "address" }},
            {{ name: "to", type: "address" }},
            {{ name: "value", type: "uint256" }},
            {{ name: "validAfter", type: "uint256" }},
            {{ name: "validBefore", type: "uint256" }},
            {{ name: "nonce", type: "bytes32" }},
          ],
        }};

        const validAfter = 0;
        const validBefore = Math.floor(Date.now() / 1000) + 300;
        const nonce = generateNonce();

        const value = {{
          from: walletAddress,
          to: PAGE_CONFIG.requirement.payTo,
          value: PAGE_CONFIG.requirement.maxAmountRequired,
          validAfter,
          validBefore,
          nonce,
        }};

        payBtn.innerHTML = '<span class="spinner"></span> Sign in wallet...';
        const signature = await signer.signTypedData(domain, types, value);
        const proof = {{
          x402Version: 1,
          scheme: PAGE_CONFIG.requirement.scheme,
          network: PAGE_CONFIG.requirement.network,
          payload: {{
            signature,
            authorization: {{
              from: walletAddress,
              to: PAGE_CONFIG.requirement.payTo,
              value: PAGE_CONFIG.requirement.maxAmountRequired,
              validAfter: validAfter.toString(),
              validBefore: validBefore.toString(),
              nonce,
            }},
          }},
        }};

        payBtn.innerHTML = '<span class="spinner"></span> Executing...';
        const {{ response, contentType, rawText, payload }} = await executeOriginalRequest({{
          "X-Payment": btoa(JSON.stringify(proof)),
        }});

        if (response.ok) {{
          renderSuccessResponse(contentType, rawText, payload, "Wallet payment complete.");
          payBtn.textContent = "Payment complete";
          payBtn.disabled = true;
          return;
        }}

        showError(payloadMessage(payload, `Payment failed (${{response.status}}).`));
      }} catch (error) {{
        showError(error instanceof Error ? error.message : "Wallet payment failed.");
      }} finally {{
        if (!payBtn.disabled) {{
          restoreButton(payBtn);
        }}
      }}
    }}

    window.addEventListener("message", async (event) => {{
      if (event.origin !== DASHBOARD_ORIGIN) return;
      const data = event.data;
      if (!data || data.source !== "apiosk-dashboard") return;
      if (!pendingBridgeState || data.state !== pendingBridgeState.state) return;

      if (data.type === "apiosk.credits.session") {{
        pendingBridgeState = null;
        if (typeof data.sessionToken !== "string" || !data.sessionToken.trim()) {{
          showError("The dashboard did not return a usable credits session.");
          return;
        }}
        creditsSessionToken = data.sessionToken.trim();
        updateCreditsStatus();
        showSuccess("Credits wallet linked. Retrying request.");
        await attemptCreditsPayment();
        return;
      }}

      if (data.type === "apiosk.credits.topup") {{
        pendingBridgeState = null;
        if (typeof data.sessionToken === "string" && data.sessionToken.trim()) {{
          creditsSessionToken = data.sessionToken.trim();
          updateCreditsStatus();
        }}

        if (data.status === "credited") {{
          showSuccess("Credits added. Retrying request.");
          await attemptCreditsPayment();
          return;
        }}

        if (data.status === "failed") {{
          showError(data.message || "Top-up was not completed.");
          return;
        }}

        showError(data.message || "Top-up could not be finalized.");
      }}
    }});

    if (connectBtn) {{
      connectBtn.dataset.originalLabel = connectBtn.textContent || "Connect wallet";
      connectBtn.addEventListener("click", connectWallet);
    }}
    if (payBtn) {{
      payBtn.dataset.originalLabel = payBtn.textContent || `Pay ${{PAGE_CONFIG.amountDisplay}} USDC`;
      payBtn.addEventListener("click", signAndPay);
    }}
    if (useCreditsBtn) {{
      useCreditsBtn.dataset.originalLabel = useCreditsBtn.textContent || "Use credits";
      useCreditsBtn.disabled = !PAGE_CONFIG.creditsActionsEnabled;
      useCreditsBtn.addEventListener("click", attemptCreditsPayment);
    }}
    if (topUpBtn) {{
      topUpBtn.dataset.originalLabel = topUpBtn.textContent || "Top up and retry";
      topUpBtn.disabled = !PAGE_CONFIG.creditsActionsEnabled;
      topUpBtn.addEventListener("click", () => {{
        if (PAGE_CONFIG.creditsActionsEnabled) {{
          openBridgePopup("topup");
        }}
      }});
    }}

    updateWalletStatus(false, null);
    updateCreditsStatus();

    if (window.ethereum && window.ethereum.selectedAddress && PAGE_CONFIG.walletActionsEnabled) {{
      connectWallet();
    }}
  </script>
</body>
</html>"##,
        rail_label = escape_html(rail_label),
        description = escape_html(&requirement.description),
        rail_copy = escape_html(rail_copy),
        banner_html = banner_html,
        amount_display = escape_html(&amount_display),
        slug = escape_html(api_slug),
        path = escape_html(&full_path),
        network = escape_html(&requirement.network),
        pay_to = escape_html(&requirement.pay_to),
        credits_section_html = credits_section_html,
        wallet_section_html = wallet_section_html,
        page_config_json = page_config_json,
    );

    Response::builder()
        .status(StatusCode::PAYMENT_REQUIRED)
        .header("Content-Type", "text/html; charset=utf-8")
        .header("Cache-Control", "no-store")
        .header("X-Apiosk-Gateway", "true")
        .body(Body::from(html))
        .unwrap_or_else(|_| Response::new(Body::from("Gateway response error")))
}

impl CheckoutBannerTone {
    fn css_class(&self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Danger => "danger",
        }
    }
}

/// Maximum request body size we forward to upstreams. 8 MiB covers normal
/// JSON payloads with plenty of headroom for file-attached endpoints; the
/// gateway is not a media-upload service.
pub(super) const MAX_PROXY_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Strict validator for `api_slug` URL segments. Mirrors the registration
/// check in api_management/helpers.rs so the proxy refuses anything the
/// registration form would have rejected; defends against header injection
/// or path-traversal by way of the slug. 1..=64 chars, lowercase letters,
/// digits and hyphens only, no leading/trailing hyphen.
pub(super) fn is_valid_api_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > 64 {
        return false;
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Maximum length of the `path` segment a buyer may send in `/:slug/<path>`.
/// Above this, normalize_path returns an empty path so the routing logic
/// rejects the request before it touches an upstream.
pub(super) const MAX_REQUEST_PATH_BYTES: usize = 8192;

/// Normalises a request path while stripping anything the upstream should
/// never see:
///   - control characters and NUL bytes
///   - `..` segments (path traversal)
///   - empty / overlong inputs
///
/// Falls back to `/` when the result would otherwise be empty so the
/// upstream call is well-formed; the upstream layer will then handle the
/// 404 if `/` is not a valid endpoint there.
pub(super) fn normalize_path(path: &str) -> String {
    if path.is_empty() {
        return "/".to_string();
    }
    if path.len() > MAX_REQUEST_PATH_BYTES {
        return "/".to_string();
    }
    if path.bytes().any(|b| b == 0 || (b < 0x20 && b != b'\t')) {
        return "/".to_string();
    }

    let normalized = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{}", path)
    };

    // Reject path traversal: a `..` segment between slashes, or the path
    // ending in `/..`. This is defense-in-depth — most upstreams normalise
    // their own paths, but we should never forward an attempt to reach
    // siblings of the configured base URL.
    let mut out = String::with_capacity(normalized.len());
    for segment in normalized.split('/') {
        if segment == ".." {
            return "/".to_string();
        }
        if !segment.is_empty() {
            out.push('/');
            out.push_str(segment);
        }
    }
    if out.is_empty() {
        return "/".to_string();
    }
    if normalized.ends_with('/') && !out.ends_with('/') {
        out.push('/');
    }
    out
}

pub(super) fn join_origin_url(base_url: &str, path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if path.is_empty() || path == "/" {
        return base.to_string();
    }

    if path.starts_with('/') {
        format!("{}{}", base, path)
    } else {
        format!("{}/{}", base, path)
    }
}
