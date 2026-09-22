-- A settlement that will never confirm on its own needs an outcome.
--
-- `billing.reconciliation_stuck_settlements` reports every settlement that is
-- not `settled`, forever. That was right while a failure always meant "a retry
-- is still possible", and wrong once a failure became permanent: 35 rows from
-- 31 August to 6 September, when the treasury had no gas for the direct
-- transfer rail, have been re-reported every five minutes ever since. An alert
-- that has cried wolf for weeks hides the next real one, which is the whole
-- reason the check exists (audit K3).
--
-- So a settlement can now carry what was decided about it. Nothing here moves
-- money or changes an amount: `paid` records a payout somebody made, with its
-- transaction hash, `written_off` records a decision not to pay, and
-- `superseded` records that another settlement covered the same request. The
-- view reports what is still open, so the worker's exit code means "somebody
-- has to look at this" again.

alter table billing.settlements
  add column if not exists resolution text
    check (resolution is null or resolution in ('paid','written_off','superseded')),
  add column if not exists resolved_at timestamptz,
  add column if not exists resolution_note text
    check (resolution_note is null or length(resolution_note) between 1 and 500),
  add column if not exists resolution_tx_hash text
    check (resolution_tx_hash is null or resolution_tx_hash ~ '^0x[0-9a-fA-F]{64}$');

-- An outcome is a fact with a time and a reason, or it is not recorded.
alter table billing.settlements drop constraint if exists settlements_resolution_complete;
alter table billing.settlements add constraint settlements_resolution_complete
  check ((resolution is null) = (resolved_at is null)
     and (resolution is null) = (resolution_note is null)
     and (resolution_tx_hash is null or resolution = 'paid'));

create index if not exists settlements_open_failures
  on billing.settlements(occurred_at desc) where settlement_status <> 'settled' and resolution is null;

-- Same columns as before. Only rows nobody has dealt with are reported.
create or replace view billing.reconciliation_stuck_settlements as
 select id as settlement_id,
    api_id,
    endpoint_id,
    request_id,
    tx_hash,
    onchain_tx_hash,
    settlement_status,
    settlement_error,
    gross_amount,
    developer_amount,
    wallet_address,
    occurred_at
   from billing.settlements s
  where settlement_status <> 'settled'
    and resolution is null
    and occurred_at < (now() - '00:05:00'::interval);

-- What is still owed, per API, for an operator deciding what to pay.
create or replace view billing.reconciliation_open_payouts as
 select api_id,
    count(*) as calls,
    sum(gross_amount) as gross_amount,
    sum(developer_amount) as developer_amount,
    min(occurred_at) as oldest,
    max(occurred_at) as newest
   from billing.settlements
  where settlement_status <> 'settled' and resolution is null
  group by api_id;

-- Recording an outcome is a write on the money table, so it goes through one
-- function: service role only, one settlement at a time, never twice, and it
-- cannot touch an amount or a settled row.
create or replace function billing.resolve_settlement(
  p_settlement bigint,
  p_resolution text,
  p_note text,
  p_tx_hash text default null
) returns jsonb
language plpgsql security definer set search_path='' as $$
declare s billing.settlements%rowtype;
begin
  select * into s from billing.settlements where id=p_settlement for update;
  if not found then raise exception 'settlement_not_found' using errcode='P0002'; end if;
  if s.settlement_status='settled' then raise exception 'settlement_already_settled' using errcode='23514'; end if;
  if s.resolution is not null then
    return jsonb_build_object('settlement_id',s.id,'resolution',s.resolution,'resolved_at',s.resolved_at,'already_resolved',true);
  end if;
  update billing.settlements
     set resolution=p_resolution, resolved_at=now(), resolution_note=p_note, resolution_tx_hash=p_tx_hash
   where id=p_settlement
   returning * into s;
  return jsonb_build_object('settlement_id',s.id,'resolution',s.resolution,'resolved_at',s.resolved_at,
    'developer_amount',s.developer_amount::text,'already_resolved',false);
end $$;
revoke all on function billing.resolve_settlement(bigint,text,text,text) from public,anon,authenticated;
grant execute on function billing.resolve_settlement(bigint,text,text,text) to service_role;
