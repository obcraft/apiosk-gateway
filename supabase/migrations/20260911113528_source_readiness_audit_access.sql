-- Audit reads only catalog metadata and aggregates already-authorized v2 execution outcomes.
GRANT SELECT(description,tags,endpoint_category,query_params,request_body,metadata_source)
 ON public.api_endpoints TO gateway_v2_runtime;
GRANT SELECT(description) ON public.capabilities TO gateway_v2_runtime;
GRANT SELECT(api_slug) ON public.user_selected_apis TO gateway_v2_runtime;
