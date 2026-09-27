-- The logo check in providers.upsert_bazaar_batch used `{1,2040}`, and
-- PostgreSQL regular expressions cap a bound at 255 ("invalid repetition
-- count(s)"), so the first pilot batch failed before writing anything. Same
-- function, with the bound expressed as a length check instead.

CREATE OR REPLACE FUNCTION providers.upsert_bazaar_batch(p_batch jsonb)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
DECLARE
  v_control providers.bazaar_sync_control;
  v_owner uuid;
  v_run uuid;
  v_rows jsonb := coalesce(p_batch->'rows', '[]'::jsonb);
  v_touch jsonb := coalesce(p_batch->'touch', '[]'::jsonb);
  v_row jsonb; v_service jsonb; v_endpoint jsonb; v_contract jsonb;
  v_host text; v_method text; v_path text; v_slug text; v_price numeric; v_pay_to text; v_hex text;
  v_api uuid; v_endpoint_id uuid; v_prev_id uuid; v_prev_hash text; v_prev_active boolean;
  v_apis uuid[] := '{}';
  v_changed boolean;
  v_new integer;
  v_inserted integer := 0; v_updated integer := 0; v_unchanged integer := 0; v_staged integer := 0; v_touched integer := 0;
  v_skipped jsonb := '[]'::jsonb;
BEGIN
  PERFORM pg_advisory_xact_lock(hashtext('coinbase-bazaar-sync'));
  SELECT * INTO v_control FROM providers.bazaar_sync_control WHERE id;
  IF NOT coalesce(v_control.enabled, false) THEN RAISE EXCEPTION 'bazaar_sync_disabled' USING ERRCODE = 'P0001'; END IF;
  IF jsonb_typeof(v_rows) <> 'array' OR jsonb_typeof(v_touch) <> 'array' THEN RAISE EXCEPTION 'bazaar_batch_malformed' USING ERRCODE = '22023'; END IF;
  IF jsonb_array_length(v_rows) + jsonb_array_length(v_touch) > 200 THEN RAISE EXCEPTION 'bazaar_batch_too_large' USING ERRCODE = '22023'; END IF;
  v_run := coalesce(nullif(p_batch->>'run_id', '')::uuid, (SELECT id FROM providers.bazaar_sync_runs WHERE status = 'running'));
  IF v_run IS NULL OR NOT EXISTS (SELECT 1 FROM providers.bazaar_sync_runs WHERE id = v_run AND status = 'running') THEN
    RAISE EXCEPTION 'bazaar_run_not_running' USING ERRCODE = 'P0001';
  END IF;
  v_owner := providers.bazaar_owner();
  IF v_owner IS NULL THEN RAISE EXCEPTION 'bazaar_owner_missing' USING ERRCODE = 'P0001'; END IF;

  -- The approved ceiling holds whatever the caller claims its tier is.
  SELECT count(DISTINCT (r->>'host', r->>'method', r->>'path')) INTO v_new FROM jsonb_array_elements(v_rows) r
   WHERE NOT EXISTS (SELECT 1 FROM providers.bazaar_resources b
                      WHERE b.host = r->>'host' AND b.method = r->>'method' AND b.path = r->>'path' AND b.retired_at IS NULL);
  IF (SELECT count(*) FROM providers.bazaar_resources WHERE retired_at IS NULL) + v_new > v_control.max_tier THEN
    RAISE EXCEPTION 'bazaar_tier_exceeded (ceiling %)', v_control.max_tier USING ERRCODE = 'P0001';
  END IF;

  -- Unchanged resources seen again: they are live, their miss count resets.
  UPDATE providers.bazaar_resources b SET last_seen_run = v_run, last_seen_at = now(), missed_complete_runs = 0
    FROM jsonb_to_recordset(v_touch) AS t(host text, method text, path text)
   WHERE b.host = t.host AND b.method = t.method AND b.path = t.path AND b.retired_at IS NULL;
  GET DIAGNOSTICS v_touched = ROW_COUNT;

  IF jsonb_array_length(v_rows) > 0 THEN
    INSERT INTO catalog.fact_subject_kinds (slug, description)
    VALUES ('source_input', 'Endpoint-scoped declared inputs'), ('source_document', 'Opaque provider documents without normalized fact claims')
    ON CONFLICT DO NOTHING;
  END IF;

  FOR v_row IN SELECT value FROM jsonb_array_elements(v_rows) LOOP
    v_service := v_row->'service'; v_endpoint := v_row->'endpoint'; v_contract := v_row->'contract';
    v_host := v_row->>'host'; v_method := v_row->>'method'; v_path := v_row->>'path'; v_slug := v_service->>'slug';
    v_pay_to := lower(v_endpoint->>'pay_to');
    v_price := CASE WHEN v_endpoint->>'price_usd' ~ '^[0-9]{1,6}(\.[0-9]{1,6})?$' THEN (v_endpoint->>'price_usd')::numeric END;
    IF v_host IS NULL OR v_host !~ '^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$'
       OR v_host = 'apiosk.com' OR v_host LIKE '%.apiosk.com'
       OR v_method IS NULL OR v_method NOT IN ('GET', 'POST')
       OR v_path IS NULL OR v_path !~ '^/' OR v_path ~ '[?#{}]' OR v_path ~ '(^|/):' OR length(v_path) > 1024
       OR v_price IS NULL OR v_price <= 0 OR v_price > 1
       OR v_pay_to IS NULL OR v_pay_to !~ '^0x[0-9a-f]{40}$'
       OR v_slug IS NULL OR v_slug !~ '^cbz-[a-z0-9-]{1,96}$'
       OR nullif(trim(v_service->>'name'), '') IS NULL OR nullif(trim(v_endpoint->>'description'), '') IS NULL
       OR (v_row->>'content_hash') !~ '^[0-9a-f]{64}$'
       OR coalesce(v_service->>'origin_url', '') <> 'https://' || v_host
       OR (v_endpoint->>'id') !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' THEN
      v_skipped := v_skipped || jsonb_build_object('host', v_host, 'method', v_method, 'path', v_path, 'reason', 'invalid_row');
      CONTINUE;
    END IF;
    IF EXISTS (SELECT 1 FROM actions.x402_host_policy p WHERE p.host = v_host AND p.policy = 'deny') THEN
      v_skipped := v_skipped || jsonb_build_object('host', v_host, 'method', v_method, 'path', v_path, 'reason', 'host_policy_deny');
      CONTINUE;
    END IF;
    IF EXISTS (SELECT 1 FROM actions.x402_host_policy p WHERE p.host = v_host AND p.max_price_usdc IS NOT NULL AND v_price > p.max_price_usdc) THEN
      v_skipped := v_skipped || jsonb_build_object('host', v_host, 'method', v_method, 'path', v_path, 'reason', 'host_policy_price_cap');
      CONTINUE;
    END IF;
    IF EXISTS (SELECT 1 FROM providers.apis a WHERE a.slug NOT LIKE 'cbz-%'
                 AND v_host IN (lower(substring(a.origin_url FROM '^https?://([^/:?#]+)')),
                                lower(substring(a.endpoint_url FROM '^https?://([^/:?#]+)')))) THEN
      v_skipped := v_skipped || jsonb_build_object('host', v_host, 'method', v_method, 'path', v_path, 'reason', 'host_already_listed');
      CONTINUE;
    END IF;

    -- The service: one listing per host, owned by coinbase-bazaar only.
    v_api := NULL;
    INSERT INTO providers.apis AS a (owner_id, slug, name, origin_url, endpoint_url, wallet_address, status, is_verified,
      discoverable, category, listing_type, source, commission_rate, description, docs_url, website_url, is_external, listing_metadata)
    VALUES (v_owner, v_slug, left(v_service->>'name', 160), 'https://' || v_host, 'https://' || v_host, v_pay_to, 'pending', true,
      false, coalesce(nullif(v_service->>'category', ''), 'data'), 'federated', 'x402-manifest', 0, left(v_service->>'description', 600),
      'https://' || v_host, 'https://' || v_host, true,
      providers.bazaar_merge_metadata(NULL, jsonb_build_object(
        'source', 'bazaar-sync', 'provider_network', 'Coinbase Bazaar', 'name', left(v_service->>'name', 160),
        'logo', CASE WHEN v_service->>'logo' ~ '^https://[^[:space:]]+$' AND length(v_service->>'logo') <= 2048 THEN v_service->>'logo' END,
        'tags', coalesce(v_service->'tags', '[]'::jsonb),
        'bazaar', jsonb_build_object('host', v_host, 'first_seen', now(), 'last_seen', now(), 'run_id', v_run, 'pay_to', jsonb_build_array(v_pay_to)))))
    ON CONFLICT (slug) DO UPDATE SET
      name = excluded.name, description = excluded.description, category = excluded.category,
      origin_url = excluded.origin_url, endpoint_url = excluded.endpoint_url, docs_url = excluded.docs_url,
      website_url = excluded.website_url, listing_type = excluded.listing_type, is_external = true,
      listing_metadata = providers.bazaar_merge_metadata(a.listing_metadata, excluded.listing_metadata - 'bazaar'
        || jsonb_build_object('bazaar', (excluded.listing_metadata->'bazaar') - 'pay_to'))
      WHERE a.owner_id = v_owner
    RETURNING a.id INTO v_api;
    IF v_api IS NULL THEN
      v_skipped := v_skipped || jsonb_build_object('host', v_host, 'method', v_method, 'path', v_path, 'reason', 'slug_owned_by_another_provider');
      CONTINUE;
    END IF;
    IF NOT v_api = ANY (v_apis) THEN v_apis := v_apis || v_api; END IF;

    -- The endpoint, keyed (api, method, path). Rewritten only when the synced
    -- content changed, the endpoint was retired, or it is new.
    SELECT e.id, b.content_hash, e.active INTO v_prev_id, v_prev_hash, v_prev_active
      FROM providers.api_endpoints e
      LEFT JOIN providers.bazaar_resources b ON b.host = v_host AND b.method = v_method AND b.path = v_path AND b.retired_at IS NULL
     WHERE e.api_id = v_api AND e.method = v_method AND e.path = v_path;
    IF NOT FOUND THEN
      INSERT INTO providers.api_endpoints (id, api_id, method, path, payment_required, price, network_token_id, description,
        query_params, request_body, response_example, active, tags, endpoint_category, action_type, metadata_source,
        metadata_at, contract_documented)
      VALUES ((v_endpoint->>'id')::uuid, v_api, v_method, v_path, true, v_price, '26adabc5-92ec-44f3-9540-3bd636eeb1f4',
        left(v_endpoint->>'description', 2000), nullif(v_endpoint->'query_params', 'null'::jsonb), nullif(v_endpoint->'request_body', 'null'::jsonb),
        nullif(v_endpoint->'response_example', 'null'::jsonb), true,
        ARRAY(SELECT left(t, 80) FROM jsonb_array_elements_text(coalesce(v_endpoint->'tags', '[]'::jsonb)) t LIMIT 8),
        v_endpoint->>'endpoint_category', v_endpoint->>'action_type', 'declared', now(), true)
      RETURNING id INTO v_endpoint_id;
      v_inserted := v_inserted + 1; v_changed := true;
    ELSIF v_prev_hash IS DISTINCT FROM v_row->>'content_hash' OR NOT coalesce(v_prev_active, true) THEN
      UPDATE providers.api_endpoints SET payment_required = true, price = v_price,
        network_token_id = '26adabc5-92ec-44f3-9540-3bd636eeb1f4', description = left(v_endpoint->>'description', 2000),
        query_params = nullif(v_endpoint->'query_params', 'null'::jsonb), request_body = nullif(v_endpoint->'request_body', 'null'::jsonb),
        response_example = nullif(v_endpoint->'response_example', 'null'::jsonb), active = true,
        tags = ARRAY(SELECT left(t, 80) FROM jsonb_array_elements_text(coalesce(v_endpoint->'tags', '[]'::jsonb)) t LIMIT 8),
        endpoint_category = v_endpoint->>'endpoint_category', action_type = v_endpoint->>'action_type',
        metadata_source = 'declared', metadata_at = now(), contract_documented = true
       WHERE id = v_prev_id;
      v_endpoint_id := v_prev_id; v_updated := v_updated + 1; v_changed := true;
    ELSE
      v_endpoint_id := v_prev_id; v_unchanged := v_unchanged + 1; v_changed := false;
    END IF;

    INSERT INTO providers.bazaar_resources AS b (host, method, path, resource, api_id, endpoint_id, content_hash, pay_to, price_usd, first_seen_run, last_seen_run)
    VALUES (v_host, v_method, v_path, left(v_row->>'resource', 2048), v_api, v_endpoint_id, v_row->>'content_hash', v_pay_to, v_price, v_run, v_run)
    ON CONFLICT (host, method, path) DO UPDATE SET resource = excluded.resource, api_id = excluded.api_id, endpoint_id = excluded.endpoint_id,
      content_hash = excluded.content_hash, pay_to = excluded.pay_to, price_usd = excluded.price_usd, last_seen_run = v_run,
      last_seen_at = now(), missed_complete_runs = 0, retired_at = NULL;

    -- The staged document contract. Only for the endpoint id it was built for:
    -- its capability and vocabulary slugs are derived from that id.
    IF v_changed AND jsonb_typeof(v_contract) = 'object' THEN
      v_hex := replace(v_endpoint_id::text, '-', '');
      IF v_endpoint_id <> (v_endpoint->>'id')::uuid THEN
        v_skipped := v_skipped || jsonb_build_object('host', v_host, 'method', v_method, 'path', v_path, 'reason', 'endpoint_id_mismatch');
      ELSIF v_contract->>'capability_slug' !~ ('^operation\.cbz-[a-z0-9-]+\.(get|post)-[a-z0-9-]*-' || left(v_hex, 8) || '$')
         OR EXISTS (SELECT 1 FROM jsonb_array_elements(coalesce(v_contract->'vocabulary', '[]'::jsonb)) w
                     WHERE w->>'slug' NOT LIKE 'source_input.e' || v_hex || '.%' AND w->>'slug' <> 'source_document.e' || v_hex || '.response'
                        OR w->>'subject_kind' NOT IN ('source_input', 'source_document') OR jsonb_typeof(w->'value_schema') <> 'object') THEN
        v_skipped := v_skipped || jsonb_build_object('host', v_host, 'method', v_method, 'path', v_path, 'reason', 'contract_rejected');
      ELSE
        INSERT INTO catalog.fact_types (slug, name, description, subject_kind, value_schema, status)
        SELECT w->>'slug', left(w->>'description', 500), left(w->>'description', 500), w->>'subject_kind', w->'value_schema', 'active'
          FROM jsonb_array_elements(v_contract->'vocabulary') w
        ON CONFLICT (slug) DO UPDATE SET name = excluded.name, description = excluded.description,
          value_schema = excluded.value_schema, status = 'active', updated_at = now();
        INSERT INTO catalog.capabilities (slug, name, description, category, input_contract, output_contract, status)
        VALUES (v_contract->>'capability_slug', left(v_contract->>'description', 180), left(v_contract->>'description', 2000),
                'source_document', '{}'::jsonb, '{}'::jsonb, 'active')
        ON CONFLICT (slug) DO UPDATE SET name = excluded.name, description = excluded.description, updated_at = now();
        INSERT INTO catalog.capability_offers (capability_slug, endpoint_id, api_id, source, confidence, input_map, fixed_params, notes)
        VALUES (v_contract->>'capability_slug', v_endpoint_id, v_api, 'declared', 1,
                coalesce((SELECT jsonb_object_agg(m.value->>'name', m.value) FROM jsonb_each(v_contract->'input_map') m), '{}'::jsonb),
                '{}'::jsonb, 'Provider-declared input schema from the Coinbase Bazaar listing')
        ON CONFLICT (capability_slug, endpoint_id) DO UPDATE SET input_map = excluded.input_map, notes = excluded.notes, updated_at = now();
        INSERT INTO catalog.capability_execution_contracts AS c (id, capability_slug, endpoint_id, version, status, requires, produces,
          input_map, fixed_params, output_map, cardinality, identifies, max_fan_out, preconditions, pagination, effect, price_model,
          freshness_seconds, sample_payload, validated_at, validated_note, notes, adapter_version)
        VALUES ((v_contract->>'id')::uuid, v_contract->>'capability_slug', v_endpoint_id, 1, 'validated', v_contract->'requires',
          v_contract->'produces', v_contract->'input_map', '{}'::jsonb, v_contract->'output_map', 'one', NULL, 1, '{}'::jsonb, '{}'::jsonb,
          'read', 'per_call', 3600, '{"data":"Synthetic document-adapter fixture; not a provider response or live execution."}'::jsonb,
          now(), 'Staged from the provider-declared Coinbase Bazaar schema; NOT proof of live execution',
          'source-document-v1 (bazaar-sync); omitted optional inputs: ' || coalesce((SELECT string_agg(x, ', ') FROM jsonb_array_elements_text(v_contract->'omitted_optional_inputs') x), ''),
          'source-document-v1')
        ON CONFLICT (id) DO UPDATE SET requires = excluded.requires, produces = excluded.produces, input_map = excluded.input_map,
          output_map = excluded.output_map, status = 'validated', sample_payload = excluded.sample_payload, validated_at = now(),
          validated_note = 'Re-staged: the provider changed its declared Coinbase Bazaar schema', updated_at = now()
          WHERE c.adapter_version = 'source-document-v1' AND c.endpoint_id = excluded.endpoint_id
            AND (c.requires, c.input_map, c.output_map) IS DISTINCT FROM (excluded.requires, excluded.input_map, excluded.output_map);
        v_staged := v_staged + 1;
      END IF;
    ELSIF v_changed THEN
      -- The declared schema no longer yields a contract: withdraw the old one.
      UPDATE catalog.capability_execution_contracts SET status = 'draft', updated_at = now(),
        validated_note = 'Withdrawn: the provider''s declared Coinbase Bazaar schema no longer maps to a contract'
       WHERE endpoint_id = v_endpoint_id AND adapter_version = 'source-document-v1' AND status IN ('validated', 'active');
    END IF;
  END LOOP;

  -- The payees the resell guard accepts are exactly the live ledger's.
  UPDATE providers.apis a SET
    listing_metadata = jsonb_set(a.listing_metadata, '{bazaar,pay_to}', coalesce((SELECT jsonb_agg(DISTINCT b.pay_to ORDER BY b.pay_to)
      FROM providers.bazaar_resources b WHERE b.api_id = a.id AND b.retired_at IS NULL), '[]'::jsonb)),
    wallet_address = coalesce((SELECT min(b.pay_to) FROM providers.bazaar_resources b WHERE b.api_id = a.id AND b.retired_at IS NULL), a.wallet_address)
   WHERE a.id = ANY (v_apis) AND a.owner_id = v_owner;

  UPDATE providers.bazaar_sync_runs SET batches = batches + 1, inserted_endpoints = inserted_endpoints + v_inserted,
    updated_endpoints = updated_endpoints + v_updated, staged_contracts = staged_contracts + v_staged,
    skipped_rows = skipped_rows + jsonb_array_length(v_skipped)
   WHERE id = v_run;
  RETURN jsonb_build_object('run_id', v_run, 'services', cardinality(v_apis), 'inserted', v_inserted, 'updated', v_updated,
    'unchanged', v_unchanged, 'touched', v_touched, 'staged_contracts', v_staged, 'skipped', v_skipped);
END $$;
