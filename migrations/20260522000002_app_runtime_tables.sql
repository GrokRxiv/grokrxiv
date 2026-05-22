-- Generic app/DAG runtime tables.
--
-- Product apps share one orchestration schema. DAG-specific tables are only
-- projection/business tables; they are not the scheduler or executor contract.

create extension if not exists pgcrypto;

create table if not exists app_runs (
  id                  uuid primary key default gen_random_uuid(),
  app_id              text not null,
  action_id           text not null,
  state               text not null default 'queued'
                        check (state in (
                          'queued','running','partial','done','failed',
                          'cancelled','system_failed'
                        )),
  input               jsonb not null default '{}'::jsonb,
  output              jsonb not null default '{}'::jsonb,
  error_code          text,
  error_message       text,
  error_retryable     boolean,
  created_by          text,
  started_at          timestamptz,
  finished_at         timestamptz,
  created_at          timestamptz not null default now(),
  updated_at          timestamptz not null default now()
);

create index if not exists app_runs_app_action_created_idx
  on app_runs(app_id, action_id, created_at desc);
create index if not exists app_runs_state_idx on app_runs(state);

create table if not exists dag_runs (
  id                  uuid primary key default gen_random_uuid(),
  app_run_id          uuid references app_runs(id) on delete cascade,
  dag_type            text not null,
  manifest_version    int,
  manifest_hash       text,
  state               text not null default 'queued'
                        check (state in (
                          'queued','running','partial','done','failed',
                          'cancelled','system_failed'
                        )),
  input               jsonb not null default '{}'::jsonb,
  output              jsonb not null default '{}'::jsonb,
  error_code          text,
  error_message       text,
  error_retryable     boolean,
  started_at          timestamptz,
  finished_at         timestamptz,
  created_at          timestamptz not null default now(),
  updated_at          timestamptz not null default now()
);

create index if not exists dag_runs_app_run_idx on dag_runs(app_run_id);
create index if not exists dag_runs_dag_type_state_idx on dag_runs(dag_type, state);

create table if not exists dag_run_nodes (
  id                  uuid primary key default gen_random_uuid(),
  dag_run_id          uuid not null references dag_runs(id) on delete cascade,
  node_id             text not null,
  node_kind           text not null,
  role                text,
  tool                text,
  child_dag_type      text,
  state               text not null default 'queued'
                        check (state in (
                          'queued','running','ok','degraded','skipped',
                          'failed','cancelled','system_failed'
                        )),
  required            boolean not null default true,
  attempt             int not null default 0,
  runner              text,
  model               text,
  input               jsonb not null default '{}'::jsonb,
  output              jsonb not null default '{}'::jsonb,
  error_code          text,
  error_message       text,
  error_retryable     boolean,
  latency_ms          int,
  started_at          timestamptz,
  finished_at         timestamptz,
  created_at          timestamptz not null default now(),
  updated_at          timestamptz not null default now()
);

create unique index if not exists dag_run_nodes_dag_node_attempt_uidx
  on dag_run_nodes(dag_run_id, node_id, attempt);
create index if not exists dag_run_nodes_role_idx on dag_run_nodes(role);
create index if not exists dag_run_nodes_tool_idx on dag_run_nodes(tool);
create index if not exists dag_run_nodes_state_idx on dag_run_nodes(state);

create table if not exists dag_artifacts (
  id                  uuid primary key default gen_random_uuid(),
  app_run_id          uuid references app_runs(id) on delete cascade,
  dag_run_id          uuid references dag_runs(id) on delete cascade,
  node_run_id         uuid references dag_run_nodes(id) on delete set null,
  name                text not null,
  uri                 text not null,
  media_type          text,
  sha256              text,
  size_bytes          bigint,
  schema_ref          text,
  metadata            jsonb not null default '{}'::jsonb,
  created_at          timestamptz not null default now()
);

create index if not exists dag_artifacts_app_run_idx on dag_artifacts(app_run_id);
create index if not exists dag_artifacts_dag_run_idx on dag_artifacts(dag_run_id);
create index if not exists dag_artifacts_node_run_idx on dag_artifacts(node_run_id);
create index if not exists dag_artifacts_name_idx on dag_artifacts(name);

create table if not exists dag_events (
  id                  bigserial primary key,
  app_run_id          uuid references app_runs(id) on delete cascade,
  dag_run_id          uuid references dag_runs(id) on delete cascade,
  node_run_id         uuid references dag_run_nodes(id) on delete set null,
  level               text not null default 'info'
                        check (level in ('debug','info','warn','error')),
  event_type          text not null,
  message             text,
  payload             jsonb not null default '{}'::jsonb,
  created_at          timestamptz not null default now()
);

create index if not exists dag_events_app_created_idx on dag_events(app_run_id, created_at desc);
create index if not exists dag_events_dag_created_idx on dag_events(dag_run_id, created_at desc);
create index if not exists dag_events_type_idx on dag_events(event_type);

create table if not exists worker_nodes (
  id                  uuid primary key default gen_random_uuid(),
  name                text not null,
  capabilities        jsonb not null default '{}'::jsonb,
  state               text not null default 'online'
                        check (state in ('online','draining','offline')),
  last_heartbeat_at   timestamptz,
  created_at          timestamptz not null default now(),
  updated_at          timestamptz not null default now()
);

create unique index if not exists worker_nodes_name_uidx on worker_nodes(name);
create index if not exists worker_nodes_state_idx on worker_nodes(state);

create table if not exists worker_leases (
  id                  uuid primary key default gen_random_uuid(),
  worker_id           uuid not null references worker_nodes(id) on delete cascade,
  app_run_id          uuid references app_runs(id) on delete cascade,
  dag_run_id          uuid references dag_runs(id) on delete cascade,
  node_run_id         uuid references dag_run_nodes(id) on delete cascade,
  state               text not null default 'leased'
                        check (state in ('leased','released','expired','failed')),
  leased_until        timestamptz not null,
  created_at          timestamptz not null default now(),
  updated_at          timestamptz not null default now()
);

create index if not exists worker_leases_worker_state_idx on worker_leases(worker_id, state);
create index if not exists worker_leases_expiry_idx on worker_leases(leased_until);
create index if not exists worker_leases_node_idx on worker_leases(node_run_id);

create table if not exists agent_output_cache (
  id                  uuid primary key default gen_random_uuid(),
  app_id              text not null,
  dag_type            text not null,
  node_id             text not null,
  role                text,
  runner              text,
  model               text,
  input_hash          text not null,
  output              jsonb not null,
  verifier_status     text,
  tokens_in           int,
  tokens_out          int,
  created_at          timestamptz not null default now(),
  updated_at          timestamptz not null default now()
);

create unique index if not exists agent_output_cache_lookup_uidx
  on agent_output_cache(
    app_id,
    dag_type,
    node_id,
    coalesce(role, ''),
    coalesce(runner, ''),
    coalesce(model, ''),
    input_hash
  );

-- Research app projections. These exist for product queries and moderation UI,
-- while app_runs/dag_runs/dag_run_nodes remain the execution source of truth.
create table if not exists research_sources (
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

create unique index if not exists research_sources_source_uidx
  on research_sources(source_kind, source_id);
create index if not exists research_sources_field_idx on research_sources(field);

create table if not exists research_reviews (
  id                  uuid primary key default gen_random_uuid(),
  source_id           uuid not null references research_sources(id) on delete cascade,
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

create index if not exists research_reviews_source_created_idx
  on research_reviews(source_id, created_at desc);
create index if not exists research_reviews_state_idx on research_reviews(state);
create index if not exists research_reviews_app_run_idx on research_reviews(app_run_id);
create index if not exists research_reviews_dag_run_idx on research_reviews(dag_run_id);

create table if not exists research_moderation_queue (
  id                  uuid primary key default gen_random_uuid(),
  review_id           uuid not null references research_reviews(id) on delete cascade,
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

create index if not exists research_moderation_queue_state_idx
  on research_moderation_queue(state);
create index if not exists research_moderation_queue_review_idx
  on research_moderation_queue(review_id);

insert into research_sources (
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

insert into research_reviews (
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

insert into research_moderation_queue (
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
  select 1 from research_reviews rr
  where rr.id = mq.review_id
)
on conflict (id) do nothing;
