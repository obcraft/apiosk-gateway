-- The schedule holds funds in the existing account. Each call atomically
-- transfers part of that hold into the existing balance_charges ledger.
create table billing.recurring_orders (
 id uuid primary key, owner_id uuid not null references auth.users(id), workspace_id uuid,
 state text not null default 'draft' check(state in ('draft','active','paused','attention','cancelled','completed')),
 question text not null, timezone text not null, configuration jsonb not null,
 sources jsonb not null check(jsonb_typeof(sources)='array' and jsonb_array_length(sources) between 1 and 100),
 delivery jsonb not null, dates timestamptz[] not null check(cardinality(dates) between 1 and 366),
 provider_per_run bigint not null check(provider_per_run>=0), total_per_run bigint not null check(total_per_run>0),
 approved_micro_usd bigint not null check(approved_micro_usd>0), reserved_micro_usd bigint not null default 0 check(reserved_micro_usd>=0),
 transferred_micro_usd bigint not null default 0 check(transferred_micro_usd>=0), released_micro_usd bigint not null default 0 check(released_micro_usd>=0),
 expires_at timestamptz not null default now()+interval '15 minutes', approved_at timestamptz,
 created_at timestamptz not null default now(), updated_at timestamptz not null default now(),
 check(approved_micro_usd=total_per_run*cardinality(dates)),
 check(approved_at is null or approved_micro_usd=reserved_micro_usd+transferred_micro_usd+released_micro_usd)
);
-- A repeated approval flow for the same original order, interval and channel
-- must not accidentally fund a second active schedule.
create unique index recurring_active_mandate on billing.recurring_orders
 (owner_id,coalesce(workspace_id,'00000000-0000-0000-0000-000000000000'::uuid),configuration,delivery)
 where approved_at is not null and state in ('active','paused','attention');
create table billing.recurring_runs (
 id uuid primary key default gen_random_uuid(), order_id uuid not null references billing.recurring_orders(id),
 scheduled_at timestamptz not null, state text not null default 'queued' check(state in ('queued','running','succeeded','failed','unresolved','skipped','cancelled')),
 lease_id uuid, lease_until timestamptz, finished_at timestamptz, released_micro_usd bigint not null default 0,
 result jsonb, error text, unique(order_id,scheduled_at)
);
create index recurring_due on billing.recurring_runs(scheduled_at) where state in ('queued','running');
create table billing.recurring_calls (
 id uuid primary key default gen_random_uuid(), run_id uuid not null references billing.recurring_runs(id),
 ordinal integer not null, selection_id uuid not null unique references actions.user_selected_apis(id),
 inputs jsonb not null, cap_micro_usd bigint not null check(cap_micro_usd>0), allocated_micro_usd bigint not null default 0,
 state text not null default 'pending' check(state in ('pending','dispatching','succeeded','failed','unresolved')),
 dispatched_at timestamptz, response jsonb, unique(run_id,ordinal), check(allocated_micro_usd between 0 and cap_micro_usd)
);
create table billing.recurring_deliveries (
 id uuid primary key default gen_random_uuid(), run_id uuid not null unique references billing.recurring_runs(id),
 state text not null default 'queued' check(state in ('queued','sending','sent','failed','uncertain')),
 attempts integer not null default 0, next_attempt_at timestamptz not null default now(),
 sent_at timestamptz, error text
);
-- All reads go through owner/workspace-scoped Gateway routes. No Data API writes.
alter table billing.recurring_orders enable row level security;
alter table billing.recurring_runs enable row level security;
alter table billing.recurring_calls enable row level security;
alter table billing.recurring_deliveries enable row level security;
revoke all on billing.recurring_orders,billing.recurring_runs,billing.recurring_calls,billing.recurring_deliveries from public,anon,authenticated;
grant all on billing.recurring_orders,billing.recurring_runs,billing.recurring_calls,billing.recurring_deliveries to service_role;

create function billing.approve_recurring_order(p_owner uuid,p_id uuid,p_total bigint) returns void
language plpgsql security definer set search_path='' as $$
declare s billing.recurring_orders%rowtype; available bigint;
begin
 select * into strict s from billing.recurring_orders where id=p_id and owner_id=p_owner;
 perform tenancy.verified_actor(p_owner);
 if s.workspace_id is not null then
  perform 1 from tenancy.organisations o join tenancy.workspaces w on w.organisation_id=o.id where w.id=s.workspace_id for update of o;
  if not tenancy.workspace_permission(p_owner,s.workspace_id,'spend') then raise exception 'forbidden'; end if;
 else
  perform tenancy.reservation_account(p_owner,null,p_total);
 end if;
 select balance_micro_usd into available from billing.user_balances where account_id=coalesce(s.workspace_id,p_owner) and reversal_debt_micro_usd=0 for update;
 select * into strict s from billing.recurring_orders where id=p_id and owner_id=p_owner for update;
 if s.approved_micro_usd<>p_total then raise exception 'quote_changed'; end if;
 if s.approved_at is not null then return; end if;
 if s.state<>'draft' or s.expires_at<=now() or s.dates[1]<=now() then raise exception 'quote_expired'; end if;
 if available is null or available<p_total then raise exception 'insufficient_balance'; end if;
 update billing.user_balances set balance_micro_usd=balance_micro_usd-p_total where account_id=coalesce(s.workspace_id,p_owner);
 update billing.recurring_orders set state='active',approved_at=now(),reserved_micro_usd=p_total,updated_at=now() where id=p_id;
 insert into billing.recurring_runs(order_id,scheduled_at) select p_id,unnest(s.dates);
end $$;

-- Lock account -> schedule -> run -> call everywhere money can move.
create function public.reserve_recurring_call(p_call uuid,p_user_id uuid,p_idempotency_key text,p_provider_price_micro_usd bigint,p_charged_micro_usd bigint,p_selection_id uuid,p_metadata jsonb,p_gateway_idempotency_key text) returns jsonb
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
  if s.state<>'active' or r.state<>'running' or s.reserved_micro_usd<p_charged_micro_usd then raise exception 'recurring_order_inactive'; end if;
  update billing.user_balances set balance_micro_usd=balance_micro_usd+p_charged_micro_usd where account_id=coalesce(s.workspace_id,p_user_id);
  update billing.recurring_orders set reserved_micro_usd=reserved_micro_usd-p_charged_micro_usd,transferred_micro_usd=transferred_micro_usd+p_charged_micro_usd,updated_at=now() where id=s.id;
  update billing.recurring_calls set allocated_micro_usd=p_charged_micro_usd where id=c.id;
 end if;
 answer:=public.reserve_balance_for_call(p_user_id,p_idempotency_key,p_provider_price_micro_usd,p_charged_micro_usd,p_selection_id,coalesce(p_metadata,'{}'::jsonb)||jsonb_build_object('recurring_order_id',s.id,'recurring_run_id',r.id,'recurring_call_id',c.id),p_gateway_idempotency_key);
 if answer ? 'error' then raise exception 'recurring_reservation_refused'; end if;
 return answer;
end $$;

create function billing.finish_recurring_run(p_run uuid,p_state text,p_result jsonb,p_error text) returns void
language plpgsql security definer set search_path='' as $$
declare s billing.recurring_orders%rowtype; r billing.recurring_runs%rowtype; release bigint;
begin
 if p_state not in ('succeeded','failed','unresolved','skipped','cancelled') then raise exception 'invalid_state'; end if;
 select o.* into strict s from billing.recurring_orders o join billing.recurring_runs rr on rr.order_id=o.id where rr.id=p_run;
 perform 1 from billing.user_balances where account_id=coalesce(s.workspace_id,s.owner_id) for update;
 select * into strict s from billing.recurring_orders where id=s.id for update;
 select * into strict r from billing.recurring_runs where id=p_run for update;
 if r.finished_at is not null then return; end if;
 release:=s.total_per_run-coalesce((select sum(allocated_micro_usd) from billing.recurring_calls where run_id=p_run),0);
 update billing.user_balances set balance_micro_usd=balance_micro_usd+release where account_id=coalesce(s.workspace_id,s.owner_id);
 update billing.recurring_orders set reserved_micro_usd=reserved_micro_usd-release,released_micro_usd=released_micro_usd+release,updated_at=now() where id=s.id;
 update billing.recurring_runs set state=p_state,result=p_result,error=p_error,finished_at=now(),released_micro_usd=release,lease_until=null where id=p_run;
 if p_state='unresolved' then update billing.recurring_orders set state='attention' where id=s.id and state='active'; end if;
 if p_state in ('succeeded','failed') then insert into billing.recurring_deliveries(run_id) values(p_run) on conflict do nothing; end if;
 update billing.recurring_orders set state='completed' where id=s.id and state in ('active','paused') and not exists(select 1 from billing.recurring_runs where order_id=s.id and finished_at is null);
end $$;

create function billing.control_recurring_order(p_owner uuid,p_id uuid,p_state text) returns void
language plpgsql security definer set search_path='' as $$
declare s billing.recurring_orders%rowtype; r record;
begin
 if p_state not in ('paused','active','cancelled') then raise exception 'invalid_state'; end if;
 select * into strict s from billing.recurring_orders where id=p_id and owner_id=p_owner;
 perform tenancy.verified_actor(p_owner);
 if s.workspace_id is not null and not tenancy.workspace_permission(p_owner,s.workspace_id,'spend') then raise exception 'forbidden'; end if;
 perform 1 from billing.user_balances where account_id=coalesce(s.workspace_id,p_owner) for update;
 select * into strict s from billing.recurring_orders where id=p_id for update;
 if s.state=p_state then return; end if;
 if s.state in ('draft','cancelled','completed') or (s.state='attention' and p_state='active') then raise exception 'invalid_transition'; end if;
 update billing.recurring_orders set state=p_state,updated_at=now() where id=p_id;
 if p_state='cancelled' then
  for r in select id from billing.recurring_runs where order_id=p_id and state='queued' order by scheduled_at loop
   perform billing.finish_recurring_run(r.id,'cancelled',null,null);
  end loop;
 end if;
end $$;

revoke all on function billing.approve_recurring_order(uuid,uuid,bigint),billing.finish_recurring_run(uuid,text,jsonb,text),billing.control_recurring_order(uuid,uuid,text),public.reserve_recurring_call(uuid,uuid,text,bigint,bigint,uuid,jsonb,text) from public,anon,authenticated;
grant execute on function billing.approve_recurring_order(uuid,uuid,bigint),billing.finish_recurring_run(uuid,text,jsonb,text),billing.control_recurring_order(uuid,uuid,text),public.reserve_recurring_call(uuid,uuid,text,bigint,bigint,uuid,jsonb,text) to service_role;
do $$ begin if exists(select 1 from pg_roles where rolname='gateway_v2_runtime') then
 grant select,insert,update on billing.recurring_orders,billing.recurring_runs,billing.recurring_calls,billing.recurring_deliveries to gateway_v2_runtime;
 grant execute on function billing.approve_recurring_order(uuid,uuid,bigint),billing.finish_recurring_run(uuid,text,jsonb,text),billing.control_recurring_order(uuid,uuid,text) to gateway_v2_runtime;
 end if; end $$;

create function public.read_recurring_call(p_call uuid) returns jsonb
language sql security invoker set search_path='' as $$
 select jsonb_build_object('owner_id',o.owner_id,'selection_id',c.selection_id,'inputs',c.inputs)
 from billing.recurring_calls c join billing.recurring_runs r on r.id=c.run_id join billing.recurring_orders o on o.id=r.order_id
 where c.id=p_call and c.state='dispatching' and r.state='running' and r.lease_until>now() and o.state='active'
$$;
revoke all on function public.read_recurring_call(uuid) from public,anon,authenticated;
grant execute on function public.read_recurring_call(uuid) to service_role;
do $$ declare t text; begin if exists(select 1 from pg_roles where rolname='gateway_v2_runtime') then
 foreach t in array array['recurring_orders','recurring_runs','recurring_calls','recurring_deliveries'] loop
  execute format('create policy recurring_runtime on billing.%I to gateway_v2_runtime using (true) with check (true)',t);
 end loop;
 grant select(id,user_id,prompt,kind,source,name,capability,api_slug,candidate_id,resource_url,method,provider_price_micro_usd,charged_micro_usd,metadata,created_at) on actions.user_selected_apis to gateway_v2_runtime;
 end if; end $$;

-- Balance read models count the remaining schedule hold, alongside existing
-- call holds. Available is already net of all reservations; never subtract twice.
grant select(owner_id,workspace_id,reserved_micro_usd) on billing.recurring_orders to authenticated;
create policy recurring_personal_hold on billing.recurring_orders for select to authenticated
 using(owner_id=(select auth.uid()) and workspace_id is null);
create or replace function public.balance_summary()
returns jsonb language sql stable security invoker set search_path='' as $$
  select jsonb_build_object('available_micro_usd',coalesce((select balance_micro_usd from billing.user_balances where user_id=auth.uid()),0),
    'held_micro_usd',coalesce((select sum(reserved_micro_usd) from billing.recurring_orders where owner_id=auth.uid() and workspace_id is null),0)+coalesce((select sum(reserved_micro_usd) from billing.job_budget_reservations where user_id=auth.uid() and status in ('open','unresolved')),0)
      +coalesce((select sum(charged_micro_usd) from billing.balance_charges where user_id=auth.uid() and job_id is null and status in ('reserved','unresolved')),0))
$$;
create or replace function public.workspace_finances(p_actor uuid,p_workspace uuid) returns jsonb
language plpgsql security invoker set search_path='' as $$
begin
 perform public.workspace_context(p_actor,p_workspace);
 return jsonb_build_object('workspace_id',p_workspace,'currency','USD','unit','micro',
 'available_micro_usd',(select balance_micro_usd::text from billing.user_balances where workspace_id=p_workspace),
 'reversal_debt_micro_usd',(select reversal_debt_micro_usd::text from billing.user_balances where workspace_id=p_workspace),
 'held_micro_usd',(coalesce((select sum(charged_micro_usd) from billing.balance_charges where workspace_id=p_workspace and status in ('reserved','unresolved')),0)+coalesce((select sum(reserved_micro_usd) from billing.recurring_orders where workspace_id=p_workspace),0))::text,
 'charges',coalesce((select jsonb_agg(to_jsonb(c)) from (select id,user_id actor_id,(select email from auth.users where id=bc.user_id) actor_email,status,charged_micro_usd::text,provider_price_micro_usd::text,fee_micro_usd::text,created_at,settled_at from billing.balance_charges bc where workspace_id=p_workspace order by created_at desc limit 100)c),'[]'::jsonb),
 'topups',coalesce((select jsonb_agg(to_jsonb(t)) from (select id,status,amount_micro_eur::text,origin,credited_at,created_at from billing.balance_topups where workspace_id=p_workspace order by created_at desc limit 50)t),'[]'::jsonb));
end $$;

create or replace function public.organisation_finances(p_actor uuid,p_organisation uuid) returns jsonb
language plpgsql security invoker set search_path='' as $$
declare account uuid;
begin
 perform tenancy.verified_actor(p_actor);
 if not (select enabled from tenancy.feature_flags where name='management') then raise exception 'workspaces_unavailable' using errcode='42501'; end if;
 if not tenancy.org_admin(p_actor,p_organisation) then raise exception 'forbidden' using errcode='42501'; end if;
 select account_id into account from tenancy.organisation_billing_accounts where organisation_id=p_organisation;
 if account is null then raise exception 'budget_unavailable' using errcode='42501'; end if;
 return jsonb_build_object('organisation_id',p_organisation,'currency','USD','unit','micro',
 'available_micro_usd',(select balance_micro_usd::text from billing.user_balances where account_id=account),
 'held_micro_usd',(coalesce((select sum(reserved_micro_usd) from billing.recurring_orders where owner_id=account and workspace_id is null),0)+coalesce((select sum(charged_micro_usd) from billing.balance_charges where user_id=account and workspace_id is null and job_id is null and status in ('reserved','unresolved')),0)+coalesce((select sum(reserved_micro_usd) from billing.job_budget_reservations where user_id=account and status in ('open','unresolved')),0))::text,
 'reversal_debt_micro_usd',(select reversal_debt_micro_usd::text from billing.user_balances where account_id=account),
 'workspaces',coalesce((select jsonb_agg(jsonb_build_object('workspace_id',w.id,'name',w.name,'icon',w.icon,'colour',w.colour,'archived',w.archived_at is not null,
  'available_micro_usd',b.balance_micro_usd::text,'held_micro_usd',(coalesce((select sum(c.charged_micro_usd) from billing.balance_charges c where c.workspace_id=w.id and c.status in ('reserved','unresolved')),0)+coalesce((select sum(reserved_micro_usd) from billing.recurring_orders where workspace_id=w.id),0))::text) order by w.created_at)
  from tenancy.workspaces w join billing.user_balances b on b.workspace_id=w.id where w.organisation_id=p_organisation),'[]'::jsonb),
 'topups',coalesce((select jsonb_agg(to_jsonb(t)) from (select id,status,amount_micro_eur::text,origin,credited_at,created_at from billing.balance_topups where user_id=account and workspace_id is null order by created_at desc limit 50)t),'[]'::jsonb),
 'transfers',coalesce((select jsonb_agg(to_jsonb(t)) from (select id,workspace_id,actor_id,direction,amount_micro_usd::text,created_at from billing.workspace_budget_transfers where organisation_id=p_organisation order by created_at desc limit 50)t),'[]'::jsonb));
end $$;
