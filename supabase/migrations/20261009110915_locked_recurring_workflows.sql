-- A quote owns an immutable, versioned recipe. Ongoing mandates reserve a whole
-- occurrence atomically; every call still settles through the existing ledger.
alter table billing.recurring_orders
 add column billing_mode text not null default 'prepaid' check(billing_mode in ('prepaid','per_run')),
 add column workflow jsonb,
 add column change_only boolean not null default false,
 add column baseline_hash text,
 add column baseline_at timestamptz;
alter table billing.recurring_runs add column funded_micro_usd bigint not null default 0 check(funded_micro_usd>=0);
update billing.recurring_runs r set funded_micro_usd=o.total_per_run from billing.recurring_orders o where o.id=r.order_id;
-- Existing mandates need a new reviewed recipe. Preserve all financial holds.
update billing.recurring_orders set state='attention' where state in ('active','paused');
do $$ declare c record; begin
 for c in select conname from pg_constraint where conrelid='billing.recurring_orders'::regclass and contype='c'
 and (pg_get_constraintdef(oid) like '%approved_micro_usd > 0%' or pg_get_constraintdef(oid) like '%approved_micro_usd = (total_per_run * cardinality(dates))%') loop
  execute format('alter table billing.recurring_orders drop constraint %I',c.conname);
 end loop;
 for c in select conname from pg_constraint where conrelid='billing.recurring_runs'::regclass and contype='c' and pg_get_constraintdef(oid) like '%state%' loop
  execute format('alter table billing.recurring_runs drop constraint %I',c.conname);
 end loop;
 for c in select conname from pg_constraint where conrelid='billing.recurring_deliveries'::regclass and contype='c' and pg_get_constraintdef(oid) like '%state%' loop
  execute format('alter table billing.recurring_deliveries drop constraint %I',c.conname);
 end loop;
end $$;
alter table billing.recurring_orders add constraint recurring_approved_nonnegative check(approved_micro_usd>=0);
alter table billing.recurring_orders add constraint recurring_prepaid_total check(billing_mode='per_run' or approved_micro_usd=total_per_run*cardinality(dates));
alter table billing.recurring_runs add constraint recurring_run_state check(state in ('queued','running','succeeded','failed','unresolved','skipped','skipped_insufficient_balance','cancelled'));
alter table billing.recurring_deliveries add constraint recurring_delivery_state check(state in ('queued','sending','sent','failed','uncertain','cancelled'));

create function billing.guard_recurring_recipe() returns trigger language plpgsql set search_path='' as $$
begin
 if old.approved_at is not null and
  (new.approved_at,new.owner_id,new.workspace_id,new.question,new.timezone,new.configuration,new.sources,new.delivery,new.dates,new.total_per_run,new.provider_per_run,new.billing_mode,new.workflow,new.change_only)
  is distinct from
  (old.approved_at,old.owner_id,old.workspace_id,old.question,old.timezone,old.configuration,old.sources,old.delivery,old.dates,old.total_per_run,old.provider_per_run,old.billing_mode,old.workflow,old.change_only)
 then raise exception 'approved_workflow_immutable'; end if;
 if old.approved_at is null and new.approved_at is not null and (new.workflow is null or new.workflow->>'fingerprint' is null) then
  raise exception 'locked_workflow_required';
 end if;
 return new;
end $$;
create trigger recurring_recipe_immutable before update on billing.recurring_orders for each row execute function billing.guard_recurring_recipe();

create function billing.approve_locked_recurring_order(p_owner uuid,p_id uuid,p_total bigint,p_cap bigint,p_hash text) returns void
language plpgsql security definer set search_path='' as $$
declare s billing.recurring_orders%rowtype; available bigint; required bigint;
begin
 select * into strict s from billing.recurring_orders where id=p_id and owner_id=p_owner;
 perform tenancy.verified_actor(p_owner);
 if s.workspace_id is not null then
  perform 1 from tenancy.organisations o join tenancy.workspaces w on w.organisation_id=o.id where w.id=s.workspace_id for update of o;
  if not tenancy.workspace_permission(p_owner,s.workspace_id,'spend') then raise exception 'forbidden'; end if;
 else perform tenancy.reservation_account(p_owner,null,p_cap); end if;
 select balance_micro_usd into available from billing.user_balances where account_id=coalesce(s.workspace_id,p_owner) and reversal_debt_micro_usd=0 for update;
 select * into strict s from billing.recurring_orders where id=p_id and owner_id=p_owner for update;
 required:=case when s.billing_mode='prepaid' then s.approved_micro_usd else 0 end;
 if p_total<>required or p_cap<>s.total_per_run or s.workflow is null or s.workflow->>'fingerprint' is distinct from p_hash then raise exception 'quote_changed'; end if;
 if s.approved_at is not null then return; end if;
 if s.state<>'draft' or s.expires_at<=now() or s.dates[1]<=now() then raise exception 'quote_expired'; end if;
 if available is null or available<required then raise exception 'insufficient_balance'; end if;
 update billing.user_balances set balance_micro_usd=balance_micro_usd-required where account_id=coalesce(s.workspace_id,p_owner);
 update billing.recurring_orders set state='active',approved_at=now(),reserved_micro_usd=required,updated_at=now() where id=p_id;
 insert into billing.recurring_runs(order_id,scheduled_at,funded_micro_usd)
 select p_id,d,case when s.billing_mode='prepaid' then s.total_per_run else 0 end from unnest(s.dates) d;
end $$;

create function billing.fund_recurring_run(p_run uuid) returns boolean
language plpgsql security definer set search_path='' as $$
declare s billing.recurring_orders%rowtype; r billing.recurring_runs%rowtype; available bigint;
begin
 select o.* into strict s from billing.recurring_orders o join billing.recurring_runs rr on rr.order_id=o.id where rr.id=p_run;
 perform tenancy.verified_actor(s.owner_id);
 if s.workspace_id is not null then
  perform 1 from tenancy.organisations o join tenancy.workspaces w on w.organisation_id=o.id where w.id=s.workspace_id for update of o;
  if not tenancy.workspace_permission(s.owner_id,s.workspace_id,'spend') then raise exception 'forbidden'; end if;
 else perform tenancy.reservation_account(s.owner_id,null,s.total_per_run); end if;
 select balance_micro_usd into available from billing.user_balances where account_id=coalesce(s.workspace_id,s.owner_id) and reversal_debt_micro_usd=0 for update;
 select * into strict s from billing.recurring_orders where id=s.id for update;
 select * into strict r from billing.recurring_runs where id=p_run for update;
 if s.state<>'active' or s.workflow is null or r.state<>'running' or r.finished_at is not null then raise exception 'recurring_order_inactive'; end if;
 if r.funded_micro_usd=s.total_per_run then return true; end if;
 if s.billing_mode<>'per_run' or r.funded_micro_usd<>0 then raise exception 'invalid_run_funding'; end if;
 if available is null or available<s.total_per_run then return false; end if;
 update billing.user_balances set balance_micro_usd=balance_micro_usd-s.total_per_run where account_id=coalesce(s.workspace_id,s.owner_id);
 update billing.recurring_orders set approved_micro_usd=approved_micro_usd+s.total_per_run,reserved_micro_usd=reserved_micro_usd+s.total_per_run,updated_at=now() where id=s.id;
 update billing.recurring_runs set funded_micro_usd=s.total_per_run where id=p_run;
 return true;
end $$;

create or replace function billing.finish_recurring_run(p_run uuid,p_state text,p_result jsonb,p_error text) returns void
language plpgsql security definer set search_path='' as $$
declare s billing.recurring_orders%rowtype; r billing.recurring_runs%rowtype; release bigint; changed boolean:=true; fingerprint text;
begin
 if p_state not in ('succeeded','failed','unresolved','skipped','skipped_insufficient_balance','cancelled') then raise exception 'invalid_state'; end if;
 select o.* into strict s from billing.recurring_orders o join billing.recurring_runs rr on rr.order_id=o.id where rr.id=p_run;
 perform 1 from billing.user_balances where account_id=coalesce(s.workspace_id,s.owner_id) for update;
 select * into strict s from billing.recurring_orders where id=s.id for update;
 select * into strict r from billing.recurring_runs where id=p_run for update;
 if r.finished_at is not null then return; end if;
 release:=r.funded_micro_usd-coalesce((select sum(allocated_micro_usd) from billing.recurring_calls where run_id=p_run),0);
 if release<0 or release>s.reserved_micro_usd then raise exception 'invalid_run_reservation'; end if;
 update billing.user_balances set balance_micro_usd=balance_micro_usd+release where account_id=coalesce(s.workspace_id,s.owner_id);
 update billing.recurring_orders set reserved_micro_usd=reserved_micro_usd-release,released_micro_usd=released_micro_usd+release,updated_at=now() where id=s.id;
 update billing.recurring_runs set state=p_state,result=p_result,error=p_error,finished_at=now(),released_micro_usd=release,lease_until=null where id=p_run;
 if p_state='unresolved' then update billing.recurring_orders set state='attention' where id=s.id and state='active'; end if;
 if p_state='succeeded' then
  fingerprint:=p_result->>'comparison_hash';
  if fingerprint is null then raise exception 'missing_result_fingerprint'; end if;
  changed:=s.baseline_hash is distinct from fingerprint;
  -- Occurrences of one order execute in order; the timestamp check also protects
  -- baseline ordering during recovery of an old lease.
  if s.baseline_at is null or r.scheduled_at>s.baseline_at then
   update billing.recurring_orders set baseline_hash=fingerprint,baseline_at=r.scheduled_at where id=s.id;
  end if;
 end if;
 if s.state<>'cancelled' and (p_state='failed' or (p_state='succeeded' and (not s.change_only or changed))) then
  insert into billing.recurring_deliveries(run_id) values(p_run) on conflict do nothing;
 end if;
 update billing.recurring_orders set state='completed' where id=s.id and billing_mode='prepaid' and state in ('active','paused')
 and not exists(select 1 from billing.recurring_runs where order_id=s.id and finished_at is null);
end $$;

-- The worker creates a selection and binds its workspace in the same transaction.
-- Acquire the same organisation -> account -> order locks as approval/funding
-- before workspace_bind_execution, without broad table privileges for runtime.
create function billing.lock_recurring_order_for_call(p_owner uuid,p_id uuid) returns text
language plpgsql security definer set search_path='' as $$
declare s billing.recurring_orders%rowtype;
begin
 select * into strict s from billing.recurring_orders where id=p_id and owner_id=p_owner;
 if s.workspace_id is not null then
  perform 1 from tenancy.organisations o join tenancy.workspaces w on w.organisation_id=o.id where w.id=s.workspace_id for update of o;
 end if;
 perform 1 from billing.user_balances where account_id=coalesce(s.workspace_id,p_owner) for update;
 select * into strict s from billing.recurring_orders where id=p_id and owner_id=p_owner for update;
 return s.state;
end $$;

-- Revocation must never trap future funds. Pausing/cancelling remains possible
-- for the original owner; resuming continues to require current spending rights.
create or replace function billing.control_recurring_order(p_owner uuid,p_id uuid,p_state text) returns void
language plpgsql security definer set search_path='' as $$
declare s billing.recurring_orders%rowtype; r record;
begin
 if p_state not in ('paused','active','cancelled') then raise exception 'invalid_state'; end if;
 select * into strict s from billing.recurring_orders where id=p_id and owner_id=p_owner;
 perform tenancy.verified_actor(p_owner);
 if p_state='active' and s.workspace_id is not null and not tenancy.workspace_permission(p_owner,s.workspace_id,'spend') then raise exception 'forbidden'; end if;
 perform 1 from billing.user_balances where account_id=coalesce(s.workspace_id,p_owner) for update;
 select * into strict s from billing.recurring_orders where id=p_id for update;
 if s.state=p_state then return; end if;
 if s.state in ('draft','cancelled') or (s.state='completed' and p_state<>'cancelled') or (s.state='attention' and p_state='active') then raise exception 'invalid_transition'; end if;
 update billing.recurring_orders set state=p_state,updated_at=now() where id=p_id;
 if p_state='cancelled' then
  update billing.recurring_deliveries d set state='cancelled',error='Delivery cancelled with the recurring order.'
  from billing.recurring_runs rr where d.run_id=rr.id and rr.order_id=p_id and d.state='queued';
  for r in select id from billing.recurring_runs where order_id=p_id and state='queued' order by scheduled_at loop
   perform billing.finish_recurring_run(r.id,'cancelled',null,null);
  end loop;
 end if;
end $$;

revoke all on function billing.guard_recurring_recipe(),billing.approve_locked_recurring_order(uuid,uuid,bigint,bigint,text),billing.fund_recurring_run(uuid),billing.lock_recurring_order_for_call(uuid,uuid) from public,anon,authenticated;
grant execute on function billing.approve_locked_recurring_order(uuid,uuid,bigint,bigint,text),billing.fund_recurring_run(uuid),billing.lock_recurring_order_for_call(uuid,uuid) to service_role;
do $$ begin if exists(select 1 from pg_roles where rolname='gateway_v2_runtime') then
 grant execute on function billing.approve_locked_recurring_order(uuid,uuid,bigint,bigint,text),billing.fund_recurring_run(uuid),billing.lock_recurring_order_for_call(uuid,uuid) to gateway_v2_runtime;
end if; end $$;

create or replace function public.reserve_recurring_call(p_call uuid,p_user_id uuid,p_idempotency_key text,p_provider_price_micro_usd bigint,p_charged_micro_usd bigint,p_selection_id uuid,p_metadata jsonb,p_gateway_idempotency_key text) returns jsonb
language plpgsql security invoker set search_path='' as $$
declare s billing.recurring_orders%rowtype; r billing.recurring_runs%rowtype; c billing.recurring_calls%rowtype; answer jsonb;
begin
 select o.* into strict s from billing.recurring_orders o join billing.recurring_runs rr on rr.order_id=o.id join billing.recurring_calls cc on cc.run_id=rr.id where cc.id=p_call and o.owner_id=p_user_id;
 -- Reuse account authorization, organisation lock, limits and membership rules.
 if tenancy.reservation_account(p_user_id,p_selection_id,p_charged_micro_usd)<>coalesce(s.workspace_id,p_user_id) then raise exception 'payer_mismatch'; end if;
 perform 1 from billing.user_balances where account_id=coalesce(s.workspace_id,p_user_id) for update;
 select * into strict s from billing.recurring_orders where id=s.id for update;
 select r0.* into strict r from billing.recurring_runs r0 join billing.recurring_calls c0 on c0.run_id=r0.id where c0.id=p_call for update of r0;
 select * into strict c from billing.recurring_calls where id=p_call for update;
 if p_idempotency_key<>c.id::text or p_selection_id<>c.selection_id or p_charged_micro_usd<>c.cap_micro_usd or c.state<>'dispatching' then raise exception 'recurring_call_mismatch'; end if;
 if c.allocated_micro_usd=0 then
  if r.funded_micro_usd<p_charged_micro_usd+coalesce((select sum(allocated_micro_usd) from billing.recurring_calls where run_id=r.id),0) then raise exception 'run_not_funded'; end if;
  if s.state<>'active' or r.state<>'running' or s.reserved_micro_usd<p_charged_micro_usd then raise exception 'recurring_order_inactive'; end if;
  update billing.user_balances set balance_micro_usd=balance_micro_usd+p_charged_micro_usd where account_id=coalesce(s.workspace_id,p_user_id);
  update billing.recurring_orders set reserved_micro_usd=reserved_micro_usd-p_charged_micro_usd,transferred_micro_usd=transferred_micro_usd+p_charged_micro_usd,updated_at=now() where id=s.id;
  update billing.recurring_calls set allocated_micro_usd=p_charged_micro_usd where id=c.id;
 end if;
 answer:=public.reserve_balance_for_call(p_user_id,p_idempotency_key,p_provider_price_micro_usd,p_charged_micro_usd,p_selection_id,coalesce(p_metadata,'{}'::jsonb)||jsonb_build_object('recurring_order_id',s.id,'recurring_run_id',r.id,'recurring_call_id',c.id),p_gateway_idempotency_key);
 if answer ? 'error' then raise exception 'recurring_reservation_refused'; end if;
 return answer;
end $$;

-- Retired approval cannot bypass the reviewed fingerprint and per-run cap.
create or replace function billing.approve_recurring_order(p_owner uuid,p_id uuid,p_total bigint) returns void
language plpgsql security definer set search_path='' as $$
begin raise exception 'locked_workflow_approval_required'; end $$;

alter table billing.recurring_deliveries add column provider_log_id text;
alter table billing.recurring_calls add column input_parts jsonb check(input_parts is null or jsonb_typeof(input_parts)='object');

create or replace function public.read_recurring_call(p_call uuid) returns jsonb
language sql security invoker set search_path='' as $$
 select jsonb_build_object('owner_id',o.owner_id,'selection_id',c.selection_id,'selection_kind',s.kind,'inputs',c.inputs,'input_parts',c.input_parts)
 from billing.recurring_calls c join billing.recurring_runs r on r.id=c.run_id join billing.recurring_orders o on o.id=r.order_id
 join actions.user_selected_apis s on s.id=c.selection_id and s.user_id=o.owner_id
 where c.id=p_call and c.state='dispatching' and r.state='running' and r.lease_until>now() and o.state='active'
$$;
