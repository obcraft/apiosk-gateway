-- The browser can store/replace the fixed API.market credential, never read it.
CREATE SCHEMA IF NOT EXISTS apiosk_private;
REVOKE ALL ON SCHEMA apiosk_private FROM PUBLIC, anon;
GRANT USAGE ON SCHEMA apiosk_private TO authenticated;

CREATE OR REPLACE FUNCTION apiosk_private.admin_apimarket_credential(p_save boolean, p_key text)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
DECLARE v_id uuid; v_updated_at timestamptz; v_key text;
BEGIN
  IF auth.uid() IS NULL OR NOT public.is_active_platform_admin() THEN
    RAISE EXCEPTION 'not_authorized' USING ERRCODE = '42501';
  END IF;
  IF p_save THEN
    v_key := btrim(p_key);
    IF v_key IS NULL OR length(v_key) < 8 OR length(v_key) > 4096 OR v_key ~ '[^!-~]' THEN
      RAISE EXCEPTION 'Enter a valid API.market key without spaces.' USING ERRCODE = '22023';
    END IF;
    PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtextextended('admin:APIMARKET_API_KEY', 0));
    SELECT id INTO v_id FROM vault.secrets WHERE name = 'APIMARKET_API_KEY';
    IF v_id IS NULL THEN
      PERFORM vault.create_secret(v_key, 'APIMARKET_API_KEY', 'API.market platform connection; stored by an authenticated platform admin');
    ELSE
      PERFORM vault.update_secret(v_id, v_key);
    END IF;
  END IF;
  SELECT id, updated_at INTO v_id, v_updated_at FROM vault.secrets WHERE name = 'APIMARKET_API_KEY';
  RETURN jsonb_build_object('configured', v_id IS NOT NULL, 'updated_at', v_updated_at);
END;
$$;
REVOKE ALL ON FUNCTION apiosk_private.admin_apimarket_credential(boolean,text) FROM PUBLIC, anon;
GRANT EXECUTE ON FUNCTION apiosk_private.admin_apimarket_credential(boolean,text) TO authenticated;

CREATE OR REPLACE FUNCTION public.admin_apimarket_key_status()
RETURNS jsonb LANGUAGE sql SECURITY INVOKER SET search_path = '' AS $$
  SELECT apiosk_private.admin_apimarket_credential(false, NULL);
$$;
CREATE OR REPLACE FUNCTION public.admin_save_apimarket_key(p_key text)
RETURNS jsonb LANGUAGE sql SECURITY INVOKER SET search_path = '' AS $$
  SELECT apiosk_private.admin_apimarket_credential(true, p_key);
$$;
REVOKE ALL ON FUNCTION public.admin_apimarket_key_status() FROM PUBLIC, anon;
REVOKE ALL ON FUNCTION public.admin_save_apimarket_key(text) FROM PUBLIC, anon;
GRANT EXECUTE ON FUNCTION public.admin_apimarket_key_status() TO authenticated;
GRANT EXECUTE ON FUNCTION public.admin_save_apimarket_key(text) TO authenticated;
COMMENT ON FUNCTION public.admin_save_apimarket_key(text) IS 'Admin-only write-only API.market key storage. Does not enable execution or return credentials.';
NOTIFY pgrst, 'reload schema';
