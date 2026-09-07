-- A narrow counterpart to the global review gate for this platform-managed
-- import. The service-role caller still cannot activate anything else, and
-- this source cannot become visible before its credential and complete,
-- validated catalogue are present.

begin;

create or replace function public.activate_overheid_io_source()
returns jsonb
language plpgsql
security definer
set search_path = public, pg_temp
as $function$
declare
  source_id constant uuid := '3ed22ff1-5dc3-42ee-909b-2771cd9361e5';
  source_row public.apis%rowtype;
  endpoint_total integer;
  contract_total integer;
  credential_ready boolean;
begin
  if current_setting('request.jwt.claim.role', true) is distinct from 'service_role' then
    raise exception 'not_authorized' using errcode = '42501';
  end if;

  select * into source_row
  from public.apis
  where id = source_id and slug = 'overheid-io'
  for update;

  if not found then
    raise exception 'overheid_io_source_missing';
  end if;

  select exists (
    select 1
    from jsonb_array_elements(
      case
        when jsonb_typeof(source_row.listing_metadata->'auth_headers') = 'array'
          then source_row.listing_metadata->'auth_headers'
        else '[]'::jsonb
      end
    ) header
    where lower(header->>'name') = 'ovio-api-key'
      and length(coalesce(header->>'value', '')) >= 20
  ) into credential_ready;

  select count(*) into endpoint_total
  from public.api_endpoints
  where api_id = source_id and active = true;

  select count(*) into contract_total
  from public.capability_execution_contracts contract
  join public.api_endpoints endpoint on endpoint.id = contract.endpoint_id
  where endpoint.api_id = source_id
    and endpoint.active = true
    and contract.status = 'active'
    and contract.validated_at is not null
    and contract.effect = 'read'
    and contract.price_model = 'per_call';

  if not credential_ready or endpoint_total <> 10 or contract_total <> 10 then
    raise exception 'overheid_io_not_ready: credential=%, endpoints=%, contracts=%',
      credential_ready, endpoint_total, contract_total;
  end if;

  -- This is the exact, transaction-local capability expected by the global
  -- review trigger. It names one fixed API and is cleared immediately after.
  perform set_config('apiosk.api_review_approved_id', source_id::text, true);
  update public.apis
  set status = 'active',
      active = true,
      is_verified = true,
      discoverable = true,
      approved_at = coalesce(approved_at, now()),
      suspended_at = null
  where id = source_id;
  perform set_config('apiosk.api_review_approved_id', '', true);

  select * into source_row from public.apis where id = source_id;
  if source_row.status <> 'active' or not source_row.active
     or not source_row.is_verified or not source_row.discoverable then
    raise exception 'overheid_io_activation_failed';
  end if;

  return jsonb_build_object(
    'id', source_row.id,
    'slug', source_row.slug,
    'status', source_row.status,
    'active', source_row.active,
    'is_verified', source_row.is_verified,
    'discoverable', source_row.discoverable,
    'endpoint_count', endpoint_total,
    'contract_count', contract_total
  );
end
$function$;

revoke all on function public.activate_overheid_io_source() from public, anon, authenticated;
grant execute on function public.activate_overheid_io_source() to service_role;

commit;
