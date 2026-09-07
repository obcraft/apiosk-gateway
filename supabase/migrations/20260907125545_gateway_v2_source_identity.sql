-- Read the same published provider handles used by the Sources page.
GRANT SELECT(owner_id) ON public.apis TO gateway_v2_runtime;
GRANT SELECT(id,username) ON public.provider_profiles TO gateway_v2_runtime;
DROP POLICY IF EXISTS gateway_v2_public_provider_handles ON public.provider_profiles;
CREATE POLICY gateway_v2_public_provider_handles ON public.provider_profiles FOR SELECT TO gateway_v2_runtime
USING (EXISTS (SELECT 1 FROM public.apis a WHERE a.owner_id=provider_profiles.id
 AND a.status='active' AND coalesce(a.active,true) AND a.is_verified=true
 AND coalesce(a.discoverable,true) AND a.suspended_at IS NULL));
