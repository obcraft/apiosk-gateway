#!/usr/bin/env node

import fs from "node:fs/promises";

const SOURCE_SLUG = "overheid-io";
const EXPECTED_PATHS = [
  "/v3/openkvk",
  "/v3/openkvk/{id}",
  "/v3/suggest/openkvk/{query}",
  "/v3/geo/openkvk/radius/{lat}/{lon}/{radius}",
  "/v3/geo/openkvk/around/{id}/{radius}",
  "/v3/geo/openkvk/box/{tlLat}/{tlLon}/{brLat}/{brLon}",
  "/voertuiggegevens",
  "/voertuiggegevens/{kenteken}",
  "/bag",
  "/bag/{bag_id}",
].sort();

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

function object(value) {
  return value && typeof value === "object" && !Array.isArray(value);
}

const env = parseEnv(await fs.readFile(new URL("../.env", import.meta.url), "utf8"));
const supabaseUrl = required(env, "SUPABASE_URL").replace(/\/$/, "");
const serviceKey = required(env, "SUPABASE_SERVICE_ROLE_KEY");
const headers = { apikey: serviceKey, authorization: `Bearer ${serviceKey}` };

async function read(table, params) {
  const url = new URL(`${supabaseUrl}/rest/v1/${table}`);
  for (const [name, value] of Object.entries(params)) url.searchParams.set(name, value);
  const response = await fetch(url, {
    headers,
    signal: AbortSignal.timeout(20_000),
  });
  if (!response.ok) {
    throw new Error(`${table} audit failed (HTTP ${response.status}): ${await response.text()}`);
  }
  return response.json();
}

const sources = await read("apis", {
  select: "id,slug,status,active,is_verified,discoverable,category,listing_metadata",
  slug: `eq.${SOURCE_SLUG}`,
});
if (sources.length !== 1) throw new Error(`Expected exactly one ${SOURCE_SLUG} source`);
const source = sources[0];

const endpoints = await read("api_endpoints", {
  select:
    "id,path,method,active,description,query_params,response_body,tags,sectors,use_cases,endpoint_category,action_type",
  api_id: `eq.${source.id}`,
  order: "path",
});
const endpointIds = endpoints.map(({ id }) => id);
const contracts = endpointIds.length
  ? await read("capability_execution_contracts", {
      select:
        "id,endpoint_id,capability_slug,status,requires,produces,input_map,output_map,validated_at,identifies,effect,price_model",
      endpoint_id: `in.(${endpointIds.join(",")})`,
    })
  : [];
const capabilitySlugs = [...new Set(contracts.map(({ capability_slug }) => capability_slug))];
const capabilities = capabilitySlugs.length
  ? await read("capabilities", {
      select: "*",
      slug: `in.(${capabilitySlugs.join(",")})`,
    })
  : [];

const referencedFacts = new Set();
for (const contract of contracts) {
  for (const item of [...(contract.requires ?? []), ...(contract.produces ?? [])]) {
    if (item?.fact_type) referencedFacts.add(item.fact_type);
  }
  for (const fact of Object.keys(contract.input_map ?? {})) referencedFacts.add(fact);
  for (const fact of Object.keys(contract.output_map?.produces ?? {})) referencedFacts.add(fact);
}
const facts = referencedFacts.size
  ? await read("fact_types", {
      select: "slug,status,value_schema",
      slug: `in.(${[...referencedFacts].join(",")})`,
    })
  : [];
const discoveryCapabilities = process.argv.includes("--find-capabilities")
  ? await read("capabilities", {
      select: "slug,name,description,category,status",
      or: "(slug.ilike.*vehicle*,slug.ilike.*address*,slug.ilike.*building*,description.ilike.*vehicle*,description.ilike.*address*)",
      order: "slug",
    })
  : [];
const produceSamples = process.argv.includes("--sample-produces")
  ? (await read("capability_execution_contracts", {
      select: "capability_slug,produces,output_map",
      status: "eq.active",
      limit: "500",
    }))
      .filter(({ produces }) => Array.isArray(produces) && produces.length > 0)
      .slice(0, 5)
  : [];

const violations = [];
const expect = (condition, message) => {
  if (!condition) violations.push(message);
};
const metadata = source.listing_metadata ?? {};
expect(source.status === "active" && source.active, "source is not active");
expect(source.is_verified && source.discoverable, "source is not verified and discoverable");
expect(typeof source.category === "string" && source.category.length > 0, "source category missing");
expect(Array.isArray(metadata.tags) && metadata.tags.length >= 10, "source discovery tags missing");
expect(Array.isArray(metadata.sectors) && metadata.sectors.length >= 3, "source sectors missing");
expect(Array.isArray(metadata.categories) && metadata.categories.length === 4, "source categories incomplete");
expect(Array.isArray(metadata.service_groups) && metadata.service_groups.length === 3, "service groups incomplete");

const actualPaths = endpoints.map(({ path }) => path).sort();
expect(JSON.stringify(actualPaths) === JSON.stringify(EXPECTED_PATHS), "endpoint path set is incomplete or unexpected");

const contractsByEndpoint = Map.groupBy(contracts, ({ endpoint_id }) => endpoint_id);
const capabilityBySlug = new Map(capabilities.map((capability) => [capability.slug, capability]));
for (const endpoint of endpoints) {
  const label = endpoint.path;
  expect(endpoint.active && endpoint.method === "GET", `${label}: endpoint is not an active GET`);
  expect(typeof endpoint.description === "string" && endpoint.description.length >= 20, `${label}: description missing`);
  expect(object(endpoint.query_params) && endpoint.query_params.type === "object", `${label}: input schema missing`);
  expect(object(endpoint.query_params?.properties), `${label}: input properties missing`);
  expect(object(endpoint.response_body) && ["object", "array"].includes(endpoint.response_body.type), `${label}: output schema missing`);
  expect(
    Object.keys(endpoint.response_body?.properties ?? endpoint.response_body?.items?.properties ?? {}).length > 0,
    `${label}: output properties missing`,
  );
  expect(Array.isArray(endpoint.tags) && endpoint.tags.length >= 3, `${label}: endpoint tags incomplete`);
  expect(Array.isArray(endpoint.sectors) && endpoint.sectors.length >= 1, `${label}: endpoint sectors missing`);
  expect(Array.isArray(endpoint.use_cases) && endpoint.use_cases.length >= 1, `${label}: endpoint use cases missing`);
  expect(Boolean(endpoint.endpoint_category), `${label}: endpoint category missing`);
  expect(Boolean(endpoint.action_type), `${label}: endpoint action type missing`);

  const endpointContracts = contractsByEndpoint.get(endpoint.id) ?? [];
  expect(endpointContracts.length === 1, `${label}: expected exactly one execution contract`);
  for (const contract of endpointContracts) {
    const capability = capabilityBySlug.get(contract.capability_slug);
    expect(contract.status === "active" && Boolean(contract.validated_at), `${label}: contract is not active and validated`);
    expect(capability?.status === "active" && Boolean(capability.description), `${label}: capability is missing or inactive`);
    expect(
      object(capability?.input_contract) &&
        capability.input_contract.type === "object" &&
        object(capability.input_contract.properties) &&
        Object.keys(capability.input_contract.properties).length > 0,
      `${label}: capability input contract missing`,
    );
    expect(
      object(capability?.output_contract) &&
        ["object", "array"].includes(capability.output_contract.type) &&
        Object.keys(
          capability.output_contract.properties ?? capability.output_contract.items?.properties ?? {},
        ).length > 0,
      `${label}: capability output contract missing`,
    );
    expect(Array.isArray(contract.requires) && contract.requires.length >= 1, `${label}: contract requires are empty`);
    expect(Array.isArray(contract.produces) && contract.produces.length >= 1, `${label}: contract produces are empty`);
    expect(object(contract.input_map) && Object.keys(contract.input_map).length >= 1, `${label}: contract input map is empty`);
    expect(object(contract.output_map?.produces) && Object.keys(contract.output_map.produces).length >= 1, `${label}: contract output map is empty`);
    expect(contract.effect === "read" && contract.price_model === "per_call", `${label}: contract execution semantics are incomplete`);
    for (const requirement of contract.requires ?? []) {
      expect(Boolean(contract.input_map?.[requirement.fact_type]), `${label}: required fact ${requirement.fact_type} has no input mapping`);
    }
    for (const produced of contract.produces ?? []) {
      expect(Boolean(contract.output_map?.produces?.[produced.fact_type]), `${label}: produced fact ${produced.fact_type} has no output mapping`);
    }
  }
}

const factBySlug = new Map(facts.map((fact) => [fact.slug, fact]));
for (const slug of referencedFacts) {
  const fact = factBySlug.get(slug);
  expect(fact?.status === "active" && object(fact.value_schema), `fact ${slug} is missing, inactive or has no value schema`);
}
expect(
  Array.isArray(metadata.capability_slugs) &&
    capabilitySlugs.every((slug) => metadata.capability_slugs.includes(slug)),
  "source capability index is incomplete",
);

console.log(
  JSON.stringify(
    {
      source: SOURCE_SLUG,
      endpoints: endpoints.length,
      inputSchemas: endpoints.filter(({ query_params }) => object(query_params)).length,
      outputSchemas: endpoints.filter(({ response_body }) => object(response_body)).length,
      endpointTaxonomies: endpoints.filter(
        ({ tags, sectors, use_cases, endpoint_category, action_type }) =>
          tags?.length && sectors?.length && use_cases?.length && endpoint_category && action_type,
      ).length,
      contracts: contracts.length,
      activeValidatedContracts: contracts.filter(
        ({ status, validated_at }) => status === "active" && validated_at,
      ).length,
      capabilities: capabilities.length,
      ...(process.argv.includes("--dump-capabilities") ? { capabilityRows: capabilities } : {}),
      ...(process.argv.includes("--find-capabilities")
        ? { discoveryCapabilities }
        : {}),
      ...(process.argv.includes("--sample-produces") ? { produceSamples } : {}),
      referencedFacts: referencedFacts.size,
      ...(process.argv.includes("--dump-facts") ? { factRows: facts } : {}),
      coverage: endpoints.map((endpoint) => {
        const [contract] = contractsByEndpoint.get(endpoint.id) ?? [];
        return {
          path: endpoint.path,
          category: endpoint.endpoint_category,
          tags: endpoint.tags,
          capability: contract?.capability_slug ?? null,
          requires: contract?.requires?.map(({ fact_type }) => fact_type) ?? [],
          produces: contract?.produces?.map(({ fact_type }) => fact_type) ?? [],
          mappedOutputs: Object.keys(contract?.output_map?.produces ?? {}),
        };
      }),
      violations,
    },
    null,
    2,
  ),
);

if (violations.length) process.exitCode = 1;
