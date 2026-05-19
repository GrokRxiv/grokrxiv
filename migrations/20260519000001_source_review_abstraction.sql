-- Source review abstraction + automated gate feedback.
--
-- V1 keeps the review DAG academic-paper-only while allowing papers to come
-- from arXiv, local PDF/TeX files, or Git repositories containing a PDF/TeX
-- manuscript. Existing arXiv rows keep their `arxiv_id`; local/git rows use a
-- stable source_id in that compatibility column until the public API can move
-- away from arXiv-shaped naming entirely.

alter table papers
  add column if not exists source_kind text not null default 'arxiv',
  add column if not exists source_id text,
  add column if not exists source_uri text,
  add column if not exists source_hash text,
  add column if not exists source_metadata jsonb not null default '{}'::jsonb;

update papers
   set source_id = coalesce(source_id, arxiv_id),
       source_kind = coalesce(source_kind, 'arxiv')
 where source_id is null;

alter table papers
  drop constraint if exists papers_source_kind_check;
alter table papers
  add constraint papers_source_kind_check
  check (source_kind in ('arxiv','local_file','git_repo'));

create unique index if not exists papers_source_uidx
  on papers(source_kind, source_id)
  where source_id is not null;

create index if not exists papers_source_kind_idx on papers(source_kind);
create index if not exists papers_source_hash_idx on papers(source_hash);

-- The authenticated submissions surface can now refer to a Git repository.
alter table submissions
  drop constraint if exists submissions_source_type_check;
alter table submissions
  add constraint submissions_source_type_check
  check (source_type in ('arxiv','pdf','tex','mixed','git_repo'));

-- Durable lifecycle/event stream used to dedupe GitHub deliveries and explain
-- how a review moved through automated gates, moderation, and re-review.
create table if not exists review_events (
  id                 uuid primary key default gen_random_uuid(),
  review_id          uuid references reviews(id) on delete cascade,
  paper_id           uuid references papers(id) on delete cascade,
  event_type         text not null,
  source             text not null,
  payload            jsonb not null default '{}'::jsonb,
  github_delivery_id text,
  created_at         timestamptz not null default now()
);

create unique index if not exists review_events_github_delivery_uidx
  on review_events(github_delivery_id)
  where github_delivery_id is not null;
create index if not exists review_events_review_created_idx
  on review_events(review_id, created_at desc);
create index if not exists review_events_paper_created_idx
  on review_events(paper_id, created_at desc);

create table if not exists review_gate_failures (
  id                   uuid primary key default gen_random_uuid(),
  review_id            uuid not null references reviews(id) on delete cascade,
  gate                 text not null,
  severity             text not null
                         check (severity in ('low','medium','high','critical')),
  summary              text not null,
  details_md           text not null,
  action_required_md   text,
  status               text not null default 'open'
                         check (status in ('open','addressed','superseded')),
  github_comment_id    bigint,
  github_comment_url   text,
  created_at           timestamptz not null default now(),
  resolved_at          timestamptz
);

create index if not exists review_gate_failures_review_idx
  on review_gate_failures(review_id, created_at desc);
create index if not exists review_gate_failures_status_idx
  on review_gate_failures(status);

create table if not exists github_review_threads (
  id                    uuid primary key default gen_random_uuid(),
  review_id              uuid not null references reviews(id) on delete cascade,
  paper_id               uuid not null references papers(id) on delete cascade,
  repo_owner             text not null,
  repo_name              text not null,
  pr_number              bigint,
  pr_url                 text,
  head_ref               text,
  head_sha               text,
  feedback_comment_id    bigint,
  feedback_comment_url   text,
  last_seen_comment_id   bigint,
  last_seen_commit_sha   text,
  created_at             timestamptz not null default now(),
  updated_at             timestamptz not null default now()
);

create unique index if not exists github_review_threads_review_uidx
  on github_review_threads(review_id);
create index if not exists github_review_threads_pr_idx
  on github_review_threads(repo_owner, repo_name, pr_number);

create table if not exists rereview_requests (
  id                 uuid primary key default gen_random_uuid(),
  paper_id           uuid not null references papers(id) on delete cascade,
  prior_review_id    uuid references reviews(id) on delete set null,
  trigger            text not null
                       check (trigger in ('author_commit','author_comment','moderator_request_changes','manual')),
  github_comment_url text,
  github_commit_sha  text,
  requested_by       text,
  notes_md           text,
  error              text,
  state              text not null default 'queued'
                       check (state in ('queued','running','done','failed','ignored_duplicate')),
  new_review_id      uuid references reviews(id) on delete set null,
  created_at         timestamptz not null default now(),
  started_at         timestamptz,
  finished_at        timestamptz
);

create unique index if not exists rereview_requests_commit_uidx
  on rereview_requests(prior_review_id, github_commit_sha)
  where github_commit_sha is not null;
create index if not exists rereview_requests_state_idx on rereview_requests(state);
create index if not exists rereview_requests_paper_idx on rereview_requests(paper_id, created_at desc);

alter table review_events          enable row level security;
alter table review_gate_failures   enable row level security;
alter table github_review_threads  enable row level security;
alter table rereview_requests      enable row level security;

drop policy if exists review_events_service_all on review_events;
create policy review_events_service_all on review_events
  for all to service_role using (true) with check (true);

drop policy if exists review_gate_failures_public_read on review_gate_failures;
create policy review_gate_failures_public_read on review_gate_failures
  for select to anon, authenticated
  using (
    exists (
      select 1 from reviews r
      where r.id = review_gate_failures.review_id
        and r.visibility = 'public'
        and r.status in ('pr_open','published','corrected','rejected')
    )
  );

drop policy if exists review_gate_failures_submitter_read on review_gate_failures;
create policy review_gate_failures_submitter_read on review_gate_failures
  for select to authenticated
  using (
    exists (
      select 1 from reviews r
      where r.id = review_gate_failures.review_id
        and r.submitted_by = auth.uid()
    )
  );

drop policy if exists review_gate_failures_moderator_read on review_gate_failures;
create policy review_gate_failures_moderator_read on review_gate_failures
  for select to authenticated
  using (public.grokrxiv_is_moderator_or_admin());

drop policy if exists review_gate_failures_service_all on review_gate_failures;
create policy review_gate_failures_service_all on review_gate_failures
  for all to service_role using (true) with check (true);

drop policy if exists github_review_threads_service_all on github_review_threads;
create policy github_review_threads_service_all on github_review_threads
  for all to service_role using (true) with check (true);

drop policy if exists rereview_requests_service_all on rereview_requests;
create policy rereview_requests_service_all on rereview_requests
  for all to service_role using (true) with check (true);
