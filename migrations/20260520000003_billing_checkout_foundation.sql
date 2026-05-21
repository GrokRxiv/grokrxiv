-- Billing checkout foundation and default account rows.

alter table user_billing
  add column if not exists provider text not null default 'manual',
  add column if not exists stripe_customer_id text,
  add column if not exists stripe_subscription_id text,
  add column if not exists stripe_price_id text;

do $$
begin
  if not exists (
    select 1
    from pg_constraint
    where conname = 'user_billing_provider_check'
      and conrelid = 'user_billing'::regclass
  ) then
    alter table user_billing
      add constraint user_billing_provider_check
      check (provider in ('manual','stripe'));
  end if;
end $$;

create unique index if not exists user_billing_stripe_customer_idx
  on user_billing(stripe_customer_id)
  where stripe_customer_id is not null;

create unique index if not exists user_billing_stripe_subscription_idx
  on user_billing(stripe_subscription_id)
  where stripe_subscription_id is not null;

create table if not exists stripe_webhook_events (
  id          text primary key,
  event_type  text not null,
  payload     jsonb not null default '{}'::jsonb,
  received_at timestamptz not null default now()
);

alter table stripe_webhook_events enable row level security;

drop policy if exists stripe_webhook_events_service_all on stripe_webhook_events;
create policy stripe_webhook_events_service_all on stripe_webhook_events
  for all
  to service_role
  using (true)
  with check (true);

drop policy if exists stripe_webhook_events_admin_read on stripe_webhook_events;
create policy stripe_webhook_events_admin_read on stripe_webhook_events
  for select
  to authenticated
  using (public.grokrxiv_is_admin());

create or replace function public.grokrxiv_ensure_user_account(
  target_user_id uuid,
  target_email text default null
)
returns void
language plpgsql
security definer
set search_path = public
as $$
begin
  insert into profiles (user_id, display_name, billing_tier)
  values (target_user_id, nullif(target_email, ''), 'free')
  on conflict (user_id) do nothing;

  insert into user_roles (user_id, role)
  values (target_user_id, 'user')
  on conflict (user_id) do nothing;

  insert into user_billing (user_id, plan_id, status)
  values (target_user_id, 'free', 'active')
  on conflict (user_id) do nothing;

  insert into review_credits (user_id)
  values (target_user_id)
  on conflict (user_id) do nothing;
end;
$$;

grant execute on function public.grokrxiv_ensure_user_account(uuid, text)
  to authenticated, service_role;

create or replace function public.grokrxiv_handle_new_auth_user()
returns trigger
language plpgsql
security definer
set search_path = public
as $$
begin
  perform public.grokrxiv_ensure_user_account(new.id, new.email);
  return new;
end;
$$;

drop trigger if exists on_auth_user_created on auth.users;
create trigger on_auth_user_created
  after insert on auth.users
  for each row execute function public.grokrxiv_handle_new_auth_user();

insert into profiles (user_id, display_name, billing_tier)
select id, nullif(email, ''), 'free'
from auth.users
on conflict (user_id) do nothing;

insert into user_roles (user_id, role)
select id, 'user'
from auth.users
on conflict (user_id) do nothing;

insert into user_billing (user_id, plan_id, status)
select id, 'free', 'active'
from auth.users
on conflict (user_id) do nothing;

insert into review_credits (user_id)
select id
from auth.users
on conflict (user_id) do nothing;
