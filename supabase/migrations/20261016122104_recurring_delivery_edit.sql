-- Delivery can be explicitly revised between runs. The approved source recipe,
-- schedule and financial mandate remain immutable.
alter table billing.recurring_orders
 add column if not exists delivery_revision integer not null default 1 check(delivery_revision > 0);

create table if not exists billing.recurring_delivery_changes (
 order_id uuid not null references billing.recurring_orders(id),
 revision integer not null,
 owner_id uuid not null references auth.users(id),
 changed_at timestamptz not null default now(),
 previous_delivery jsonb not null,
 delivery jsonb not null,
 previous_action jsonb not null,
 action jsonb not null,
 previous_fingerprint text not null,
 fingerprint text not null,
 primary key(order_id, revision)
);
alter table billing.recurring_delivery_changes enable row level security;
revoke all on billing.recurring_delivery_changes from public,anon,authenticated;
grant select,insert on billing.recurring_delivery_changes to service_role;
do $$ begin if exists(select 1 from pg_roles where rolname='gateway_v2_runtime') then
 grant select,insert on billing.recurring_delivery_changes to gateway_v2_runtime;
 drop policy if exists recurring_delivery_audit_read on billing.recurring_delivery_changes;
 create policy recurring_delivery_audit_read on billing.recurring_delivery_changes for select to gateway_v2_runtime using (true);
 drop policy if exists recurring_delivery_audit_insert on billing.recurring_delivery_changes;
 create policy recurring_delivery_audit_insert on billing.recurring_delivery_changes for insert to gateway_v2_runtime with check (true);
end if; end $$;

create or replace function billing.guard_recurring_recipe() returns trigger language plpgsql set search_path='' as $$
begin
 if old.approved_at is not null then
  if (new.approved_at,new.owner_id,new.workspace_id,new.question,new.timezone,new.configuration,new.sources,new.dates,new.total_per_run,new.provider_per_run,new.billing_mode,new.change_only)
   is distinct from
   (old.approved_at,old.owner_id,old.workspace_id,old.question,old.timezone,old.configuration,old.sources,old.dates,old.total_per_run,old.provider_per_run,old.billing_mode,old.change_only)
  then raise exception 'approved_workflow_immutable'; end if;

  if (new.delivery,new.workflow,new.delivery_revision) is distinct from (old.delivery,old.workflow,old.delivery_revision) then
   if old.state not in ('active','paused') or new.state<>old.state
    or new.delivery_revision<>old.delivery_revision+1
    or old.workflow is null or new.workflow is null
    or (new.workflow - array['delivery','fingerprint']) is distinct from (old.workflow - array['delivery','fingerprint'])
    or coalesce(new.workflow->>'fingerprint','')='' or new.workflow->>'fingerprint'=old.workflow->>'fingerprint'
    or jsonb_typeof(new.workflow->'delivery') is distinct from 'object'
    or new.workflow->'delivery'->>'owner' is distinct from old.owner_id::text
    or new.delivery->>'toolkit' is distinct from new.workflow->'delivery'->>'toolkit'
    or new.delivery->>'connected_account_id' is distinct from new.workflow->'delivery'->>'connected_account_id'
    or (new.approved_micro_usd,new.reserved_micro_usd,new.transferred_micro_usd,new.released_micro_usd)
       is distinct from (old.approved_micro_usd,old.reserved_micro_usd,old.transferred_micro_usd,old.released_micro_usd)
    or new.baseline_hash is not null or new.baseline_at is not null
   then raise exception 'approved_workflow_immutable'; end if;
   if coalesce(new.delivery->>'toolkit','') not in ('slack','gmail')
    or (new.delivery->>'toolkit'='slack' and new.delivery->>'destination' is distinct from new.workflow->'delivery'->'fixed_arguments'->>'channel')
    or (new.delivery->>'toolkit'='gmail' and new.delivery->>'destination' is distinct from new.workflow->'delivery'->'fixed_arguments'->>'recipient_email')
   then raise exception 'invalid_delivery_revision'; end if;

   -- The UPDATE already holds the order lock. Serialize against execution
   -- claims without waiting on a worker that could itself need the order lock.
   perform 1 from billing.recurring_runs where order_id=old.id and finished_at is null for update nowait;
   if exists(select 1 from billing.recurring_runs where order_id=old.id and state in ('running','unresolved'))
    or exists(select 1 from billing.recurring_deliveries d join billing.recurring_runs r on r.id=d.run_id
              where r.order_id=old.id and d.state in ('queued','sending','uncertain'))
   then raise exception 'recurring_delivery_busy'; end if;

   insert into billing.recurring_delivery_changes(order_id,revision,owner_id,previous_delivery,delivery,previous_action,action,previous_fingerprint,fingerprint)
    values(old.id,new.delivery_revision,old.owner_id,old.delivery,new.delivery,old.workflow->'delivery',new.workflow->'delivery',old.workflow->>'fingerprint',new.workflow->>'fingerprint');
  end if;
 end if;
 if old.approved_at is null and new.approved_at is not null and (new.workflow is null or new.workflow->>'fingerprint' is null) then
  raise exception 'locked_workflow_required';
 end if;
 return new;
end $$;
