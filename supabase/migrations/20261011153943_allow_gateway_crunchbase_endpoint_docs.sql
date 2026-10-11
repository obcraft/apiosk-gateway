-- The runtime catalog uses these metadata columns to attribute official Crunchbase MCP operations correctly.
-- Grant column-level SELECT only to the existing server runtime role; no client-role or write access.
GRANT SELECT (origin_override, external_docs_url) ON providers.api_endpoints TO gateway_v2_runtime;
