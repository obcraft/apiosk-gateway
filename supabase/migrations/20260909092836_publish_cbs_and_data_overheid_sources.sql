-- Publish the staged CBS StatLine and data.overheid.nl listings after live
-- read-only probes from the production Fly.io machine in ams.
--
-- On 2026-09-09 every active endpoint returned HTTP 200 with a non-empty
-- response: 16/16 for CBS and 3/3 for data.overheid.nl. Neither listing has a
-- Gateway v2 execution contract, so they are browseable sources and direct
-- endpoint offers; they do not claim planner-driven v2 execution.

begin;

do $publish$
declare
  cbs_id constant uuid := 'c5939334-ac41-40d0-80d5-ced0101a7ad6';
  donl_id constant uuid := '803bcf10-ae0f-4875-9872-b25585d9b92a';
  source_id uuid;
  source_slug text;
  expected_endpoints integer;
  endpoint_total integer;
  active_get_total integer;
  contract_total integer;
begin
  for source_id, source_slug, expected_endpoints in
    select * from (values
      (cbs_id, 'cbs-statline'::text, 16),
      (donl_id, 'data-overheid-nl'::text, 3)
    ) as sources(id, slug, endpoint_count)
  loop
    perform 1
    from public.apis
    where id = source_id and slug = source_slug
    for update;

    if not found then
      raise exception '% source is missing; apply its onboarding migration first', source_slug;
    end if;

    select count(*), count(*) filter (where active and method = 'GET')
      into endpoint_total, active_get_total
    from public.api_endpoints
    where api_id = source_id;

    if endpoint_total <> expected_endpoints
       or active_get_total <> expected_endpoints then
      raise exception '% publication requires % active GET endpoints; found % endpoints and % active GET endpoints',
        source_slug, expected_endpoints, endpoint_total, active_get_total;
    end if;

    select count(*) into contract_total
    from public.capability_execution_contracts contract
    join public.api_endpoints endpoint on endpoint.id = contract.endpoint_id
    where endpoint.api_id = source_id;

    if contract_total <> 0 then
      raise exception '% publication expected zero Gateway v2 execution contracts; found %',
        source_slug, contract_total;
    end if;

    -- The global review trigger accepts only this exact transaction-local id.
    perform set_config('apiosk.api_review_approved_id', source_id::text, true);
    update public.apis
    set status = 'active',
        active = true,
        is_verified = true,
        discoverable = true,
        verified_at = coalesce(verified_at, now()),
        approved_at = coalesce(approved_at, now()),
        suspended_at = null,
        settlement_mode = 'apiosk_revenue',
        requires_payout_wallet = false,
        listing_metadata = coalesce(listing_metadata, '{}'::jsonb) || jsonb_build_object(
          'catalog_state', 'published',
          'production_probe', jsonb_build_object(
            'region', 'ams',
            'probed_on', '2026-09-09',
            'http_200', expected_endpoints,
            'endpoint_count', expected_endpoints
          )
        )
    where id = source_id;
    perform set_config('apiosk.api_review_approved_id', '', true);
  end loop;
end;
$publish$;

do $postconditions$
declare
  source_id uuid;
  source_slug text;
  expected_endpoints integer;
  visible boolean;
  endpoint_total integer;
  contract_total integer;
  probe_count integer;
begin
  for source_id, source_slug, expected_endpoints in
    select * from (values
      ('c5939334-ac41-40d0-80d5-ced0101a7ad6'::uuid, 'cbs-statline'::text, 16),
      ('803bcf10-ae0f-4875-9872-b25585d9b92a'::uuid, 'data-overheid-nl'::text, 3)
    ) as sources(id, slug, endpoint_count)
  loop
    select status = 'active'
        and active
        and is_verified
        and discoverable
        and suspended_at is null
        and settlement_mode = 'apiosk_revenue'
        and not requires_payout_wallet,
      (listing_metadata->'production_probe'->>'http_200')::integer
      into visible, probe_count
    from public.apis
    where id = source_id and slug = source_slug;

    select count(*) into endpoint_total
    from public.api_endpoints
    where api_id = source_id and active and method = 'GET';

    select count(*) into contract_total
    from public.capability_execution_contracts contract
    join public.api_endpoints endpoint on endpoint.id = contract.endpoint_id
    where endpoint.api_id = source_id;

    if not coalesce(visible, false)
       or endpoint_total <> expected_endpoints
       or probe_count <> expected_endpoints
       or contract_total <> 0 then
      raise exception '% publication invariant failed: visible=%, active_get_endpoints=%, probe_count=%, contracts=%',
        source_slug, visible, endpoint_total, probe_count, contract_total;
    end if;
  end loop;
end;
$postconditions$;

commit;
