-- A review recipient is independent from automatic approval requests.
ALTER TABLE gateway_v2.inbound_triggers
    ADD COLUMN approval_mode text NOT NULL DEFAULT 'on_request';

-- Keep the behavior of previously configured approval triggers.
UPDATE gateway_v2.inbound_triggers SET approval_mode='automatic'
WHERE approver_id IS NOT NULL;

ALTER TABLE gateway_v2.inbound_triggers
    ADD CONSTRAINT inbound_triggers_approval_mode CHECK (
        approval_mode IN ('on_request', 'automatic')
        AND (approval_mode='on_request' OR (source='slack' AND approver_id IS NOT NULL))
    );
