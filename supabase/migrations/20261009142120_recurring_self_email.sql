-- Internal recipient resolution exposes one verified address, not auth.users.
-- The Gateway supplies the authenticated/durable recurring order owner UUID.
create function billing.recurring_self_email(p_owner uuid) returns text
language sql stable security definer set search_path='' as $$
 select u.email from auth.users u
 where u.id=p_owner and u.email_confirmed_at is not null and u.deleted_at is null
   and nullif(btrim(u.email),'') is not null
$$;
revoke all on function billing.recurring_self_email(uuid) from public,anon,authenticated;
do $$ begin
 if exists(select 1 from pg_roles where rolname='gateway_v2_runtime') then
  grant execute on function billing.recurring_self_email(uuid) to gateway_v2_runtime;
 end if;
end $$;
