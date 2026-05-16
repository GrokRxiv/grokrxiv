-- GrokRxiv — initial schema
-- Supabase Postgres. All UUID PKs, timestamptz timestamps, JSONB for typed artifacts.
-- Status/state columns use check constraints rather than Postgres enums for
-- migration ergonomics.

create extension if not exists pgcrypto;

-- papers: one row per ingested arXiv paper.
-- NOTE: private storage paths (pdf_path, latex_source_path) live in the
-- `paper_assets` table below. Anything anon-readable must live here, because
-- the RLS policy on `papers` exposes the entire row to anon when a public
-- review references the paper.
create table if not exists papers (
  id              uuid primary key default gen_random_uuid(),
  arxiv_id        text not null,
  title           text not null,
  authors         jsonb not null default '[]'::jsonb,
  abstract        text,
  field           text,
  submitted_date  date,
  ingested_at     timestamptz not null default now()
);

create unique index if not exists papers_arxiv_id_uidx on papers(arxiv_id);
create index if not exists papers_field_idx on papers(field);
create index if not exists papers_submitted_date_idx on papers(submitted_date desc);

-- paper_assets: PRIVATE storage paths for each paper.
-- Service-role only — never exposed to anon, because the path values point
-- into a private Supabase Storage bucket holding raw arXiv PDFs / LaTeX
-- source that we are not licensed to redistribute.
create table if not exists paper_assets (
  paper_id           uuid primary key references papers(id) on delete cascade,
  pdf_path           text,
  latex_source_path  text,
  updated_at         timestamptz not null default now()
);

-- reviews: one row per (paper, run)
create table if not exists reviews (
  id                 uuid primary key default gen_random_uuid(),
  paper_id           uuid not null references papers(id) on delete cascade,
  status             text not null
                       check (status in (
                         'draft','awaiting_moderation','in_review','pr_open',
                         'published','corrected','withdrawn'
                       )),
  github_pr_url      text,
  github_review_url  text,
  html_path          text,
  pdf_path           text,
  zip_path           text,
  models_used        jsonb not null default '{}'::jsonb,
  meta_review        jsonb,
  created_at         timestamptz not null default now(),
  published_at       timestamptz
);

create index if not exists reviews_paper_created_idx
  on reviews(paper_id, created_at desc);
create index if not exists reviews_status_idx on reviews(status);
create index if not exists reviews_published_at_idx
  on reviews(published_at desc) where status = 'published';

-- review_agents: provenance for every specialist agent run
create table if not exists review_agents (
  id               uuid primary key default gen_random_uuid(),
  review_id        uuid not null references reviews(id) on delete cascade,
  role             text not null
                     check (role in (
                       'summary','technical_correctness','novelty',
                       'reproducibility','citation','meta_reviewer'
                     )),
  model            text not null,
  prompt_hash      text,
  output           jsonb not null,
  tokens_in        int,
  tokens_out       int,
  latency_ms       int,
  verifier_status  text check (verifier_status in ('pass','warn','fail')),
  verifier_notes   jsonb,
  created_at       timestamptz not null default now()
);

create index if not exists review_agents_review_role_idx
  on review_agents(review_id, role);
create index if not exists review_agents_role_idx on review_agents(role);

-- jobs: orchestrator task tracking
create table if not exists jobs (
  id           uuid primary key default gen_random_uuid(),
  kind         text not null
                 check (kind in ('ingest','review','render','publish','preview')),
  ref_id       uuid,
  state        text not null
                 check (state in ('queued','running','done','failed')),
  attempt      int not null default 0,
  error        text,
  started_at   timestamptz,
  finished_at  timestamptz,
  created_at   timestamptz not null default now()
);

create index if not exists jobs_state_kind_idx on jobs(state, kind);
create index if not exists jobs_ref_idx on jobs(ref_id);

-- moderation_queue: human-gate state for each review
create table if not exists moderation_queue (
  id           uuid primary key default gen_random_uuid(),
  review_id    uuid not null references reviews(id) on delete cascade,
  state        text not null
                 check (state in ('pending','approved','rejected','changes_requested')),
  notes        text,
  moderator    text,
  decided_at   timestamptz,
  created_at   timestamptz not null default now()
);

create index if not exists moderation_queue_state_idx on moderation_queue(state);
create index if not exists moderation_queue_review_idx on moderation_queue(review_id);

-- "uploads" stores landing-page sample_reviews; rows here MUST NOT appear in
-- the public index, sitemap, RSS, or /api/v1/* endpoints.
-- uploads: anonymous landing-page preview requests
create table if not exists uploads (
  id              uuid primary key default gen_random_uuid(),
  ip_hash         text,
  pdf_path        text,
  preview_review  jsonb,
  bundle_path     text,
  created_at      timestamptz not null default now()
);

create index if not exists uploads_ip_hash_idx on uploads(ip_hash);
create index if not exists uploads_created_at_idx on uploads(created_at desc);
