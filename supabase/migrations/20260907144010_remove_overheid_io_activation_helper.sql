-- The source was approved through the normal admin review path. Remove the
-- temporary, unused provisioning helper so the global review RPC remains the
-- only activation capability.

begin;

drop function if exists public.activate_overheid_io_source();

commit;
