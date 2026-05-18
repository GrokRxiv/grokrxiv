-- Auth, pricing, visibility, and quota foundation.
--
-- Public review surfaces must require BOTH:
--   reviews.visibility = 'public'
--   reviews.status IN ('pr_open','published','corrected','rejected')
--
-- Private reviews are for owner dashboards and moderator/admin consoles only.

create extension if not exists pgcrypto;

create table if not exists user_roles (
  user_id     uuid primary key references auth.users(id) on delete cascade,
  role        text not null default 'user'
                check (role in ('user','moderator','admin')),
  created_at  timestamptz not null default now(),
  updated_at  timestamptz not null default now()
);

-- ---------------------------------------------------------------------------
-- Role helpers
-- ---------------------------------------------------------------------------
create or replace function public.grokrxiv_current_user_role()
returns text
language sql
stable
security definer
set search_path = public
as $$
  select coalesce(
    (
      select ur.role
      from public.user_roles ur
      where ur.user_id = auth.uid()
      order by ur.created_at desc
      limit 1
    ),
    'user'
  )
$$;

create or replace function public.grokrxiv_is_admin()
returns boolean
language sql
stable
security definer
set search_path = public
as $$
  select public.grokrxiv_current_user_role() = 'admin'
$$;

create or replace function public.grokrxiv_is_moderator_or_admin()
returns boolean
language sql
stable
security definer
set search_path = public
as $$
  select public.grokrxiv_current_user_role() in ('moderator','admin')
$$;

grant execute on function public.grokrxiv_current_user_role() to anon, authenticated;
grant execute on function public.grokrxiv_is_admin() to anon, authenticated;
grant execute on function public.grokrxiv_is_moderator_or_admin() to anon, authenticated;

-- ---------------------------------------------------------------------------
-- Account, billing, quota, and submission tables
-- ---------------------------------------------------------------------------
create table if not exists profiles (
  user_id                 uuid primary key references auth.users(id) on delete cascade,
  display_name            text,
  orcid_id                text,
  github_handle           text,
  billing_tier            text not null default 'free'
                            check (billing_tier in ('free','supporter','researcher','admin')),
  review_limit_override   int check (review_limit_override is null or review_limit_override >= 0),
  created_at              timestamptz not null default now(),
  updated_at              timestamptz not null default now()
);

create table if not exists billing_plans (
  id                         text primary key,
  name                       text not null,
  public_reviews_per_month   int not null default 0 check (public_reviews_per_month >= 0),
  private_reviews_per_month  int not null default 0 check (private_reviews_per_month >= 0),
  lifetime_public_reviews    int check (lifetime_public_reviews is null or lifetime_public_reviews >= 0),
  allow_private              boolean not null default false,
  allow_api_addon            boolean not null default false,
  queue_priority             int not null default 0,
  created_at                 timestamptz not null default now()
);

insert into billing_plans (
  id, name, public_reviews_per_month, private_reviews_per_month,
  lifetime_public_reviews, allow_private, allow_api_addon, queue_priority
) values
  ('free', 'Free', 0, 0, 3, false, false, 0),
  ('supporter', 'Supporter', 10, 2, null, true, false, 10),
  ('researcher', 'Researcher', 30, 10, null, true, true, 20),
  ('admin', 'Admin', 1000000, 1000000, null, true, true, 100)
on conflict (id) do update set
  name = excluded.name,
  public_reviews_per_month = excluded.public_reviews_per_month,
  private_reviews_per_month = excluded.private_reviews_per_month,
  lifetime_public_reviews = excluded.lifetime_public_reviews,
  allow_private = excluded.allow_private,
  allow_api_addon = excluded.allow_api_addon,
  queue_priority = excluded.queue_priority;

create table if not exists user_billing (
  user_id       uuid primary key references auth.users(id) on delete cascade,
  plan_id       text not null references billing_plans(id),
  status        text not null default 'active'
                  check (status in ('active','trialing','past_due','canceled')),
  period_start  timestamptz,
  period_end    timestamptz,
  created_at    timestamptz not null default now(),
  updated_at    timestamptz not null default now()
);

create table if not exists review_credits (
  user_id          uuid primary key references auth.users(id) on delete cascade,
  public_credits   int not null default 0 check (public_credits >= 0),
  private_credits  int not null default 0 check (private_credits >= 0),
  api_credits      int not null default 0 check (api_credits >= 0),
  updated_at       timestamptz not null default now()
);

create table if not exists submissions (
  id               uuid primary key default gen_random_uuid(),
  user_id          uuid references auth.users(id) on delete set null,
  source           text not null,
  source_type      text not null default 'arxiv'
                     check (source_type in ('arxiv','pdf','tex','mixed')),
  visibility       text not null default 'public'
                     check (visibility in ('public','private')),
  compute_profile  text not null default 'public_free'
                     check (compute_profile in (
                       'sample_preview','public_free','paid_standard',
                       'paid_private','premium_api'
                     )),
  state            text not null default 'queued'
                     check (state in (
                       'queued','running','awaiting_moderation','pr_open',
                       'published','corrected','rejected','private_ready',
                       'failed','system_failed','cancelled'
                     )),
  review_id        uuid references reviews(id) on delete set null,
  quota_charged    boolean not null default false,
  cost_cap_usd     numeric(10,2) check (cost_cap_usd is null or cost_cap_usd >= 0),
  error            text,
  created_at       timestamptz not null default now(),
  updated_at       timestamptz not null default now()
);

create index if not exists submissions_user_created_idx
  on submissions(user_id, created_at desc);
create index if not exists submissions_state_idx on submissions(state);
create index if not exists submissions_review_idx on submissions(review_id);
create index if not exists submissions_visibility_idx on submissions(visibility);

create table if not exists quota_events (
  id              uuid primary key default gen_random_uuid(),
  user_id         uuid references auth.users(id) on delete set null,
  submission_id   uuid references submissions(id) on delete set null,
  event_type      text not null
                    check (event_type in (
                      'accepted','blocked','charged','refunded','override'
                    )),
  visibility      text check (visibility in ('public','private')),
  compute_profile text check (compute_profile in (
                      'sample_preview','public_free','paid_standard',
                      'paid_private','premium_api'
                    )),
  amount          int,
  reason          text,
  metadata        jsonb not null default '{}'::jsonb,
  created_at      timestamptz not null default now()
);

create index if not exists quota_events_user_created_idx
  on quota_events(user_id, created_at desc);
create index if not exists quota_events_submission_idx on quota_events(submission_id);

create table if not exists quota_snapshots (
  id                     uuid primary key default gen_random_uuid(),
  provider               text not null check (provider in ('codex','claude','gemini')),
  auth_mode              text not null check (auth_mode in ('cli_oauth','app_server','cookie','api')),
  quota_window_used      numeric,
  quota_window_remaining numeric,
  reset_at               timestamptz,
  confidence             text not null default 'unknown'
                           check (confidence in ('high','medium','low','unknown')),
  source                 text,
  error                  text,
  observed_at            timestamptz not null default now()
);

create index if not exists quota_snapshots_provider_observed_idx
  on quota_snapshots(provider, observed_at desc);

-- ---------------------------------------------------------------------------
-- Review visibility and moderation ownership
-- ---------------------------------------------------------------------------
alter table reviews
  add column if not exists visibility text not null default 'public';

do $$
begin
  if not exists (
    select 1 from pg_constraint
    where conname = 'reviews_visibility_check'
      and conrelid = 'reviews'::regclass
  ) then
    alter table reviews
      add constraint reviews_visibility_check
      check (visibility in ('public','private'));
  end if;
end $$;

alter table reviews
  add column if not exists submitted_by uuid references auth.users(id) on delete set null;

create index if not exists reviews_visibility_status_idx
  on reviews(visibility, status, created_at desc);
create index if not exists reviews_submitted_by_idx on reviews(submitted_by);

alter table moderation_queue
  add column if not exists moderator_user_id uuid references auth.users(id) on delete set null;

create index if not exists moderation_queue_moderator_user_idx
  on moderation_queue(moderator_user_id);

-- ---------------------------------------------------------------------------
-- RLS
-- ---------------------------------------------------------------------------
alter table profiles        enable row level security;
alter table user_roles      enable row level security;
alter table billing_plans   enable row level security;
alter table user_billing    enable row level security;
alter table review_credits  enable row level security;
alter table submissions     enable row level security;
alter table quota_events    enable row level security;
alter table quota_snapshots enable row level security;

drop policy if exists profiles_self_read on profiles;
create policy profiles_self_read on profiles
  for select to authenticated
  using (user_id = auth.uid());

drop policy if exists profiles_admin_read on profiles;
create policy profiles_admin_read on profiles
  for select to authenticated
  using (public.grokrxiv_is_admin());

drop policy if exists profiles_admin_update on profiles;
create policy profiles_admin_update on profiles
  for update to authenticated
  using (public.grokrxiv_is_admin())
  with check (public.grokrxiv_is_admin());

drop policy if exists profiles_admin_insert on profiles;
create policy profiles_admin_insert on profiles
  for insert to authenticated
  with check (public.grokrxiv_is_admin());

drop policy if exists profiles_service_all on profiles;
create policy profiles_service_all on profiles
  for all to service_role
  using (true)
  with check (true);

drop policy if exists user_roles_self_read on user_roles;
create policy user_roles_self_read on user_roles
  for select to authenticated
  using (user_id = auth.uid());

drop policy if exists user_roles_admin_read on user_roles;
create policy user_roles_admin_read on user_roles
  for select to authenticated
  using (public.grokrxiv_is_admin());

drop policy if exists user_roles_service_all on user_roles;
create policy user_roles_service_all on user_roles
  for all to service_role
  using (true)
  with check (true);

drop policy if exists billing_plans_read on billing_plans;
create policy billing_plans_read on billing_plans
  for select to anon, authenticated
  using (true);

drop policy if exists user_billing_self_read on user_billing;
create policy user_billing_self_read on user_billing
  for select to authenticated
  using (user_id = auth.uid());

drop policy if exists user_billing_admin_read on user_billing;
create policy user_billing_admin_read on user_billing
  for select to authenticated
  using (public.grokrxiv_is_admin());

drop policy if exists user_billing_admin_update on user_billing;
create policy user_billing_admin_update on user_billing
  for update to authenticated
  using (public.grokrxiv_is_admin())
  with check (public.grokrxiv_is_admin());

drop policy if exists user_billing_admin_insert on user_billing;
create policy user_billing_admin_insert on user_billing
  for insert to authenticated
  with check (public.grokrxiv_is_admin());

drop policy if exists user_billing_service_all on user_billing;
create policy user_billing_service_all on user_billing
  for all to service_role
  using (true)
  with check (true);

drop policy if exists review_credits_self_read on review_credits;
create policy review_credits_self_read on review_credits
  for select to authenticated
  using (user_id = auth.uid());

drop policy if exists review_credits_admin_read on review_credits;
create policy review_credits_admin_read on review_credits
  for select to authenticated
  using (public.grokrxiv_is_admin());

drop policy if exists review_credits_admin_update on review_credits;
create policy review_credits_admin_update on review_credits
  for update to authenticated
  using (public.grokrxiv_is_admin())
  with check (public.grokrxiv_is_admin());

drop policy if exists review_credits_admin_insert on review_credits;
create policy review_credits_admin_insert on review_credits
  for insert to authenticated
  with check (public.grokrxiv_is_admin());

drop policy if exists review_credits_service_all on review_credits;
create policy review_credits_service_all on review_credits
  for all to service_role
  using (true)
  with check (true);

drop policy if exists submissions_self_read on submissions;
create policy submissions_self_read on submissions
  for select to authenticated
  using (user_id = auth.uid());

drop policy if exists submissions_admin_read on submissions;
create policy submissions_admin_read on submissions
  for select to authenticated
  using (public.grokrxiv_is_moderator_or_admin());

drop policy if exists submissions_service_all on submissions;
create policy submissions_service_all on submissions
  for all to service_role
  using (true)
  with check (true);

drop policy if exists quota_events_self_read on quota_events;
create policy quota_events_self_read on quota_events
  for select to authenticated
  using (user_id = auth.uid());

drop policy if exists quota_events_admin_read on quota_events;
create policy quota_events_admin_read on quota_events
  for select to authenticated
  using (public.grokrxiv_is_admin());

drop policy if exists quota_events_service_all on quota_events;
create policy quota_events_service_all on quota_events
  for all to service_role
  using (true)
  with check (true);

drop policy if exists quota_snapshots_admin_read on quota_snapshots;
create policy quota_snapshots_admin_read on quota_snapshots
  for select to authenticated
  using (public.grokrxiv_is_admin());

drop policy if exists quota_snapshots_service_all on quota_snapshots;
create policy quota_snapshots_service_all on quota_snapshots
  for all to service_role
  using (true)
  with check (true);

-- Public reviews must be public visibility AND public status.
drop policy if exists reviews_public_read on reviews;
create policy reviews_public_read on reviews
  for select
  to anon, authenticated
  using (
    visibility = 'public'
    and status in ('pr_open','published','corrected','rejected')
  );

drop policy if exists reviews_submitter_read on reviews;
create policy reviews_submitter_read on reviews
  for select
  to authenticated
  using (submitted_by = auth.uid());

drop policy if exists reviews_moderator_read on reviews;
create policy reviews_moderator_read on reviews
  for select
  to authenticated
  using (public.grokrxiv_is_moderator_or_admin());

drop policy if exists papers_public_read on papers;
create policy papers_public_read on papers
  for select
  to anon, authenticated
  using (
    exists (
      select 1 from reviews r
      where r.paper_id = papers.id
        and r.visibility = 'public'
        and r.status in ('pr_open','published','corrected','rejected')
    )
  );

drop policy if exists review_agents_public_read on review_agents;
create policy review_agents_public_read on review_agents
  for select
  to anon, authenticated
  using (
    exists (
      select 1 from reviews r
      where r.id = review_agents.review_id
        and r.visibility = 'public'
        and r.status in ('pr_open','published','corrected','rejected')
    )
  );

drop policy if exists review_agents_submitter_read on review_agents;
create policy review_agents_submitter_read on review_agents
  for select
  to authenticated
  using (
    exists (
      select 1 from reviews r
      where r.id = review_agents.review_id
        and r.submitted_by = auth.uid()
    )
  );

drop policy if exists review_agents_moderator_read on review_agents;
create policy review_agents_moderator_read on review_agents
  for select
  to authenticated
  using (public.grokrxiv_is_moderator_or_admin());

drop policy if exists rejections_public_read on rejections;
create policy rejections_public_read on rejections
  for select
  to anon, authenticated
  using (
    exists (
      select 1 from reviews r
      where r.id = rejections.review_id
        and r.visibility = 'public'
        and r.status = 'rejected'
    )
  );

drop policy if exists rejections_submitter_read on rejections;
create policy rejections_submitter_read on rejections
  for select
  to authenticated
  using (
    exists (
      select 1 from reviews r
      where r.id = rejections.review_id
        and r.submitted_by = auth.uid()
    )
  );

drop policy if exists rejections_moderator_read on rejections;
create policy rejections_moderator_read on rejections
  for select
  to authenticated
  using (public.grokrxiv_is_moderator_or_admin());
