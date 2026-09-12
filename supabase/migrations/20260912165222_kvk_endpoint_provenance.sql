-- Record KVK metadata provenance in the physical provider schema.
-- Official inputs: developers.kvk.nl/nl/documentation, naamgeving-api and
-- testing/swagger-vestigingsprofiel-api; verified 2026-09-12.
-- Production saved results prove profile, annual-account and search retrieval.
-- Names and branch profiles have documented contracts and stored samples;
-- those samples alone do not establish live execution, so label documentation only.
-- No prices, input schemas, cached documents or curated metadata are changed.
BEGIN;
SELECT pg_advisory_xact_lock(hashtext('apiosk-kvk-provenance'));

UPDATE providers.api_endpoints e SET metadata_source=CASE WHEN e.path IN ('/api/v1/naamgevingen/kvknummer','/api/v1/vestigingsprofielen') THEN 'provider_documentation' ELSE 'provider_documentation_and_live_probe' END, metadata_confidence=1
 FROM providers.apis a
 WHERE a.id=e.api_id AND a.slug='kvk-dutch-business-register' AND e.active
   AND (e.method,e.path) IN (
     ('GET','/api/v1/basisprofielen'),
     ('GET','/api/v1/hvds/jaarrekeningen/kvknummer'),
     ('GET','/api/v1/naamgevingen/kvknummer'),
     ('GET','/api/v1/vestigingsprofielen'),
     ('GET','/api/v2/zoeken'))
   AND coalesce(e.metadata_source,'') NOT IN ('curated','provider_documentation','provider_documentation_and_live_probe');

-- All five, or the route list is stale and this file must be re-read rather
-- than quietly having updated nothing.
DO $verify$
DECLARE total integer; verified integer;
BEGIN
 SELECT count(*), count(*) FILTER (WHERE e.metadata_source IN ('curated','provider_documentation','provider_documentation_and_live_probe'))
 INTO total, verified
 FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id
 WHERE a.slug='kvk-dutch-business-register' AND e.active
   AND (e.method,e.path) IN (
     ('GET','/api/v1/basisprofielen'),
     ('GET','/api/v1/hvds/jaarrekeningen/kvknummer'),
     ('GET','/api/v1/naamgevingen/kvknummer'),
     ('GET','/api/v1/vestigingsprofielen'),
     ('GET','/api/v2/zoeken'));
 IF total <> 5 OR verified <> 5 THEN
   RAISE EXCEPTION 'Expected five verified KVK endpoints, found % total and % verified', total, verified;
 END IF;
END $verify$;

COMMIT;
