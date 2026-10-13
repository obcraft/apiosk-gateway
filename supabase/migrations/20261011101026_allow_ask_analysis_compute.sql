-- Fixed Ask compute destination. Individual requests still require an exact
-- signed plan, one approval, the App balance reservation and x402 verification.
-- Preserve any explicit operator policy already present.
INSERT INTO actions.x402_host_policy(host,policy,max_price_usdc,note)
VALUES ('x402engine.app','allow',1.000000,'Ask optional Claude Opus 5.5 analysis; provider cap $1, buyer cap $1.15; explicit approval required')
ON CONFLICT(host) DO NOTHING;
