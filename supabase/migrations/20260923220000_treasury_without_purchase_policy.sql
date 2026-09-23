-- The treasury connection ("Apiosk MCP agent", token 3b8f8a04) pays providers
-- for every resale and every operator procurement call. Its purchase policy
-- ("Apiosk MCP agent rules 3") capped it at 2000 calls and $25 per day, $750
-- per month, $5 per transaction and held anything above $2.50 for approval,
-- which throttles every customer at once. The treasury runs without a policy;
-- customers' own connection limits and balances still apply. Re-enable in the
-- buyer portal under Rules if a cap is wanted again.
update billing.purchase_policies
   set enabled = false, updated_at = now()
 where id = '23eb4543-2675-41c8-ba7a-413a6a619b4d' and enabled;
