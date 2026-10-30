ALTER TABLE gateway_v2.inbound_triggers
    ADD COLUMN approver_id text,
    ADD COLUMN approver_name text,
    ADD CONSTRAINT inbound_triggers_approver CHECK (
        (approver_id IS NULL AND approver_name IS NULL)
        OR (source='slack' AND approver_id IS NOT NULL AND approver_id ~ '^[UW][A-Z0-9]{2,23}$'
            AND approver_name IS NOT NULL AND length(approver_name) BETWEEN 1 AND 120)
    );
