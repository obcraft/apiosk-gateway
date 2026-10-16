-- Claim exactly the reviewed occurrence. Replays cannot consume the next run.
with candidate as (
    select r.id
    from billing.recurring_runs r
    join billing.recurring_orders o on o.id = r.order_id
    where r.id = $1 and o.id = $2 and o.owner_id = $3
      and o.workspace_id is not distinct from $4
      and o.state = 'active' and o.approved_at is not null
      and r.state = 'queued' and r.finished_at is null
      and r.scheduled_at >= now() - interval '1 hour'
      and not exists (
          select 1 from billing.recurring_runs previous
          where previous.order_id = o.id and previous.finished_at is null
            and previous.scheduled_at < r.scheduled_at
      )
    for update of r skip locked
)
update billing.recurring_runs r
set state = 'running', lease_id = $5, lease_until = now() + interval '3 minutes'
from candidate where r.id = candidate.id and r.state = 'queued'
returning r.scheduled_at
