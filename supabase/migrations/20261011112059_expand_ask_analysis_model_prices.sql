-- User requested a maximum EUR 2 add-on. Compact Astra reserves at most
-- USD 1.9895 including the fee (approximately EUR 1.83 at the current App rate).
-- Each of the twelve explicitly selected models retains its own lower cap.
-- Only the pilot's existing allow policy changes. No automatic purchase occurs.
UPDATE actions.x402_host_policy
SET max_price_usdc = 1.730000,
    note = 'Ask optional analysis: 12 server-allowlisted models; per-model provider caps $0.015-$1.73, buyer caps include 15% fee; explicit approval required'
WHERE host = 'x402engine.app' AND policy = 'allow' AND max_price_usdc = 1.000000;
