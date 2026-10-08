-- Admin's existing Approve & publish button calls this shared RPC, not the
-- Bazaar batch helper. Publish the accepted Bazaar listing in the same review
-- transaction so it can pass the Sources directory's visibility predicates.
-- Listing review does not validate a paid response or promote execution contracts.
BEGIN;
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '60s';

CREATE OR REPLACE FUNCTION public.admin_review_api(p_api_id uuid, p_decision text)
RETURNS TABLE (id uuid, status text)
LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
DECLARE
  v_publish_bazaar boolean := false;
BEGIN
  IF NOT public.is_active_platform_admin() THEN
    RAISE EXCEPTION 'not_authorized' USING ERRCODE = '42501';
  END IF;
  IF p_decision NOT IN ('approve', 'reject') THEN
    RAISE EXCEPTION 'invalid_decision' USING ERRCODE = '22023';
  END IF;

  -- Lock the row whose identity and endpoint eligibility we review. A repeated
  -- decision remains a no-op, as in the existing pending-only review contract.
  SELECT p_decision = 'approve'
     AND a.owner_id = providers.bazaar_owner()
     AND a.slug LIKE 'cbz-%'
     AND EXISTS (SELECT 1 FROM providers.api_endpoints e WHERE e.api_id = a.id AND e.active)
    INTO v_publish_bazaar
    FROM providers.apis a
   WHERE a.id = p_api_id AND a.status = 'pending'
   FOR UPDATE OF a;

  IF p_decision = 'approve' THEN
    PERFORM set_config('apiosk.api_review_approved_id', p_api_id::text, true);
  END IF;

  RETURN QUERY
  UPDATE providers.apis a
     SET status = CASE WHEN p_decision = 'approve' THEN 'active' ELSE 'inactive' END,
         approved_at = CASE WHEN p_decision = 'approve' THEN coalesce(a.approved_at, now()) ELSE a.approved_at END,
         reviewed_by = auth.uid(),
         discoverable = CASE WHEN v_publish_bazaar THEN true ELSE a.discoverable END,
         is_verified = CASE WHEN v_publish_bazaar THEN true ELSE a.is_verified END,
         verified_at = CASE WHEN v_publish_bazaar THEN coalesce(a.verified_at, now()) ELSE a.verified_at END
   WHERE a.id = p_api_id AND a.status = 'pending'
   RETURNING a.id, a.status;

  IF p_decision = 'approve' THEN
    PERFORM set_config('apiosk.api_review_approved_id', '', true);
  END IF;
END $$;

REVOKE ALL ON FUNCTION public.admin_review_api(uuid, text) FROM PUBLIC, anon;
GRANT EXECUTE ON FUNCTION public.admin_review_api(uuid, text) TO authenticated;

-- The grouped Sources card reads its mark from this provider identity. Each
-- service keeps its own listing logo; the group always carries Coinbase's mark.
UPDATE providers.provider_profiles
   SET avatar_url = 'https://app.apiosk.com/logos/coinbase.svg'
 WHERE id = providers.bazaar_owner() AND username = 'coinbase-bazaar';

COMMIT;
