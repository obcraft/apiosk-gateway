-- Gateway v2 reads its catalogue through column grants. 6626a32 made the
-- source directory read catalog.capability_offers (offered capabilities) and
-- the endpoint pages read api_endpoints.use_cases, neither of which the runtime
-- role could read, so every catalogue route, including Ask, answered 503
-- catalog_unavailable after release v126. Grant exactly those columns.
GRANT SELECT (api_id, capability_slug) ON catalog.capability_offers TO gateway_v2_runtime;
GRANT SELECT (use_cases) ON providers.api_endpoints TO gateway_v2_runtime;
