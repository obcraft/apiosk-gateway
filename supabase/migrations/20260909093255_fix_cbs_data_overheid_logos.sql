-- Replace the two broken listing logos with the official assets currently
-- published by CBS and data.overheid.nl. The previous URLs both return 404,
-- which makes the Sources cards fall back to initials.

begin;

do $logos$
declare
  cbs_id constant uuid := 'c5939334-ac41-40d0-80d5-ced0101a7ad6';
  donl_id constant uuid := '803bcf10-ae0f-4875-9872-b25585d9b92a';
begin
  if not exists (
    select 1 from public.apis
    where id = cbs_id and slug = 'cbs-statline'
  ) then
    raise exception 'CBS source is missing';
  end if;

  if not exists (
    select 1 from public.apis
    where id = donl_id and slug = 'data-overheid-nl'
  ) then
    raise exception 'data.overheid.nl source is missing';
  end if;

  update public.apis
  set listing_metadata = coalesce(listing_metadata, '{}'::jsonb)
    || jsonb_build_object(
      'logo', 'https://cdn.cbs.nl/cdn/images/apple-touch-icon.png'
    )
  where id = cbs_id;

  update public.apis
  set listing_metadata = coalesce(listing_metadata, '{}'::jsonb)
    || jsonb_build_object(
      'logo', 'https://data.overheid.nl/themes/custom/koop_overheid/favicon.ico'
    )
  where id = donl_id;
end;
$logos$;

do $postconditions$
declare
  cbs_logo text;
  donl_logo text;
begin
  select listing_metadata->>'logo' into cbs_logo
  from public.apis
  where id = 'c5939334-ac41-40d0-80d5-ced0101a7ad6'::uuid
    and slug = 'cbs-statline';

  select listing_metadata->>'logo' into donl_logo
  from public.apis
  where id = '803bcf10-ae0f-4875-9872-b25585d9b92a'::uuid
    and slug = 'data-overheid-nl';

  if cbs_logo is distinct from 'https://cdn.cbs.nl/cdn/images/apple-touch-icon.png'
     or donl_logo is distinct from 'https://data.overheid.nl/themes/custom/koop_overheid/favicon.ico' then
    raise exception 'source logo update failed: cbs=%, data.overheid.nl=%',
      cbs_logo, donl_logo;
  end if;
end;
$postconditions$;

commit;
