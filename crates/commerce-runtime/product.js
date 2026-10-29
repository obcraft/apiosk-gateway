/*!
 * Apiosk Commerce — make a product page machine-buyable.
 *
 * Drop-in, dependency-free. A webshop pastes one <script> tag (carrying its
 * integration id) onto its product pages:
 *
 *   <script src="https://gateway.apiosk.com/product.js"
 *           data-integration-id="abc123" async></script>
 *
 * On load this reads the product from the page — name, price, currency — trying
 * JSON-LD (schema.org Product) first and falling back to common meta/DOM hints.
 * It POSTs {integration_id, url, name, price, currency} to the gateway's
 * /v1/commerce/resolve, which validates the domain + URL pattern for that
 * integration and turns the page into an x402 payment endpoint. The resolved
 * {product_id, charge_url} is exposed on window.ApioskProduct for anything that
 * wants to render a buy affordance.
 *
 * The integration id is safe to expose: it can only start a product resolve for
 * one integration; the payout wallet and webhook never leave the server.
 */
(function () {
  "use strict";

  if (window.ApioskProduct && window.ApioskProduct.__loaded) return;

  // ---- Resolve config from the loading <script> tag -----------------------
  var self =
    document.currentScript ||
    document.querySelector("script[data-integration-id]") ||
    (function () {
      var s = document.getElementsByTagName("script");
      return s[s.length - 1];
    })();

  // Gateway origin = where THIS script was served from (robust against custom
  // domains / a cdn.apiosk.com CNAME).
  var GATEWAY = originOf((self && self.src) || "") || "https://gateway.apiosk.com";

  function originOf(url) {
    try {
      return new URL(url, window.location.href).origin;
    } catch (e) {
      return "";
    }
  }

  // ---- Product extraction --------------------------------------------------

  // Walk a JSON-LD value looking for a schema.org Product with an offer. Handles
  // a bare object, an array, and @graph containers.
  function findProductNode(node) {
    if (!node || typeof node !== "object") return null;
    if (Array.isArray(node)) {
      for (var i = 0; i < node.length; i++) {
        var hit = findProductNode(node[i]);
        if (hit) return hit;
      }
      return null;
    }
    if (node["@graph"]) {
      var g = findProductNode(node["@graph"]);
      if (g) return g;
    }
    if (isProductType(node["@type"]) && node.offers) return node;
    return null;
  }

  function isProductType(t) {
    if (!t) return false;
    if (Array.isArray(t)) {
      for (var i = 0; i < t.length; i++) if (isProductType(t[i])) return true;
      return false;
    }
    return String(t).toLowerCase().indexOf("product") !== -1;
  }

  // The offer may be a single object or an array; take the first with a price.
  function firstOffer(offers) {
    if (!offers) return null;
    if (Array.isArray(offers)) {
      for (var i = 0; i < offers.length; i++) {
        if (offers[i] && offers[i].price != null) return offers[i];
      }
      return offers[0] || null;
    }
    return offers;
  }

  function fromJsonLd() {
    var scripts = document.querySelectorAll('script[type="application/ld+json"]');
    for (var i = 0; i < scripts.length; i++) {
      var parsed;
      try {
        parsed = JSON.parse(scripts[i].textContent || "");
      } catch (e) {
        continue;
      }
      var product = findProductNode(parsed);
      if (!product) continue;
      var offer = firstOffer(product.offers) || {};
      var price = offer.price != null ? offer.price : offer.lowPrice;
      if (price == null) continue;
      return {
        name: text(product.name),
        price: String(price),
        currency: text(offer.priceCurrency),
      };
    }
    return null;
  }

  function metaContent(selectors) {
    for (var i = 0; i < selectors.length; i++) {
      var el = document.querySelector(selectors[i]);
      if (el) {
        var v = el.getAttribute("content") || el.getAttribute("value") || el.textContent;
        if (v && String(v).trim()) return String(v).trim();
      }
    }
    return null;
  }

  // Fallback: Open Graph / microdata / common price hints. Best-effort only.
  function fromDom() {
    var name =
      metaContent([
        'meta[property="og:title"]',
        'meta[name="twitter:title"]',
        '[itemprop="name"]',
      ]) ||
      (document.querySelector("h1") && document.querySelector("h1").textContent) ||
      document.title;

    var price = metaContent([
      'meta[property="product:price:amount"]',
      'meta[property="og:price:amount"]',
      'meta[itemprop="price"]',
      '[itemprop="price"]',
    ]);

    var currency = metaContent([
      'meta[property="product:price:currency"]',
      'meta[property="og:price:currency"]',
      'meta[itemprop="priceCurrency"]',
      '[itemprop="priceCurrency"]',
    ]);

    if (!price) return null;
    return { name: text(name), price: normalizePrice(price), currency: text(currency) };
  }

  // Strip currency symbols/spaces and normalise decimal separators so "€ 19,95"
  // and "$19.95" both become "19.95". Leaves a plain "19.95" untouched.
  function normalizePrice(raw) {
    var s = String(raw).replace(/[^0-9.,]/g, "");
    // If both separators appear, the last one is the decimal separator.
    if (s.indexOf(",") !== -1 && s.indexOf(".") !== -1) {
      if (s.lastIndexOf(",") > s.lastIndexOf(".")) {
        s = s.replace(/\./g, "").replace(",", ".");
      } else {
        s = s.replace(/,/g, "");
      }
    } else if (s.indexOf(",") !== -1) {
      // Only commas: treat as decimal if it looks like cents, else thousands.
      s = /,\d{2}$/.test(s) ? s.replace(",", ".") : s.replace(/,/g, "");
    }
    return s;
  }

  function text(v) {
    return v == null ? "" : String(v).trim();
  }

  function extractProduct() {
    return fromJsonLd() || fromDom();
  }

  // ---- Resolve against the gateway ----------------------------------------

  function resolve(integrationId) {
    var product = extractProduct();
    if (!product || !product.price) {
      emit("error", { reason: "no_product_found" });
      return;
    }
    var payload = {
      integration_id: integrationId,
      url: window.location.pathname,
      name: product.name,
      price: product.price,
      currency: product.currency || "",
    };

    fetch(GATEWAY + "/v1/commerce/resolve", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    })
      .then(function (res) {
        return res.json().then(function (body) {
          return { ok: res.ok, body: body };
        });
      })
      .then(function (r) {
        if (!r.ok) {
          emit("error", r.body || { reason: "resolve_failed" });
          return;
        }
        state.product = r.body;
        emit("resolved", r.body);
      })
      .catch(function () {
        emit("error", { reason: "network_error" });
      });
  }

  // ---- Global API ----------------------------------------------------------
  var listeners = {};
  var state = { product: null };

  function emit(event, detail) {
    (listeners[event] || []).forEach(function (fn) {
      try {
        fn(detail);
      } catch (e) {}
    });
  }

  window.ApioskProduct = {
    __loaded: true,
    /** The resolved { product_id, charge_url } once /resolve returns, else null. */
    get: function () {
      return state.product;
    },
    /** Re-read the page and resolve again (e.g. after a client-side nav). */
    refresh: function (integrationId) {
      resolve(integrationId || (self && self.getAttribute("data-integration-id")));
    },
    /** What product.js reads off this page, without hitting the network. */
    scrape: extractProduct,
    /** Subscribe to "resolved" / "error" events. */
    on: function (event, fn) {
      (listeners[event] = listeners[event] || []).push(fn);
      return this;
    },
  };

  function autoRun() {
    if (!self) return;
    var integrationId = self.getAttribute("data-integration-id");
    if (!integrationId) return;
    resolve(integrationId);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", autoRun);
  } else {
    autoRun();
  }
})();
