-- Publish the four reviewed NL benefits listings for catalogue browsing.
-- Follows the existing EODHD browse-only contract: visible endpoints are not
-- executable. Native runtime deployment/acceptance remains a separate step.
-- Nibud is deliberately excluded at the owner's request.
BEGIN;

DO $catalogue$
DECLARE
  spec record;
  source_row providers.apis%rowtype;
  total integer;
BEGIN
  FOR spec IN SELECT * FROM (VALUES
    ('samenwerkende-catalogi', 1, 'https://apiosk.com/brands/koop.png', 'https://www.koopoverheid.nl', 'SRU catalogue metadata is listed; normalized execution is not yet available.'),
    ('dienst-toeslagen', 5, 'https://www.belastingdienst.nl/bld-assets/bld/images/apple-touch-icon.png', 'https://www.belastingdienst.nl/wps/wcm/connect/nl/toeslagen/toeslagen', 'Official information pages are listed; normalized retrieval is not yet available.'),
    ('nl-toeslagen-rekenservice', 4, 'https://app.apiosk.com/apple-touch-icon.png', 'https://apiosk.com', 'Four 2026 estimate APIs are implemented locally; production deployment and execution acceptance are pending.'),
    ('nl-gemeentelijke-regelingen', 1, 'https://app.apiosk.com/apple-touch-icon.png', 'https://apiosk.com', 'Municipal retrieval is implemented locally; production deployment and execution acceptance are pending.')
  ) AS sources(slug, endpoint_count, logo, website, notice)
  LOOP
    SELECT * INTO STRICT source_row FROM providers.apis WHERE slug=spec.slug FOR UPDATE;
    SELECT count(*) INTO total FROM providers.api_endpoints WHERE api_id=source_row.id;
    IF total <> spec.endpoint_count THEN
      RAISE EXCEPTION '% requires % endpoints, found %', spec.slug, spec.endpoint_count, total;
    END IF;
    IF source_row.status <> 'inactive' OR source_row.suspended_at IS NOT NULL THEN
      RAISE EXCEPTION '% must be an unsuspended staged source', spec.slug;
    END IF;
    IF EXISTS (SELECT 1 FROM catalog.capability_execution_contracts c
      JOIN providers.api_endpoints e ON e.id=c.endpoint_id
      WHERE e.api_id=source_row.id AND c.status <> 'draft') THEN
      RAISE EXCEPTION '% already has reviewed execution contracts', spec.slug;
    END IF;

    UPDATE providers.api_endpoints SET active=true WHERE api_id=source_row.id;
    -- Same API-specific publication capability used by the approval RPC and
    -- the existing browse-only migration; this approves listing visibility.
    PERFORM set_config('apiosk.api_review_approved_id', source_row.id::text, true);
    UPDATE providers.apis SET status='active', active=true, discoverable=true,
      is_verified=true, approved_at=coalesce(approved_at,now()), website_url=spec.website,
      listing_metadata=coalesce(listing_metadata,'{}'::jsonb) || jsonb_build_object(
        'logo',spec.logo,
        'sectors',jsonb_build_array('Government and public data'),
        'catalog_state','browse_only',
        'execution_enabled',false,
        'browse_notice',spec.notice,
        'planner_block_reason',spec.notice,
        'catalogue_review',jsonb_build_object('checked_on','2026-10-13','scope','branding and documented endpoints; no execution approval')
      )
    WHERE id=source_row.id;
    PERFORM set_config('apiosk.api_review_approved_id', '', true);

    IF NOT EXISTS (SELECT 1 FROM providers.apis WHERE id=source_row.id
      AND status='active' AND active AND discoverable AND is_verified
      AND suspended_at IS NULL AND listing_metadata->>'logo'=spec.logo
      AND listing_metadata->>'catalog_state'='browse_only'
      AND listing_metadata->'execution_enabled'='false'::jsonb) THEN
      RAISE EXCEPTION '% browse-only publication failed', spec.slug;
    END IF;
  END LOOP;

  UPDATE providers.apis SET listing_metadata=jsonb_set(listing_metadata,'{municipal_websites}',
    listing_metadata->'municipal_websites' || jsonb_build_object(
      'state','implemented_in_separate_source', 'source_slug','nl-gemeentelijke-regelingen',
      'execution_state','awaiting_deployment_and_acceptance',
      'coverage','Bounded catalogue-seeded HTML/PDF retrieval; not every municipality or page is accessible.',
      'blockers',jsonb_build_array('Production runtime deployment and acceptance'),
      'change_detection','Content hashes returned; no background change monitor'))
  WHERE slug='samenwerkende-catalogi';

  UPDATE providers.apis SET listing_metadata=jsonb_set(listing_metadata,'{eligibility}',
    listing_metadata->'eligibility' || jsonb_build_object(
      'state','estimates_implemented_in_separate_source', 'source_slug','nl-toeslagen-rekenservice',
      'execution_state','awaiting_deployment_and_acceptance',
      'eligibility_assessed',false,
      'requirements',jsonb_build_array('Production runtime deployment and acceptance'),
      'scope','2026 estimates for supported full-year situations; statutory entitlement is not assessed'))
  WHERE slug='dienst-toeslagen';
END;
$catalogue$;
COMMIT;
