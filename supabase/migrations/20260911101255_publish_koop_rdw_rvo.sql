-- Publish only the three staged adapters after production-egress validation.
-- Evidence: output/source-expansion-20260911/production-adapter-smoke-final.json.
begin;
select pg_advisory_xact_lock(hashtext('dutch-public-sources-v1'));
do $publish$
declare
  source_id uuid;
  source_slug text;
  expected_contracts integer;
  expected_path text;
  contract_total integer;
begin
  for source_slug, expected_contracts, expected_path in
    select * from (values
      ('koop',2,'/sru'),
      ('rdw',1,'/resource/m9d7-ebf2.json'),
      ('rvo',1,'/api/v1/opendata/subsidies')
    ) as sources(slug, contracts, path)
  loop
    select id into strict source_id from public.apis
      where slug=source_slug and is_platform_owned
      and settlement_mode='apiosk_revenue' and not requires_payout_wallet
      and listing_metadata->>'adapter'='dutch-public-sources-v1'
      and owner_id=(select owner_id from public.apis where slug='data-overheid-nl')
      for update;
    if (select count(*) from public.api_endpoints where api_id=source_id) <> 1
      or not exists(select 1 from public.api_endpoints where api_id=source_id
        and active and method='GET' and path=expected_path and price=0.047259) then
      raise exception '% endpoint publication invariant failed',source_slug;
    end if;
    select count(*) into contract_total from public.capability_execution_contracts c
      join public.api_endpoints e on e.id=c.endpoint_id
      where e.api_id=source_id and c.status='validated' and c.validated_at is not null
      and c.capability_slug=any(case source_slug
        when 'koop' then array['government.publication.search','government.publication.detail']
        when 'rdw' then array['vehicle.technical-profile']
        else array['subsidy.scheme.first-page-search'] end);
    if contract_total <> expected_contracts or
      (select count(*) from public.capability_execution_contracts c join public.api_endpoints e
        on e.id=c.endpoint_id where e.api_id=source_id) <> expected_contracts then
      raise exception '% validated contract publication invariant failed',source_slug;
    end if;
    perform set_config('apiosk.api_review_approved_id',source_id::text,true);
    update public.apis set status='active',active=true,is_verified=true,discoverable=true,
      verified_at=now(),approved_at=now(),suspended_at=null,
      listing_metadata=listing_metadata || jsonb_build_object('catalog_state','published',
        'production_probe',jsonb_build_object('region','ams','probed_on','2026-09-11',
          'adapter_cases_passed',expected_contracts,'evidence','production-adapter-smoke-final.json'))
      where id=source_id;
    perform set_config('apiosk.api_review_approved_id','',true);
    update public.capability_execution_contracts c set status='active'
      from public.api_endpoints e where c.endpoint_id=e.id and e.api_id=source_id;
    if not exists(select 1 from public.apis where id=source_id and active and is_verified
      and discoverable and status='active') or
      (select count(*) from public.capability_execution_contracts c join public.api_endpoints e
        on e.id=c.endpoint_id where e.api_id=source_id and c.status='active') <> expected_contracts then
      raise exception '% post-publication invariant failed',source_slug;
    end if;
  end loop;
end;
$publish$;
commit;
