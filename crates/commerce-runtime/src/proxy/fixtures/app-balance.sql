-- Local disposable database only. No network-token or payer table is present:
-- ordinary API execution must not need either one.
CREATE TABLE agent_wallet_connect_tokens (
 id uuid PRIMARY KEY, user_id uuid, token_prefix text, token_name text,
 expires_at timestamptz, revoked_at timestamptz, allowed_domains text[], policy_id uuid
);
INSERT INTO agent_wallet_connect_tokens VALUES
 ('00000000-0000-0000-0000-000000000001','00000000-0000-0000-0000-000000000007',
 'test','local fixture',now()+interval '1 hour',NULL,'{}','00000000-0000-0000-0000-000000000008');
CREATE TABLE agent_wallet_connect_token_wallets (token_id uuid, wallet_id uuid, daily_limit_usdc numeric, per_tx_limit_usdc numeric);
CREATE TABLE agent_wallets (id uuid, wallet_address text, status text, daily_limit_usdc numeric, per_tx_limit_usdc numeric, created_at timestamptz);
CREATE TABLE apis (
 id uuid, slug text, origin_url text, wallet_address text, is_platform_owned boolean,
 settlement_mode text, requires_payout_wallet boolean, listing_type text, listing_metadata jsonb,
 status text, owner_id uuid, commission_rate numeric, owner_wallet text, endpoint_url text,
 is_community boolean, active boolean, is_verified boolean, hmac_signing_algorithm text,
 hmac_signing_header text, hmac_signing_secret_enc bytea, hmac_signing_payload_format text
);
INSERT INTO apis (id,slug,origin_url,is_platform_owned,settlement_mode,requires_payout_wallet,listing_type,listing_metadata,status,owner_id,commission_rate,active,is_verified)
 VALUES ('00000000-0000-0000-0000-000000000002','balance-fixture','https://fixture.example',true,'apiosk_revenue',false,'api','{}','active','00000000-0000-0000-0000-000000000007',0.2174,true,true);
CREATE TABLE settlements (api_id uuid, endpoint_id uuid, tx_hash text UNIQUE, amount numeric, gross_amount numeric, commission_rate numeric, commission_amount numeric, developer_amount numeric, network_token_id uuid, wallet_address text, request_id uuid, session_id uuid, managed_wallet_id uuid, client_id text, settlement_status text);
CREATE TABLE developer_balances (owner_id uuid, api_id uuid, amount numeric, tx_hash text, entry_type text, description text, network_token_id uuid);
CREATE TABLE gateway_usage (api_id uuid, endpoint_id uuid, paid boolean, amount numeric, network_token_id uuid, wallet_address text, request_id uuid, session_id uuid, managed_wallet_id uuid, client_id text, agent_connect_token_id uuid, occurred_at timestamptz DEFAULT now());

CREATE TABLE gateway_idempotency_keys (token_id uuid, key text, request_fingerprint text, response_status smallint, response_headers jsonb, response_body bytea, created_at timestamptz DEFAULT now(), PRIMARY KEY(token_id,key));
