-- Publish the staged EODHD catalogue for discovery without enabling calls.
--
-- The legacy catalogue only counts active endpoints, so the source and its
-- endpoints must pass the normal visibility predicate. Runtime execution is
-- independently and centrally denied by listing_metadata.execution_enabled.
-- Gateway v2 also remains unable to execute these endpoints because this
-- migration deliberately creates no capability_execution_contracts.

begin;

do $migration$
declare
  source_id constant uuid := 'b268fe55-41d2-5b7a-b147-31af1f1c1d0f';
  source_row public.apis%rowtype;
  endpoint_total integer;
  active_contract_total integer;
begin
  select * into source_row
  from public.apis
  where id = source_id and slug = 'eodhd'
  for update;

  if not found then
    raise exception 'EODHD source is missing; run 20260908012000_onboard_eodhd_staged_source.sql first';
  end if;

  select count(*) into endpoint_total
  from public.api_endpoints
  where api_id = source_id;

  if endpoint_total <> 60 then
    raise exception 'EODHD browse publication requires exactly 60 endpoints; found %', endpoint_total;
  end if;

  select count(*) into active_contract_total
  from public.capability_execution_contracts contract
  join public.api_endpoints endpoint on endpoint.id = contract.endpoint_id
  where endpoint.api_id = source_id
    and contract.status = 'active';

  if active_contract_total <> 0 then
    raise exception 'EODHD browse publication refuses % active execution contracts', active_contract_total;
  end if;

  update public.api_endpoints
  set active = true
  where api_id = source_id;

  -- The global review trigger requires this transaction-local source id before
  -- an API may enter the visible catalogue. It grants no database role rights.
  perform set_config('apiosk.api_review_approved_id', source_id::text, true);
  update public.apis
  set status = 'active',
      active = true,
      is_verified = true,
      discoverable = true,
      approved_at = coalesce(approved_at, now()),
      suspended_at = null,
      long_description = 'EODHD by Unicorn Data Services provides 60 documented financial-data operations. Browse the available market, fundamentals, corporate events, options, fixed income, news, sentiment and macro endpoints. Calls remain unavailable until Apiosk has the required commercial rights, credentials and validated execution contracts.',
      listing_metadata = coalesce(listing_metadata, '{}'::jsonb) || jsonb_build_object(
        'catalog_state', 'browse_only',
        'execution_enabled', false,
        'browse_notice', 'Catalogue available. Execution will be enabled after commercial access, credentials and endpoint validation are complete.'
      )
  where id = source_id;
  perform set_config('apiosk.api_review_approved_id', '', true);
end;
$migration$;

do $postconditions$
declare
  endpoint_total integer;
  endpoint_active integer;
  active_contract_total integer;
  source_visible boolean;
  execution_enabled boolean;
  catalog_state text;
begin
  select count(*), count(*) filter (where active)
    into endpoint_total, endpoint_active
  from public.api_endpoints
  where api_id = 'b268fe55-41d2-5b7a-b147-31af1f1c1d0f'::uuid;

  select status = 'active'
      and active
      and is_verified
      and discoverable
      and suspended_at is null,
    coalesce((listing_metadata->>'execution_enabled')::boolean, true),
    listing_metadata->>'catalog_state'
    into source_visible, execution_enabled, catalog_state
  from public.apis
  where id = 'b268fe55-41d2-5b7a-b147-31af1f1c1d0f'::uuid;

  select count(*) into active_contract_total
  from public.capability_execution_contracts contract
  join public.api_endpoints endpoint on endpoint.id = contract.endpoint_id
  where endpoint.api_id = 'b268fe55-41d2-5b7a-b147-31af1f1c1d0f'::uuid
    and contract.status = 'active';

  if endpoint_total <> 60
     or endpoint_active <> 60
     or not coalesce(source_visible, false)
     or execution_enabled
     or catalog_state is distinct from 'browse_only'
     or active_contract_total <> 0 then
    raise exception 'EODHD browse invariant failed: endpoints=%, active_endpoints=%, visible=%, execution_enabled=%, catalog_state=%, active_contracts=%',
      endpoint_total, endpoint_active, source_visible, execution_enabled, catalog_state, active_contract_total;
  end if;
end;
$postconditions$;

commit;
