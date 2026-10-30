-- Additive: old inbound events and existing gateway-only RLS/grants remain intact.
ALTER TABLE gateway_v2.inbound_trigger_events ADD COLUMN activity jsonb
    CHECK (activity IS NULL OR (jsonb_typeof(activity) = 'object' AND octet_length(activity::text) <= 32000));
