-- A request cancelled inside a transaction (client gone, timeout) leaves sqlx
-- to send ROLLBACK only when that pooled connection is used again. Until then
-- the transaction stays "idle in transaction" and keeps its row locks. On
-- 2026-09-23 one held agents.agent_connections for over seven minutes and every
-- organisation API key check (use_organisation_api_key) timed out behind it:
-- 503 authentication_unavailable.
--
-- Gateway transactions do only milliseconds of work between statements and
-- never wait on the network, so Postgres may end any that sit idle longer.
ALTER ROLE gateway_v2_server SET idle_in_transaction_session_timeout = '60s';
