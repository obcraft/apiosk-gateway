-- Owners remove a trigger and clear trigger activity from the App. Removing a
-- trigger also deletes the Slack dossiers it started, in the same transaction;
-- events cascade with the trigger. Monitoring schedules are separate and stay.
GRANT DELETE ON gateway_v2.inbound_triggers, gateway_v2.inbound_trigger_events, gateway_v2.slack_dossiers TO gateway_v2_runtime;
