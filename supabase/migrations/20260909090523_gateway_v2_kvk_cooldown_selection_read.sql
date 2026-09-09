-- The private server role needs only selection/endpoint IDs to coordinate
-- KVK's shared-IP rate limit across tasks. No client-role grants are added.
GRANT SELECT(id,candidate_id) ON public.user_selected_apis TO gateway_v2_runtime;
DROP POLICY IF EXISTS gateway_v2_runtime_selection_identity ON public.user_selected_apis;
CREATE POLICY gateway_v2_runtime_selection_identity ON public.user_selected_apis
 FOR SELECT TO gateway_v2_runtime USING (true);
