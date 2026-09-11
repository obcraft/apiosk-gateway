-- Read only public output declarations; no credential, billing or user-data grants.
GRANT SELECT(produces) ON public.capability_execution_contracts TO gateway_v2_runtime;
