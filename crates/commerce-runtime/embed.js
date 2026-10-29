/*!
 * Apiosk Connect — embeddable "Connect with Apiosk" button.
 *
 * Drop-in, dependency-free, Google-Analytics / Calendly style. A marketplace
 * pastes one <script> tag (carrying its PUBLISHABLE key, apk_pub_…) onto its own
 * site. This renders a "Connect with Apiosk" button; clicking opens the Apiosk
 * onboarding popup where a merchant registers + authorizes to join the
 * marketplace. On success the popup says "you can close this window" and the
 * button is replaced in-place by a session container showing the connection id,
 * the logged-in merchant session, and the marketplace they joined. The session is
 * persisted to localStorage so it survives reloads.
 *
 * The publishable key is safe to expose: it can only START a self-serve connect
 * for one marketplace. It is NOT the secret apk_live_ key.
 *
 *   <div id="apiosk-connect"></div>
 *   <script src="https://gateway.apiosk.com/embed.js"
 *           data-apiosk-key="apk_pub_xxx"
 *           data-apiosk-target="#apiosk-connect" async></script>
 */
(function () {
  "use strict";

  if (window.ApioskConnect && window.ApioskConnect.__loaded) return;

  // ---- Resolve config from the loading <script> tag -----------------------
  var self =
    document.currentScript ||
    document.querySelector("script[data-apiosk-key]") ||
    (function () {
      var s = document.getElementsByTagName("script");
      return s[s.length - 1];
    })();

  // Gateway origin = where THIS script was served from (robust against custom
  // domains); the onboarding popup lives on the portal.
  var GATEWAY = originOf((self && self.src) || "") || "https://gateway.apiosk.com";
  var PORTAL =
    (self && self.getAttribute("data-apiosk-portal")) || "https://dashboard.apiosk.com";
  PORTAL = PORTAL.replace(/\/+$/, "");

  var MESSAGE_SOURCE = "apiosk-connect";

  function originOf(url) {
    try {
      return new URL(url, window.location.href).origin;
    } catch (e) {
      return "";
    }
  }

  // ---- Per-marketplace persistence ----------------------------------------
  // Keyed by publishable key so several marketplaces can coexist on one page.
  function storeKey(pk) {
    return "apiosk_connect:" + String(pk).slice(-12);
  }
  // Stable per-browser ref so re-clicking resumes the same draft merchant.
  function browserRef() {
    var k = "apiosk_connect_ref";
    var v = safeGet(k);
    if (!v) {
      v =
        (window.crypto && window.crypto.randomUUID && window.crypto.randomUUID()) ||
        "ref-" + Math.random().toString(36).slice(2) + Date.now().toString(36);
      safeSet(k, v);
    }
    return v;
  }
  function safeGet(k) {
    try {
      return window.localStorage.getItem(k);
    } catch (e) {
      return null;
    }
  }
  function safeSet(k, v) {
    try {
      window.localStorage.setItem(k, v);
    } catch (e) {}
  }
  function safeDel(k) {
    try {
      window.localStorage.removeItem(k);
    } catch (e) {}
  }
  function loadSession(pk) {
    try {
      return JSON.parse(safeGet(storeKey(pk)) || "null");
    } catch (e) {
      return null;
    }
  }
  function saveSession(pk, session) {
    safeSet(storeKey(pk), JSON.stringify(session));
  }

  // ---- Styling (scoped, injected once) ------------------------------------
  function injectStyles() {
    if (document.getElementById("apiosk-connect-styles")) return;
    var css =
      ".apiosk-connect{font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;display:inline-block}" +
      ".apiosk-connect-btn{display:inline-flex;align-items:center;gap:8px;cursor:pointer;border:0;border-radius:10px;padding:11px 18px;font-size:14px;font-weight:600;line-height:1;color:#fff;background:#111827;transition:background .15s ease,transform .05s ease;box-shadow:0 1px 2px rgba(0,0,0,.08)}" +
      ".apiosk-connect-btn:hover{background:#1f2937}" +
      ".apiosk-connect-btn:active{transform:translateY(1px)}" +
      ".apiosk-connect-btn[disabled]{opacity:.6;cursor:default}" +
      ".apiosk-connect-mark{display:block;object-fit:contain;filter:invert(1)}" +
      "@media(prefers-color-scheme:dark){.apiosk-connect-mark{filter:none}}" +
      ".apiosk-connect-card{display:flex;flex-direction:column;gap:10px;min-width:260px;max-width:360px;border:1px solid #e5e7eb;border-radius:12px;padding:14px 16px;background:#fff;color:#111827;box-shadow:0 1px 3px rgba(0,0,0,.06)}" +
      ".apiosk-connect-card .ac-head{display:flex;align-items:center;gap:8px;font-weight:600;font-size:14px}" +
      ".apiosk-connect-card .ac-dot{width:8px;height:8px;border-radius:50%;background:#10b981;box-shadow:0 0 0 3px rgba(16,185,129,.18)}" +
      ".apiosk-connect-card .ac-row{display:flex;justify-content:space-between;gap:12px;font-size:12px}" +
      ".apiosk-connect-card .ac-row .ac-k{color:#6b7280}" +
      ".apiosk-connect-card .ac-row .ac-v{font-weight:500;text-align:right;word-break:break-all}" +
      ".apiosk-connect-card .ac-mono{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}" +
      ".apiosk-connect-card .ac-foot{display:flex;justify-content:space-between;align-items:center;margin-top:2px;padding-top:8px;border-top:1px solid #f3f4f6}" +
      ".apiosk-connect-card .ac-brand{font-size:11px;color:#9ca3af}" +
      ".apiosk-connect-card .ac-disconnect{background:none;border:0;color:#6b7280;font-size:12px;cursor:pointer;text-decoration:underline}" +
      "@media (prefers-color-scheme:dark){.apiosk-connect-card{background:#0b0f19;border-color:#1f2937;color:#f9fafb}.apiosk-connect-card .ac-foot{border-top-color:#1f2937}.apiosk-connect-btn{background:#fff;color:#111827}.apiosk-connect-btn:hover{background:#e5e7eb}}";
    var el = document.createElement("style");
    el.id = "apiosk-connect-styles";
    el.textContent = css;
    document.head.appendChild(el);
  }

  function logoSvg() {
    return '<img class="apiosk-connect-mark" src="' + escapeHtml(GATEWAY) + '/logo.png?v=20260918" width="20" height="20" alt="" aria-hidden="true">';
  }

  function escapeHtml(s) {
    return String(s == null ? "" : s).replace(/[&<>"']/g, function (c) {
      return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c];
    });
  }

  // ---- A single mount point (one button/card pair) ------------------------
  function Widget(el, pk, opts) {
    this.el = el;
    this.pk = pk;
    this.opts = opts || {};
    this.popup = null;
    this.poll = null;
    this.el.classList.add("apiosk-connect");
    var existing = loadSession(pk);
    if (existing && existing.status === "active") this.renderCard(existing);
    else this.renderButton();
  }

  Widget.prototype.renderButton = function () {
    var self = this;
    var label = this.opts.label || "Connect with Apiosk";
    this.el.innerHTML =
      '<button type="button" class="apiosk-connect-btn">' +
      logoSvg() +
      "<span>" +
      escapeHtml(label) +
      "</span></button>";
    this.el.querySelector("button").addEventListener("click", function () {
      self.open();
    });
  };

  Widget.prototype.renderCard = function (session) {
    var self = this;
    var shortId = String(session.merchantId || "").slice(0, 8) || "—";
    var who = session.email || (session.session ? maskToken(session.session) : "Active session");
    this.el.innerHTML =
      '<div class="apiosk-connect-card">' +
      '<div class="ac-head"><span class="ac-dot"></span><span>Connected with Apiosk</span></div>' +
      row("Marketplace", escapeHtml(session.marketplaceName || session.marketplaceId || "—")) +
      row("Connection", '<span class="ac-mono">' + escapeHtml(shortId) + "</span>") +
      row("Signed in as", escapeHtml(who)) +
      '<div class="ac-foot"><span class="ac-brand">Apiosk Connect</span>' +
      '<button type="button" class="ac-disconnect">Disconnect</button></div>' +
      "</div>";
    this.el.querySelector(".ac-disconnect").addEventListener("click", function () {
      self.disconnect();
    });
    function row(k, v) {
      return (
        '<div class="ac-row"><span class="ac-k">' +
        k +
        '</span><span class="ac-v">' +
        v +
        "</span></div>"
      );
    }
  };

  Widget.prototype.open = function () {
    var self = this;
    var url =
      PORTAL +
      "/connect/start?pk=" +
      encodeURIComponent(this.pk) +
      "&ref=" +
      encodeURIComponent(browserRef()) +
      "&origin=" +
      encodeURIComponent(window.location.origin);
    var w = 460;
    var h = 640;
    // Centering reads window.top, which throws if this page is framed
    // cross-origin — fall back to the current window's geometry.
    var features = "width=" + w + ",height=" + h + ",resizable,scrollbars";
    try {
      var base = window.top && window.top.outerWidth ? window.top : window;
      var y = base.outerHeight / 2 + (base.screenY || 0) - h / 2;
      var x = base.outerWidth / 2 + (base.screenX || 0) - w / 2;
      features += ",left=" + Math.max(0, x | 0) + ",top=" + Math.max(0, y | 0);
    } catch (e) {
      /* cross-origin top — let the browser place the popup */
    }
    this.popup = window.open(url, "apiosk-connect", features);
    if (!this.popup) {
      // Popup blocked — fall back to a full redirect-style new tab.
      window.open(url, "_blank");
      return;
    }
    // If the user closes the popup without finishing, restore the button.
    clearInterval(this.poll);
    this.poll = setInterval(function () {
      if (self.popup && self.popup.closed) {
        clearInterval(self.poll);
        self.popup = null;
      }
    }, 600);
  };

  Widget.prototype.onConnected = function (payload) {
    var session = {
      merchantId: payload.merchantId,
      marketplaceId: payload.marketplaceId,
      marketplaceName: payload.marketplaceName,
      email: payload.email,
      session: payload.session, // the merchant session token (shared access token)
      status: payload.status || "active",
      connectedAt: new Date().toISOString(),
    };
    saveSession(this.pk, session);
    clearInterval(this.poll);
    if (this.popup && !this.popup.closed) {
      try {
        this.popup.close();
      } catch (e) {}
    }
    this.renderCard(session);
    emit("connected", session);
  };

  Widget.prototype.disconnect = function () {
    safeDel(storeKey(this.pk));
    this.renderButton();
    emit("disconnected", { marketplaceKey: this.pk });
  };

  function maskToken(t) {
    t = String(t);
    return t.length <= 12 ? t : t.slice(0, 6) + "…" + t.slice(-4);
  }

  // ---- Registry + global API ----------------------------------------------
  var widgets = [];
  var listeners = {};

  function emit(event, detail) {
    (listeners[event] || []).forEach(function (fn) {
      try {
        fn(detail);
      } catch (e) {}
    });
  }

  function mount(target, pk, opts) {
    var el = typeof target === "string" ? document.querySelector(target) : target;
    if (!el || !pk) return null;
    var w = new Widget(el, pk, opts);
    widgets.push(w);
    return w;
  }

  // The popup posts the session back here on success.
  window.addEventListener("message", function (event) {
    if (event.origin !== PORTAL) return;
    var data = event.data || {};
    if (data.source !== MESSAGE_SOURCE) return;
    if (data.type === "connected" && data.payload) {
      widgets.forEach(function (w) {
        if (w.pk === data.payload.publishableKey || !data.payload.publishableKey) {
          w.onConnected(data.payload);
        }
      });
    }
  });

  function autoMount() {
    injectStyles();
    if (!self) return;
    var pk = self.getAttribute("data-apiosk-key");
    if (!pk) return;
    var targetSel = self.getAttribute("data-apiosk-target");
    var label = self.getAttribute("data-apiosk-label");
    var target;
    if (targetSel) {
      target = document.querySelector(targetSel);
    }
    if (!target) {
      // No explicit target: inject a container right after the script tag.
      target = document.createElement("div");
      target.id = "apiosk-connect";
      if (self.parentNode) self.parentNode.insertBefore(target, self.nextSibling);
    }
    mount(target, pk, { label: label || undefined });
  }

  window.ApioskConnect = {
    __loaded: true,
    /** Programmatic mount: ApioskConnect.init({ key, target, label }). */
    init: function (cfg) {
      injectStyles();
      cfg = cfg || {};
      return mount(cfg.target || "#apiosk-connect", cfg.key, { label: cfg.label });
    },
    /** Current stored session for a publishable key (or the first mounted one). */
    getSession: function (pk) {
      pk = pk || (widgets[0] && widgets[0].pk);
      return pk ? loadSession(pk) : null;
    },
    /** Clear a stored session and revert to the button. */
    disconnect: function (pk) {
      widgets.forEach(function (w) {
        if (!pk || w.pk === pk) w.disconnect();
      });
    },
    /** Subscribe to "connected" / "disconnected" events. */
    on: function (event, fn) {
      (listeners[event] = listeners[event] || []).push(fn);
      return this;
    },
  };

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", autoMount);
  } else {
    autoMount();
  }
})();
