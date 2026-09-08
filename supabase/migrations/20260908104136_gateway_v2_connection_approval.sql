-- Reuse the spending mandate granted when connecting a chatbot. This read
-- does not reserve or debit money; reserve_call_within_limits remains the
-- atomic ledger authority at dispatch, including concurrent spending.
CREATE OR REPLACE FUNCTION gateway_v2.connection_budget(p_owner uuid, p_connection uuid)
RETURNS TABLE(per_call_atomic bigint, daily_remaining_atomic bigint, balance_atomic bigint)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
 SELECT c.per_call_limit_micro_usd,
        greatest(0, c.daily_limit_micro_usd
          - coalesce((SELECT sum(b.charged_micro_usd) FROM public.balance_charges b
              WHERE b.user_id=p_owner AND b.metadata->>'connection_id'=p_connection::text
                AND b.created_at>=now()-interval '24 hours' AND b.job_id IS NULL
                AND b.status<>'refunded'),0)
          - coalesce((SELECT sum(CASE WHEN j.created_at>=now()-interval '24 hours'
                THEN j.spent_micro_usd+j.reserved_micro_usd ELSE j.reserved_micro_usd END)
              FROM public.job_budget_reservations j WHERE j.user_id=p_owner
                AND j.connection_id=p_connection
                AND (j.created_at>=now()-interval '24 hours' OR j.status='open')),0))::bigint,
        coalesce(u.balance_micro_usd,0)
 FROM public.agent_connections c LEFT JOIN public.user_balances u ON u.user_id=c.user_id
 WHERE c.id=p_connection AND c.user_id=p_owner AND c.revoked_at IS NULL
   AND c.per_call_limit_micro_usd>0 AND c.daily_limit_micro_usd>0
$$;
REVOKE ALL ON FUNCTION gateway_v2.connection_budget(uuid,uuid) FROM PUBLIC,anon,authenticated;
GRANT EXECUTE ON FUNCTION gateway_v2.connection_budget(uuid,uuid) TO gateway_v2_runtime;

ALTER TABLE gateway_v2.authorizations
 ADD COLUMN approved_via text NOT NULL DEFAULT 'person_app'
   CHECK (approved_via IN ('person_app','connection_budget')),
 ADD COLUMN approved_connection_id uuid,
 ADD CONSTRAINT authorization_origin CHECK
   ((approved_via='person_app' AND approved_connection_id IS NULL)
     OR (approved_via='connection_budget' AND approved_connection_id IS NOT NULL));
COMMENT ON COLUMN gateway_v2.authorizations.approved_via IS
 'connection_budget records a use of the existing OAuth spending mandate, not verified human presence. The app-only MCP button is a host UI affordance, not an authentication credential.';
