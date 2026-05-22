-- Rename the first DAG app projection tables from generic "research" wording
-- to the concrete GrokRxiv app namespace. Execution state remains in the
-- generic AgentHero app_runs/dag_runs/dag_run_nodes tables.

do $$
begin
  if to_regclass('public.research_sources') is not null
     and to_regclass('public.grokrxiv_sources') is null then
    alter table research_sources rename to grokrxiv_sources;
  end if;

  if to_regclass('public.research_reviews') is not null
     and to_regclass('public.grokrxiv_reviews') is null then
    alter table research_reviews rename to grokrxiv_reviews;
  end if;

  if to_regclass('public.research_moderation_queue') is not null
     and to_regclass('public.grokrxiv_moderation_queue') is null then
    alter table research_moderation_queue rename to grokrxiv_moderation_queue;
  end if;
end $$;

do $$
begin
  if to_regclass('public.research_sources_source_uidx') is not null
     and to_regclass('public.grokrxiv_sources_source_uidx') is null then
    alter index research_sources_source_uidx
      rename to grokrxiv_sources_source_uidx;
  end if;

  if to_regclass('public.research_sources_field_idx') is not null
     and to_regclass('public.grokrxiv_sources_field_idx') is null then
    alter index research_sources_field_idx
      rename to grokrxiv_sources_field_idx;
  end if;

  if to_regclass('public.research_reviews_source_created_idx') is not null
     and to_regclass('public.grokrxiv_reviews_source_created_idx') is null then
    alter index research_reviews_source_created_idx
      rename to grokrxiv_reviews_source_created_idx;
  end if;

  if to_regclass('public.research_reviews_state_idx') is not null
     and to_regclass('public.grokrxiv_reviews_state_idx') is null then
    alter index research_reviews_state_idx
      rename to grokrxiv_reviews_state_idx;
  end if;

  if to_regclass('public.research_reviews_app_run_idx') is not null
     and to_regclass('public.grokrxiv_reviews_app_run_idx') is null then
    alter index research_reviews_app_run_idx
      rename to grokrxiv_reviews_app_run_idx;
  end if;

  if to_regclass('public.research_reviews_dag_run_idx') is not null
     and to_regclass('public.grokrxiv_reviews_dag_run_idx') is null then
    alter index research_reviews_dag_run_idx
      rename to grokrxiv_reviews_dag_run_idx;
  end if;

  if to_regclass('public.research_moderation_queue_state_idx') is not null
     and to_regclass('public.grokrxiv_moderation_queue_state_idx') is null then
    alter index research_moderation_queue_state_idx
      rename to grokrxiv_moderation_queue_state_idx;
  end if;

  if to_regclass('public.research_moderation_queue_review_idx') is not null
     and to_regclass('public.grokrxiv_moderation_queue_review_idx') is null then
    alter index research_moderation_queue_review_idx
      rename to grokrxiv_moderation_queue_review_idx;
  end if;
end $$;

create table if not exists grokrxiv_sources (
  id                  uuid primary key default gen_random_uuid(),
  source_kind         text not null,
  source_id           text not null,
  source_uri          text,
  source_hash         text,
  title               text,
  authors             jsonb not null default '[]'::jsonb,
  abstract            text,
  field               text,
  submitted_date      date,
  metadata            jsonb not null default '{}'::jsonb,
  created_at          timestamptz not null default now(),
  updated_at          timestamptz not null default now()
);

create unique index if not exists grokrxiv_sources_source_uidx
  on grokrxiv_sources(source_kind, source_id);
create index if not exists grokrxiv_sources_field_idx on grokrxiv_sources(field);

create table if not exists grokrxiv_reviews (
  id                  uuid primary key default gen_random_uuid(),
  source_id           uuid not null references grokrxiv_sources(id) on delete cascade,
  app_run_id          uuid references app_runs(id) on delete set null,
  dag_run_id          uuid references dag_runs(id) on delete set null,
  state               text not null
                        check (state in (
                          'draft','in_review','awaiting_moderation',
                          'pr_open','published','corrected','withdrawn',
                          'rejected','system_failed'
                        )),
  github_pr_url       text,
  github_review_url   text,
  html_path           text,
  pdf_path            text,
  zip_path            text,
  models_used         jsonb not null default '{}'::jsonb,
  meta_review         jsonb,
  failure_code        text,
  failure_message     text,
  failure_retryable   boolean,
  failed_at           timestamptz,
  created_at          timestamptz not null default now(),
  published_at        timestamptz,
  updated_at          timestamptz not null default now()
);

create index if not exists grokrxiv_reviews_source_created_idx
  on grokrxiv_reviews(source_id, created_at desc);
create index if not exists grokrxiv_reviews_state_idx on grokrxiv_reviews(state);
create index if not exists grokrxiv_reviews_app_run_idx on grokrxiv_reviews(app_run_id);
create index if not exists grokrxiv_reviews_dag_run_idx on grokrxiv_reviews(dag_run_id);

create table if not exists grokrxiv_moderation_queue (
  id                  uuid primary key default gen_random_uuid(),
  review_id           uuid not null references grokrxiv_reviews(id) on delete cascade,
  state               text not null
                        check (state in (
                          'pending','approved','rejected','changes_requested',
                          'superseded'
                        )),
  notes               text,
  moderator           text,
  decided_at          timestamptz,
  created_at          timestamptz not null default now(),
  updated_at          timestamptz not null default now()
);

create index if not exists grokrxiv_moderation_queue_state_idx
  on grokrxiv_moderation_queue(state);
create index if not exists grokrxiv_moderation_queue_review_idx
  on grokrxiv_moderation_queue(review_id);

insert into grokrxiv_sources (
  id,
  source_kind,
  source_id,
  source_uri,
  source_hash,
  title,
  authors,
  abstract,
  field,
  submitted_date,
  metadata,
  created_at,
  updated_at
)
select
  p.id,
  coalesce(p.source_kind, 'arxiv'),
  coalesce(p.source_id, p.arxiv_id),
  p.source_uri,
  p.source_hash,
  p.title,
  p.authors,
  p.abstract,
  p.field,
  p.submitted_date,
  coalesce(p.source_metadata, '{}'::jsonb),
  p.ingested_at,
  p.ingested_at
from papers p
on conflict (source_kind, source_id) do nothing;

insert into grokrxiv_reviews (
  id,
  source_id,
  state,
  github_pr_url,
  github_review_url,
  html_path,
  pdf_path,
  zip_path,
  models_used,
  meta_review,
  failure_code,
  failure_message,
  failure_retryable,
  failed_at,
  created_at,
  published_at,
  updated_at
)
select
  r.id,
  r.paper_id,
  r.status,
  r.github_pr_url,
  r.github_review_url,
  r.html_path,
  r.pdf_path,
  r.zip_path,
  r.models_used,
  r.meta_review,
  r.failure_code,
  r.failure_message,
  r.failure_retryable,
  r.failed_at,
  r.created_at,
  r.published_at,
  coalesce(r.failed_at, r.published_at, r.created_at)
from reviews r
on conflict (id) do nothing;

insert into grokrxiv_moderation_queue (
  id,
  review_id,
  state,
  notes,
  moderator,
  decided_at,
  created_at,
  updated_at
)
select
  mq.id,
  mq.review_id,
  mq.state,
  mq.notes,
  mq.moderator,
  mq.decided_at,
  mq.created_at,
  coalesce(mq.decided_at, mq.created_at)
from moderation_queue mq
where exists (
  select 1 from grokrxiv_reviews gr
  where gr.id = mq.review_id
)
on conflict (id) do nothing;
