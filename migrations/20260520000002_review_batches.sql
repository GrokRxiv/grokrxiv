-- Batch review tracking for scheduled arXiv category sweeps.

create table if not exists review_batches (
  id            uuid primary key default gen_random_uuid(),
  category      text not null,
  from_date     date not null,
  until_date    date not null,
  daily_limit   int not null check (daily_limit > 0 and daily_limit <= 500),
  auto_pr       boolean not null default false,
  state         text not null default 'active'
                  check (state in ('active','paused','done','failed')),
  created_at    timestamptz not null default now(),
  updated_at    timestamptz not null default now()
);

create index if not exists review_batches_state_created_idx
  on review_batches(state, created_at desc);

create table if not exists review_batch_items (
  id                uuid primary key default gen_random_uuid(),
  batch_id          uuid not null references review_batches(id) on delete cascade,
  arxiv_id          text not null,
  title             text not null,
  primary_category  text,
  submitted_date    date,
  position          int not null,
  scheduled_for     date not null,
  state             text not null default 'queued'
                       check (state in (
                         'queued','running','reviewed','pr_open','failed','skipped'
                       )),
  paper_id          uuid references papers(id) on delete set null,
  review_id         uuid references reviews(id) on delete set null,
  job_id            uuid references jobs(id) on delete set null,
  pr_url            text,
  attempts          int not null default 0,
  error             text,
  started_at        timestamptz,
  finished_at       timestamptz,
  created_at        timestamptz not null default now(),
  updated_at        timestamptz not null default now(),
  unique(batch_id, arxiv_id)
);

create index if not exists review_batch_items_batch_schedule_idx
  on review_batch_items(batch_id, state, scheduled_for, position);

create index if not exists review_batch_items_arxiv_idx
  on review_batch_items(arxiv_id);

alter table review_batches enable row level security;
alter table review_batch_items enable row level security;

drop policy if exists review_batches_service_all on review_batches;
create policy review_batches_service_all on review_batches
  for all
  to service_role
  using (true)
  with check (true);

drop policy if exists review_batch_items_service_all on review_batch_items;
create policy review_batch_items_service_all on review_batch_items
  for all
  to service_role
  using (true)
  with check (true);
