/*!
 * Apiosk WebMCP — expose an Apiosk listing's endpoints as browser agent tools.
 *
 * Drop-in, dependency-free, same shape as embed.js. A provider pastes one tag
 * onto their own site and every endpoint of their Apiosk listing becomes a tool
 * that a WebMCP-capable browser agent can call:
 *
 *   <script src="https://gateway.apiosk.com/webmcp.js"
 *           data-apiosk-slug="marketstack" async></script>
 *
 * Tool definitions are derived from GET /v1/apis/{slug}, which already returns
 * every endpoint with its method, path, description, query params and price —
 * so this script invents nothing and stays correct as a listing changes.
 *
 * TWO ROUTING RULES, and getting them wrong misroutes money:
 *
 *   - Managed listing (hosted_externally = false): the tool calls
 *     gateway.apiosk.com, which issues the 402 and settles to the provider.
 *   - Federated listing (hosted_externally = true): the tool calls the
 *     provider's OWN host from external_resources[].resource and the agent pays
 *     them directly. Never route these through the gateway — Apiosk indexes
 *     them, it does not proxy or settle them. Two thirds of the catalog is
 *     federated, so this is the common path, not an edge case.
 *
 * PAYMENT: this script never sees a credential. On a 402 it opens the Apiosk
 * payment popup (/_apiosk/pay, on gateway.apiosk.com) and asks THAT window to
 * make the call. The popup holds the buyer's session and connect token on the
 * Apiosk origin, pays from the buyer's credits, and posts back only the response
 * body. A provider hosting this script therefore cannot read, and cannot spend,
 * anything belonging to the visitor.
 *
 * The popup is opt-in per listing (data-apiosk-pay="popup"). Without it — or if
 * the popup is blocked or dismissed — the 402 requirements are returned to the
 * agent as before, and window.ApioskWebMCP.onPaymentRequired still fires.
 *
 * SPEC CHURN: the API moved from navigator.modelContext to document.modelContext
 * and Chrome deprecated the navigator form in 150 while the origin trial still
 * ships it. Both are probed, newest first; absent either, the script exits
 * quietly rather than throwing on a normal visitor's browser.
 */
(function () {
  "use strict";

  if (window.ApioskWebMCP && window.ApioskWebMCP.__loaded) return;

  var DEFAULT_BASE = "https://gateway.apiosk.com";

  // ---- Resolve config from the loading <script> tag -----------------------
  var self =
    document.currentScript ||
    document.querySelector("script[data-apiosk-slug]") ||
    null;

  var slug = self && self.getAttribute("data-apiosk-slug");
  // Catalog mode: used by the gateway's own HTML pages rather than a provider's
  // site. Instead of one listing's endpoints it registers search/lookup tools
  // over the whole catalog, so an agent that lands on gateway.apiosk.com can
  // browse it as tools instead of scraping the page. All three are free — this
  // is discovery, which Apiosk does not charge for — so none of the payment
  // machinery below is reachable in this mode.
  var catalogMode = !!(self && self.getAttribute("data-apiosk-catalog"));
  var base = (self && self.getAttribute("data-apiosk-base")) || DEFAULT_BASE;
  base = String(base).replace(/\/+$/, "");
  // Opt-in: a provider who does not want a payment window on their page simply
  // omits the attribute and keeps the report-only behaviour.
  var payMode = (self && self.getAttribute("data-apiosk-pay")) || "";
  // Opt-out: data-apiosk-telemetry="off" silences the one beacon below.
  var telemetry =
    String((self && self.getAttribute("data-apiosk-telemetry")) || "").toLowerCase() !==
    "off";

  var api = {
    __loaded: true,
    slug: slug,
    base: base,
    tools: [],
    /** Set by the host page to receive x402 requirements on a 402. */
    onPaymentRequired: null,
  };
  window.ApioskWebMCP = api;

  if (!slug && !catalogMode) {
    warn("no data-apiosk-slug on the script tag; nothing to register");
    return;
  }

  function warn(message) {
    if (window.console && console.warn) console.warn("[apiosk-webmcp] " + message);
  }

  // ---- Locate the WebMCP surface -----------------------------------------
  // document.modelContext is where the spec landed; navigator.modelContext is
  // the older origin-trial location. Neither present means the visitor is on a
  // browser without WebMCP, which is the overwhelming majority — leave quietly.
  function modelContext() {
    if (typeof document !== "undefined" && document.modelContext) return document.modelContext;
    if (typeof navigator !== "undefined" && navigator.modelContext) return navigator.modelContext;
    return null;
  }

  // ---- Injection report ---------------------------------------------------
  // One beacon per run, sent only from a browser that actually exposes a
  // modelContext — i.e. only when there was a WebMCP host to inject into. It
  // carries the listing, the outcome and how many tools were registered, and
  // nothing about the visitor: the gateway takes the site from the Origin
  // header the browser sets, which page script cannot forge.
  //
  // This is the only way the volume is knowable. /webmcp.js is a static,
  // hour-cached asset whose request carries no slug, and everything that
  // decides whether tools were registered happens here, afterwards.
  //
  // sendBeacon with a text/plain blob so it is a CORS-simple request (no
  // preflight) and survives the page unloading mid-run; fetch with keepalive is
  // the fallback. Failure is ignored in full — a counter must never be visible
  // to the page it is counting.
  function report(outcome, toolCount) {
    if (!telemetry) return;
    try {
      var payload = JSON.stringify({
        slug: slug || "",
        mode: catalogMode ? "catalog" : "listing",
        outcome: outcome,
        tools: toolCount || 0,
      });
      var url = base + "/_apiosk/webmcp";
      if (navigator.sendBeacon) {
        navigator.sendBeacon(url, new Blob([payload], { type: "text/plain" }));
        return;
      }
      fetch(url, {
        method: "POST",
        body: payload,
        keepalive: true,
        headers: { "Content-Type": "text/plain" },
      }).catch(function () {});
    } catch (e) {
      /* never surface a telemetry failure to the host page */
    }
  }

  // ---- Tool-name derivation ----------------------------------------------
  // Agents address tools by name, so it must be stable and readable: the slug
  // plus the path, with everything that is not a word character folded to "_".
  function toolName(slugValue, method, path) {
    var raw = slugValue + "_" + String(path || "").replace(/\{|\}/g, "") + "_" + method;
    return raw
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "_")
      .replace(/^_+|_+$/g, "")
      .slice(0, 64);
  }

  // ---- Schema derivation --------------------------------------------------
  // query_params carries one of TWO shapes and both are legitimate, because
  // both are what providers and importers actually wrote:
  //
  //   JSON Schema  { type, required: [names], properties: { name: spec } }
  //   flat         { name: spec }, each spec carrying its own `required: true`
  //
  // The gateway's own docs builder reads both, for exactly this reason — see
  // `parameter_map` in src/endpoint_docs.rs. Only the flat form was read here,
  // and the schema form is what every OpenAPI import produces, so those
  // endpoints registered three parameters literally called `properties`,
  // `required` and `type` while the real ones never reached the agent. The call
  // still worked, which is what made it hard to notice: it just could not be
  // given any arguments.
  //
  // A flat map is only recognised when every value is itself an object, so a
  // schema with no parameters (or a value shaped like something else entirely)
  // is never mistaken for one.
  function parameterMap(params) {
    if (!params || typeof params !== "object" || Array.isArray(params)) return null;

    var properties = params.properties;
    if (properties && typeof properties === "object" && !Array.isArray(properties)) {
      return {
        properties: properties,
        required: Array.isArray(params.required) ? params.required : [],
      };
    }

    var names = Object.keys(params);
    if (!names.length) return null;
    for (var i = 0; i < names.length; i++) {
      var spec = params[names[i]];
      if (!spec || typeof spec !== "object" || Array.isArray(spec)) return null;
    }
    return {
      properties: params,
      required: names.filter(function (name) {
        return params[name].required === true;
      }),
    };
  }

  // One JSON Schema property from one stored parameter spec, in whichever of
  // the two shapes it arrived.
  function property(spec) {
    var out = { type: spec.type || "string" };
    if (spec.example !== undefined) out.examples = [spec.example];
    if (spec.description) out.description = spec.description;
    if (spec.enum) out.enum = spec.enum;
    if (spec.format) out.format = spec.format;
    return out;
  }

  // request_body, when present, is already schema-shaped and passes through
  // untouched.
  //
  // Header parameters are folded into the SAME flat argument object as query
  // params, with `headerKeys` recording which ones they were so callResource
  // can put them back where they belong. The agent sees one argument list and
  // should not have to care; the wire very much does. Endpoints documenting a
  // header the BUYER supplies were uncallable before this — the parameter was
  // never offered, so a required header could never be sent.
  function inputSchema(endpoint) {
    var properties = {};
    var required = [];
    var headerKeys = [];

    var params = parameterMap(endpoint.query_params);
    if (params) {
      Object.keys(params.properties).forEach(function (key) {
        var spec = params.properties[key] || {};
        properties[key] = property(spec);
        if (spec.required === true || params.required.indexOf(key) !== -1) {
          required.push(key);
        }
      });
    }

    var headers = parameterMap(endpoint.headers);
    if (headers) {
      Object.keys(headers.properties).forEach(function (key) {
        var spec = headers.properties[key] || {};
        // The gateway injects the PROVIDER's credential upstream. Offering the
        // agent an Authorization or API-key header would invite it to overwrite
        // that, or to go looking for a secret it is the whole point of Apiosk
        // that it never holds.
        if (/^(authorization|x-api-key|api[-_]?key)$/i.test(key)) return;
        // A header and a query param can share a name; the flat list cannot.
        // Query wins, because that is what shipped.
        if (Object.prototype.hasOwnProperty.call(properties, key)) return;
        var out = property(spec);
        out.description =
          (out.description ? out.description + " " : "") +
          "(Sent as the " + key + " request header.)";
        properties[key] = out;
        headerKeys.push(key);
        if (spec.required === true || headers.required.indexOf(key) !== -1) {
          required.push(key);
        }
      });
    }

    if (endpoint.request_body && typeof endpoint.request_body === "object") {
      properties.body = endpoint.request_body;
      required.push("body");
    }

    var schema = { type: "object", properties: properties };
    if (required.length) schema.required = required;
    return { schema: schema, headerKeys: headerKeys };
  }

  // ---- What the agent is told about an endpoint ---------------------------
  // A tool description is the only documentation most agents ever read: they
  // pick a tool from the list and call it, and nothing prompts them to go
  // looking for a manual. So the response shape and a worked example belong
  // here, inline — an agent that cannot see what comes back either avoids the
  // tool or calls it blind and mis-reads the result.
  //
  // Truncated, because the opposite failure is just as real: a listing with 30
  // endpoints, each carrying a page of markdown, is a tool list that crowds out
  // the conversation it was meant to serve. The docs tool below holds the
  // untruncated version for the agent that wants it.
  var INLINE_LIMIT = 320;

  function compact(value, limit) {
    var text = typeof value === "string" ? value : JSON.stringify(value);
    if (!text) return "";
    text = text.replace(/\s+/g, " ").trim();
    return text.length > limit ? text.slice(0, limit - 1) + "…" : text;
  }

  function describe(listing, endpoint, resourceUrl, federated) {
    var parts = [];
    parts.push(endpoint.description || listing.description || listing.slug);
    if (endpoint.price_usd) {
      parts.push("Costs " + endpoint.price_usd + " USDC per call, paid via x402.");
    }
    parts.push(
      federated
        ? "Federated listing: pay the provider directly at " + resourceUrl + "."
        : "Settled through Apiosk."
    );

    var body = endpoint.response_body;
    var returns =
      (body && body.description) ||
      (body && body.properties
        ? "an object with " + Object.keys(body.properties).join(", ")
        : "");
    if (returns) parts.push("Returns: " + compact(returns, INLINE_LIMIT));

    if (endpoint.request_example !== undefined && endpoint.request_example !== null) {
      parts.push("Example arguments: " + compact(endpoint.request_example, INLINE_LIMIT));
    }
    if (endpoint.response_example !== undefined && endpoint.response_example !== null) {
      parts.push("Example response: " + compact(endpoint.response_example, INLINE_LIMIT));
    }

    var docs = endpoint.external_docs_url || listing.docs_url;
    if (docs) parts.push("Docs: " + docs);
    if (!federated && listing.slug) {
      parts.push(
        "Call " +
          docsToolName(listing.slug) +
          " for the full schemas, examples and notes."
      );
    }
    return parts.join(" ");
  }

  // ---- Paying, without ever holding a credential --------------------------
  // Hands the call to the Apiosk popup and waits for the body to come back. The
  // popup runs on the gateway origin, so the buyer's session and connect token
  // stay there; this page only ever learns the response.
  //
  // Browsers block window.open outside a user gesture, and a tool call is not
  // one. A blocked or dismissed popup is therefore an expected outcome, not an
  // error: resolve null and let the caller fall back to reporting the 402.
  function payViaPopup(request) {
    return new Promise(function (resolve) {
      var origin;
      try {
        origin = new URL(base).origin;
      } catch (e) {
        return resolve(null);
      }

      var popup = window.open(
        base + "/_apiosk/pay?origin=" + encodeURIComponent(location.origin),
        "apiosk-pay",
        "width=440,height=560"
      );
      if (!popup) {
        warn("payment popup blocked; returning the 402 requirements instead");
        return resolve(null);
      }

      var done = false;
      function finish(value) {
        if (done) return;
        done = true;
        window.removeEventListener("message", onMessage);
        clearInterval(closedTimer);
        resolve(value);
      }

      function onMessage(event) {
        if (event.origin !== origin) return;
        var data = event.data;
        if (!data || data.source !== "apiosk-pay") return;

        if (data.type === "ready") {
          popup.postMessage({ source: "apiosk-webmcp", type: "request", request: request }, origin);
          return;
        }
        if (data.type === "result") finish(data);
        if (data.type === "cancelled" || data.type === "error") finish(null);
      }

      window.addEventListener("message", onMessage);

      // The user can close the window instead of answering; that is a decline.
      var closedTimer = setInterval(function () {
        if (popup.closed) finish(null);
      }, 500);
    });
  }

  // ---- Calling --------------------------------------------------------
  // A 402 is not an error here — it is the protocol working. Parse it, surface
  // the requirements, and let whatever holds a wallet decide what to do.
  function callResource(url, method, args, headerKeys) {
    var options = { method: method, headers: {} };
    var target = url;
    var headerNames = headerKeys || [];

    var body = args && args.body;
    var query = {};
    Object.keys(args || {}).forEach(function (key) {
      if (key === "body") return;
      // Documented as a header on the listing, so it goes back on the wire as
      // one — see inputSchema, which flattened it into the same argument bag.
      if (headerNames.indexOf(key) !== -1) {
        if (args[key] !== undefined && args[key] !== null) {
          options.headers[key] = String(args[key]);
        }
        return;
      }
      query[key] = args[key];
    });

    var queryKeys = Object.keys(query);
    if (queryKeys.length) {
      var search = queryKeys
        .map(function (k) {
          return encodeURIComponent(k) + "=" + encodeURIComponent(query[k]);
        })
        .join("&");
      target += (target.indexOf("?") === -1 ? "?" : "&") + search;
    }

    if (body !== undefined && method !== "GET" && method !== "HEAD") {
      options.headers["Content-Type"] = "application/json";
      options.body = typeof body === "string" ? body : JSON.stringify(body);
    }

    return fetch(target, options).then(function (response) {
      return response.text().then(function (text) {
        var parsed;
        try {
          parsed = JSON.parse(text);
        } catch (e) {
          parsed = null;
        }

        if (response.status === 402) {
          var requirements = {
            resource: target,
            status: 402,
            x402Version: parsed && parsed.x402Version,
            accepts: (parsed && parsed.accepts) || [],
            paymentRequiredHeader: response.headers.get("payment-required") || null,
          };
          if (typeof api.onPaymentRequired === "function") {
            try {
              api.onPaymentRequired(requirements);
            } catch (e) {
              warn("onPaymentRequired threw: " + e);
            }
          }

          var reportRequirements = {
            content: [
              {
                type: "text",
                text:
                  "Payment required before this call returns data. x402 requirements:\n" +
                  JSON.stringify(requirements, null, 2),
              },
            ],
            isError: false,
          };

          if (payMode !== "popup") return reportRequirements;

          var price =
            requirements.accepts[0] && requirements.accepts[0].maxAmountRequired;
          return payViaPopup({
            url: target,
            method: method,
            body: body,
            // The popup remakes the call from scratch on the gateway origin, so
            // any header parameter the agent supplied has to travel with it or
            // the paid attempt goes out different from the unpaid one.
            headers: options.headers,
            price: price ? String(Number(price) / 1e6) : null,
          }).then(function (paid) {
            // Declined, blocked or closed: the agent still gets the terms.
            if (!paid) return reportRequirements;
            return {
              content: [{ type: "text", text: paid.body }],
              isError: paid.ok === false,
            };
          });
        }

        return {
          content: [{ type: "text", text: text }],
          isError: !response.ok,
        };
      });
    });
  }

  // ---- Build the tool list from a listing ---------------------------------
  function buildTools(listing) {
    var federated = listing.hosted_externally === true;
    var tools = [];

    if (federated) {
      // Federated: one tool per indexed external resource, pointing at the
      // provider's own host. The gateway never sees these calls.
      (listing.external_resources || []).forEach(function (resource, index) {
        if (!resource || !resource.resource) return;
        tools.push({
          name: toolName(listing.slug, "get", "resource_" + index),
          description: describe(listing, {}, resource.resource, true),
          inputSchema: { type: "object", properties: {} },
          url: resource.resource,
          method: "GET",
        });
      });
      return tools;
    }

    (listing.endpoints || []).forEach(function (endpoint) {
      if (!endpoint || !endpoint.full_url) return;
      var method = (endpoint.method || "GET").toUpperCase();
      var input = inputSchema(endpoint);
      tools.push({
        name: toolName(listing.slug, method, endpoint.path),
        description: describe(listing, endpoint, endpoint.full_url, false),
        inputSchema: input.schema,
        headerKeys: input.headerKeys,
        url: endpoint.full_url,
        method: method,
      });
    });

    if (tools.length) tools.push(docsTool(listing, tools));
    return tools;
  }

  // ---- The docs tool ------------------------------------------------------
  // Everything the listing knows about an endpoint, untruncated: both schemas,
  // both examples, the provider's markdown, auth notes, the docs URL. Free,
  // local (the listing is already in memory) and instant — so an agent can
  // afford to read before it spends, which is the point. A tool description has
  // to stay short; documentation does not, and the two needs are irreconcilable
  // in one string.
  function docsToolName(slug) {
    return toolName(slug, "docs", "");
  }

  function endpointDoc(listing, endpoint) {
    return {
      tool: toolName(listing.slug, (endpoint.method || "GET").toUpperCase(), endpoint.path),
      method: (endpoint.method || "GET").toUpperCase(),
      path: endpoint.path,
      url: endpoint.full_url,
      description: endpoint.description,
      price_usd: endpoint.price_usd,
      payment_required: endpoint.payment_required,
      query_params: endpoint.query_params,
      headers: endpoint.headers,
      request_schema: endpoint.request_body,
      response_schema: endpoint.response_body,
      request_example: endpoint.request_example,
      response_example: endpoint.response_example,
      documentation: endpoint.documentation_markdown,
      authentication_notes: endpoint.authentication_notes,
      docs_url: endpoint.external_docs_url || listing.docs_url,
      openapi_url: endpoint.openapi_url,
    };
  }

  function docsTool(listing, endpointTools) {
    var names = endpointTools.map(function (tool) {
      return tool.name;
    });

    return {
      name: docsToolName(listing.slug),
      description:
        "Documentation for " +
        (listing.name || listing.slug) +
        ": parameters, request and response schemas, worked examples and the " +
        "provider's own notes, for one endpoint or all of them. Free — reading " +
        "the docs costs nothing, only calling an endpoint does. Tools: " +
        names.join(", ") +
        ".",
      inputSchema: {
        type: "object",
        properties: {
          tool: {
            type: "string",
            description:
              "Which endpoint to document, by tool name or by path (e.g. /get). " +
              "Omit for the whole listing.",
            examples: [names[0]],
          },
        },
      },
      run: function (args) {
        var wanted = args && typeof args.tool === "string" ? args.tool.trim() : "";
        var endpoints = listing.endpoints || [];

        var docs = endpoints.map(function (endpoint) {
          return endpointDoc(listing, endpoint);
        });

        if (wanted) {
          var lower = wanted.toLowerCase();
          var hit = docs.filter(function (doc) {
            return (
              doc.tool === lower ||
              doc.tool.indexOf(lower) !== -1 ||
              String(doc.path).toLowerCase() === lower ||
              (doc.method + " " + doc.path).toLowerCase() === lower
            );
          });
          // An unmatched name is a typo, not an empty listing: say so, and say
          // what the real names are, rather than returning the whole manual.
          if (!hit.length) {
            return Promise.resolve({
              content: [
                {
                  type: "text",
                  text:
                    'No endpoint matching "' + wanted + '". Available: ' + names.join(", "),
                },
              ],
              isError: true,
            });
          }
          docs = hit;
        }

        return Promise.resolve({
          content: [
            {
              type: "text",
              text: JSON.stringify(
                {
                  listing: {
                    slug: listing.slug,
                    name: listing.name,
                    description: listing.long_description || listing.description,
                    docs_url: listing.docs_url,
                    example_usage: listing.example_usage,
                    authentication: listing.authentication,
                    price_usd: listing.price_usd,
                    provider: listing.provider,
                  },
                  endpoints: docs,
                },
                null,
                2
              ),
            },
          ],
          isError: false,
        });
      },
    };
  }

  // ---- Catalog tools (gateway's own pages) --------------------------------
  // Free JSON reads over the catalog. An agent that lands here can search and
  // inspect listings without scraping the HTML, and every result already tells
  // it the price and how to call the thing for real.
  function catalogTools() {
    function json(path) {
      return fetch(base + path, { headers: { Accept: "application/json" } })
        .then(function (response) {
          return response.text().then(function (text) {
            return { content: [{ type: "text", text: text }], isError: !response.ok };
          });
        })
        .catch(function (error) {
          return { content: [{ type: "text", text: String(error) }], isError: true };
        });
    }

    function query(args) {
      var parts = [];
      Object.keys(args || {}).forEach(function (key) {
        var value = args[key];
        if (value === undefined || value === null || value === "") return;
        parts.push(encodeURIComponent(key) + "=" + encodeURIComponent(value));
      });
      return parts.length ? "?" + parts.join("&") : "";
    }

    return [
      {
        name: "apiosk_search_apis",
        description:
          "Search the Apiosk catalog of pay-per-call APIs. Free. Each result carries its " +
          "endpoints, schemas, pricing and the URL to call it. Filter by text, category, " +
          "listing type or price ceiling.",
        inputSchema: {
          type: "object",
          properties: {
            search: { type: "string", description: "Free-text query, e.g. weather or stock prices" },
            category: { type: "string", description: "e.g. data, compute, finance, media, tools" },
            listing_type: { type: "string", description: "api or federated" },
            max_price: { type: "number", description: "Highest price per call in USDC" },
            limit: { type: "integer", description: "How many to return, default 20" },
          },
        },
        run: function (args) {
          var a = args || {};
          return json("/v1/apis" + query({
            search: a.search,
            category: a.category,
            listing_type: a.listing_type,
            max_price: a.max_price,
            limit: a.limit || 20,
          }));
        },
      },
      {
        name: "apiosk_get_api",
        description:
          "Fetch one Apiosk listing in full by slug: every endpoint with its method, path, " +
          "input schema, examples and price, plus whether it is settled through Apiosk or " +
          "hosted externally. Free.",
        inputSchema: {
          type: "object",
          properties: { slug: { type: "string", description: "The listing slug, e.g. marketstack" } },
          required: ["slug"],
        },
        run: function (args) {
          return json("/v1/apis/" + encodeURIComponent((args && args.slug) || ""));
        },
      },
      {
        name: "apiosk_list_providers",
        description: "List the providers publishing on Apiosk, with how many listings each has. Free.",
        inputSchema: {
          type: "object",
          properties: { search: { type: "string", description: "Optional name filter" } },
        },
        run: function (args) {
          return json("/v1/providers" + query({ search: args && args.search }));
        },
      },
    ];
  }

  // ---- Registration -------------------------------------------------------
  function register(context, tools) {
    var descriptors = tools.map(function (tool) {
      return {
        name: tool.name,
        description: tool.description,
        inputSchema: tool.inputSchema,
        // Catalog tools carry their own `run`; listing tools go through the
        // paid path in `callResource`.
        execute: function (args) {
          return tool.run
            ? tool.run(args || {})
            : callResource(tool.url, tool.method, args || {}, tool.headerKeys);
        },
      };
    });

    // registerTool is the per-tool form; provideContext replaces the whole set.
    // Both have appeared in the spec's short life, so use whichever this
    // browser actually implements.
    if (typeof context.registerTool === "function") {
      descriptors.forEach(function (descriptor) {
        try {
          context.registerTool(descriptor);
        } catch (e) {
          warn("registerTool failed for " + descriptor.name + ": " + e);
        }
      });
      return true;
    }
    if (typeof context.provideContext === "function") {
      try {
        context.provideContext({ tools: descriptors });
        return true;
      } catch (e) {
        warn("provideContext failed: " + e);
        return false;
      }
    }
    warn("modelContext exposes neither registerTool nor provideContext");
    return false;
  }

  // ---- Boot ---------------------------------------------------------------
  var context = modelContext();
  if (!context) return; // Not a WebMCP browser. Normal, and not worth a warning.

  if (catalogMode) {
    // No listing to fetch — the catalog tools are static definitions that read
    // the API when the agent actually calls them.
    var tools = catalogTools();
    if (register(context, tools)) {
      api.tools = tools;
      report("registered", tools.length);
    } else {
      report("failed", 0);
    }
    return;
  }

  fetch(base + "/v1/apis/" + encodeURIComponent(slug), {
    headers: { Accept: "application/json" },
  })
    .then(function (response) {
      if (!response.ok) throw new Error("listing fetch failed: HTTP " + response.status);
      return response.json();
    })
    .then(function (payload) {
      var listing = payload && (payload.api || payload);
      if (!listing || !listing.slug) throw new Error("listing payload has no slug");

      var tools = buildTools(listing);
      if (!tools.length) {
        warn("listing " + slug + " exposes no callable endpoints");
        report("empty", 0);
        return;
      }
      if (register(context, tools)) {
        api.tools = tools;
        api.listing = listing;
        report("registered", tools.length);
      } else {
        report("failed", 0);
      }
    })
    .catch(function (error) {
      warn(String(error));
      report("failed", 0);
    });
})();
