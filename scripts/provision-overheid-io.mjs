#!/usr/bin/env node

import fs from "node:fs/promises";

const SOURCE_SLUG = "overheid-io";

function parseEnv(text) {
  const values = new Map();
  for (const line of text.split(/\r?\n/)) {
    const match = line.match(/^\s*([^#=]+?)\s*=\s*(.*?)\s*$/);
    if (!match) continue;
    let value = match[2];
    if (
      value.length >= 2 &&
      ((value.startsWith('"') && value.endsWith('"')) ||
        (value.startsWith("'") && value.endsWith("'")))
    ) {
      value = value.slice(1, -1);
    }
    values.set(match[1].trim(), value);
  }
  return values;
}

function required(values, name) {
  const value = values.get(name)?.trim();
  if (!value) throw new Error(`${name} is missing from gateway/.env`);
  return value;
}

async function responseJson(response, label) {
  const text = await response.text();
  let value = null;
  try {
    value = text ? JSON.parse(text) : null;
  } catch {
    throw new Error(`${label} returned non-JSON (HTTP ${response.status})`);
  }
  if (!response.ok) {
    const message =
      value && typeof value === "object" && typeof value.message === "string"
        ? `: ${value.message}`
        : "";
    throw new Error(`${label} failed (HTTP ${response.status})${message}`);
  }
  return value;
}

const env = parseEnv(await fs.readFile(new URL("../.env", import.meta.url), "utf8"));
const supabaseUrl = required(env, "SUPABASE_URL").replace(/\/$/, "");
const serviceKey = required(env, "SUPABASE_SERVICE_ROLE_KEY");
// The second spelling preserves the key already present before gateway-v2 was
// renamed. New environments should use the conventional uppercase name.
const upstreamKey =
  env.get("OVERHEID_IO_API_KEY")?.trim() || env.get("overheid.io")?.trim();
if (!upstreamKey || upstreamKey.length < 20) {
  throw new Error(
    "OVERHEID_IO_API_KEY (or the legacy overheid.io entry) is missing or implausibly short",
  );
}

const probes = [
  ["OpenKvK list", "/v3/openkvk?size=1&query=Mollie"],
  [
    "OpenKvK detail",
    "/v3/openkvk/hoofdvestiging-gastouder-mollie-66076986-34733841",
  ],
  ["OpenKvK suggest", "/v3/suggest/openkvk/Mollie?size=1"],
  ["OpenKvK radius", "/v3/geo/openkvk/radius/52.3676/4.9041/1000?size=1"],
  [
    "OpenKvK around",
    "/v3/geo/openkvk/around/hoofdvestiging-gastouder-mollie-66076986-34733841/1000?size=1",
  ],
  ["OpenKvK box", "/v3/geo/openkvk/box/52.38/4.88/52.35/4.93?size=1"],
  ["Vehicle list", "/voertuiggegevens?size=1&query=RZ874R"],
  ["Vehicle detail", "/voertuiggegevens/RZ874R"],
  ["BAG list", "/bag?size=1&filters%5Bpostcode%5D=3015BA"],
  ["BAG detail", "/bag/3015ba-nieuwe-binnenweg-10-c"],
];

for (const [label, path] of probes) {
  const response = await fetch(`https://api.overheid.io${path}`, {
    headers: { "ovio-api-key": upstreamKey, accept: "application/json" },
    signal: AbortSignal.timeout(20_000),
  });
  await responseJson(response, label);
  console.log(`ok  ${label}`);
}

const adminHeaders = {
  apikey: serviceKey,
  authorization: `Bearer ${serviceKey}`,
  "content-type": "application/json",
};
const sourceUrl =
  `${supabaseUrl}/rest/v1/apis?select=id,listing_metadata,active,is_verified,discoverable` +
  `&slug=eq.${encodeURIComponent(SOURCE_SLUG)}`;
const rows = await responseJson(
  await fetch(sourceUrl, { headers: adminHeaders, signal: AbortSignal.timeout(20_000) }),
  "Read staged overheid.io source",
);
if (!Array.isArray(rows) || rows.length !== 1) {
  throw new Error("The staged overheid.io source was not found exactly once; apply the migration first");
}

const existing = rows[0];
const metadata =
  existing.listing_metadata && typeof existing.listing_metadata === "object"
    ? { ...existing.listing_metadata }
    : {};
const authHeaders = Array.isArray(metadata.auth_headers)
  ? metadata.auth_headers.filter(
      (header) =>
        !header ||
        typeof header !== "object" ||
        String(header.name).toLowerCase() !== "ovio-api-key",
    )
  : [];
metadata.auth_headers = [
  ...authHeaders,
  { name: "ovio-api-key", value: upstreamKey },
];

const activated = await responseJson(
  await fetch(`${supabaseUrl}/rest/v1/apis?slug=eq.${encodeURIComponent(SOURCE_SLUG)}`, {
    method: "PATCH",
    headers: { ...adminHeaders, prefer: "return=representation" },
    body: JSON.stringify({
      listing_metadata: metadata,
      status: "active",
      active: true,
      is_verified: true,
      discoverable: true,
      suspended_at: null,
      verified_at: new Date().toISOString(),
    }),
    signal: AbortSignal.timeout(20_000),
  }),
  "Activate overheid.io source",
);
if (!Array.isArray(activated) || activated.length !== 1) {
  throw new Error("Activation did not update exactly one overheid.io source");
}

console.log(`activated ${SOURCE_SLUG}; credential stored server-side (value not printed)`);
