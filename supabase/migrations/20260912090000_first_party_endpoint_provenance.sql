-- Record the provenance that was already known but never written down.
--
-- `metadata_source` arrived with 109 and was filled by the 110 backfill, which
-- covered the resellers and left the first-party registers NULL. NULL is what
-- the readiness audit reads as "lacks provenance", so five sources that were
-- onboarded FROM the provider's own documentation, with every path called
-- before it was written, have been reported ever since as though nobody knew
-- where their metadata came from.
--
-- This records a fact we hold; it does not create one. Each onboarding script
-- in gateway/legacy-executor/migrations states its own source and its own
-- verification date, which is what makes 'provider_documentation_and_live_probe'
-- — the value koop, rdw and rvo already carry — the accurate one here:
--
--   pdok             onboard-pdok.sql            21 curated services, PDOK's own
--                                                landing pages; no credential needed
--   tenderned        onboard-tenderned.sql       public JSON API captured 2026-09-04,
--                                                every path and parameter verified live
--   companies-house  onboard-companies-house.sql Companies House Swagger 2.0 spec
--   gleif            onboard-gleif.sql           GLEIF's own Postman docs plus live
--                                                calls against api.gleif.org, 2026-09-03
--   cbs-statline     20260908180815_onboard_cbs_statline.sql  curated StatLine OData tables
--
-- NOT eurostat, whose ten endpoints are also NULL. Its metadata was rendered
-- from tests/fixtures/public-statistics/catalog.json by
-- scripts/render-public-statistics-migration.mjs, and that fixture is a planner
-- snapshot, not a probe log. Nothing in this repository records those ten paths
-- being called. Verify them first; a stamp is not a verification.
--
-- Keyed on (api slug, method, path), never on endpoint uuid: a catalogue
-- rebuild regenerates uuids and the route is what identifies an endpoint.
-- Idempotent, and it will not touch a row a person has since marked
-- 'curated' — a hand correction outranks this file permanently.
--
-- Changes no description, tag, category or input schema. Provenance only.
BEGIN;
SELECT pg_advisory_xact_lock(hashtext('apiosk-first-party-provenance'));

-- cbs-statline: 1 endpoint
UPDATE public.api_endpoints e SET metadata_source='provider_documentation_and_live_probe', metadata_confidence=1
 FROM public.apis a
 WHERE a.id=e.api_id AND a.slug='cbs-statline' AND e.active
   AND (e.method,e.path) IN (('GET','/odata/v1/CBS/{tableId}/TypeGefailleerdeCodes'))
   AND coalesce(e.metadata_source,'') NOT IN ('curated','provider_documentation_and_live_probe');

-- companies-house: 3 endpoints
UPDATE public.api_endpoints e SET metadata_source='provider_documentation_and_live_probe', metadata_confidence=1
 FROM public.apis a
 WHERE a.id=e.api_id AND a.slug='companies-house' AND e.active
   AND (e.method,e.path) IN (('GET','/company/{company_number}/filing-history'),('GET','/company/{company_number}/officers'),('GET','/company/{company_number}/persons-with-significant-control'))
   AND coalesce(e.metadata_source,'') NOT IN ('curated','provider_documentation_and_live_probe');

-- gleif: 4 endpoints
UPDATE public.api_endpoints e SET metadata_source='provider_documentation_and_live_probe', metadata_confidence=1
 FROM public.apis a
 WHERE a.id=e.api_id AND a.slug='gleif' AND e.active
   AND (e.method,e.path) IN (('GET','/lei-records/{lei}/direct-children'),('GET','/lei-records/{lei}/direct-parent-relationship'),('GET','/lei-records/{lei}/ultimate-children'),('GET','/lei-records/{lei}/ultimate-parent-relationship'))
   AND coalesce(e.metadata_source,'') NOT IN ('curated','provider_documentation_and_live_probe');

-- pdok: 21 endpoints
UPDATE public.api_endpoints e SET metadata_source='provider_documentation_and_live_probe', metadata_confidence=1
 FROM public.apis a
 WHERE a.id=e.api_id AND a.slug='pdok' AND e.active
   AND (e.method,e.path) IN (('GET','/bzk/locatieserver/search/v3_1/free'),('GET','/bzk/locatieserver/search/v3_1/lookup'),('GET','/bzk/locatieserver/search/v3_1/reverse'),('GET','/bzk/locatieserver/search/v3_1/suggest'),('GET','/cbs/postcode4/ogc/v1/collections/postcode4/items'),('GET','/cbs/postcode6/ogc/v1/collections/postcode6/items'),('GET','/cbs/wijken-en-buurten-2025/ogc/v1/collections/buurten/items'),('GET','/kadaster/bag/ogc/v2/collections/adres/items'),('GET','/kadaster/bag/ogc/v2/collections/pand/items'),('GET','/kadaster/bag/ogc/v2/collections/verblijfsobject/items'),('GET','/kadaster/brk-bestuurlijke-gebieden/ogc/v1/collections/gemeentegebied/items'),('GET','/kadaster/brk-bestuurlijke-gebieden/ogc/v1/collections/provinciegebied/items'),('GET','/kadaster/brk-kadastrale-kaart/ogc/v1/collections/perceel/items'),('GET','/kadaster/brk-publiekrechtelijke-beperkingen-wkpb/ogc/v1/collections/pb_multipolygon/items'),('GET','/kadaster/brt-top10nl/ogc/v1/collections/gebouw_vlak/items'),('GET','/lv/bgt/ogc/v1/collections/pand/items'),('GET','/prorail/spoorwegen/ogc/v1/collections/station/items'),('GET','/rvo/gewaspercelen/ogc/v1/collections/brpgewas/items'),('GET','/rvo/indicatieve-aandachtsgebieden-funderingsproblematiek/ogc/v1/collections/indgebfunderingsproblematiek/items'),('GET','/rvo/natura2000/ogc/v1/collections/natura2000/items'),('GET','/rws/nationaal-wegenbestand-wegen/ogc/v1/collections/wegvakken/items'))
   AND coalesce(e.metadata_source,'') NOT IN ('curated','provider_documentation_and_live_probe');

-- tenderned: 3 endpoints
UPDATE public.api_endpoints e SET metadata_source='provider_documentation_and_live_probe', metadata_confidence=1
 FROM public.apis a
 WHERE a.id=e.api_id AND a.slug='tenderned' AND e.active
   AND (e.method,e.path) IN (('GET','/rss/laatste-publicatie.rss'),('GET','/v2/nutscodes'),('GET','/v2/publicaties'))
   AND coalesce(e.metadata_source,'') NOT IN ('curated','provider_documentation_and_live_probe');

-- Nothing first-party may be left without provenance except the ten Eurostat
-- paths this file deliberately does not touch. A source added later with NULL
-- provenance fails here rather than quietly widening the gap.
DO $$ DECLARE remaining int; BEGIN
 SELECT count(*) INTO remaining FROM public.api_endpoints e JOIN public.apis a ON a.id=e.api_id
  WHERE e.active AND a.status='active' AND a.is_verified AND coalesce(a.discoverable,true)
    AND nullif(trim(coalesce(e.metadata_source,'')),'') IS NULL
    AND a.slug IN ('cbs-statline','companies-house','gleif','pdok','tenderned');
 IF remaining > 0 THEN RAISE EXCEPTION 'Provenance still missing on % endpoints', remaining; END IF;
END $$;

COMMIT;
