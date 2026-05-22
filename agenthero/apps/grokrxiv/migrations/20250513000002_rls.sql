-- GrokRxiv — Row Level Security policies.
-- Public anon reads are limited to artifacts that have reached `published`.
-- All writes are reserved for the service role used by the Rust orchestrator
-- (service-role JWTs bypass RLS, but explicit policies are kept for clarity).

alter table papers            enable row level security;
alter table paper_assets      enable row level security;
alter table reviews           enable row level security;
alter table review_agents     enable row level security;
alter table jobs              enable row level security;
alter table moderation_queue  enable row level security;
alter table uploads           enable row level security;

-- ---------------------------------------------------------------------------
-- papers: world-readable iff at least one published review references the row
-- ---------------------------------------------------------------------------
drop policy if exists papers_public_read on papers;
create policy papers_public_read on papers
  for select
  to anon, authenticated
  using (
    exists (
      select 1 from reviews r
      where r.paper_id = papers.id
        and r.status in ('published','corrected')
    )
  );

drop policy if exists papers_service_write on papers;
create policy papers_service_write on papers
  for all
  to service_role
  using (true)
  with check (true);

-- ---------------------------------------------------------------------------
-- paper_assets: PRIVATE. Anon must never see arXiv source / PDF paths.
-- Service role only. No public-read policy at all.
-- ---------------------------------------------------------------------------
drop policy if exists paper_assets_service_all on paper_assets;
create policy paper_assets_service_all on paper_assets
  for all
  to service_role
  using (true)
  with check (true);

-- ---------------------------------------------------------------------------
-- reviews: world-readable when status is 'published' or 'corrected'.
-- `withdrawn` and any pre-publication state are NOT exposed to anon.
-- ---------------------------------------------------------------------------
drop policy if exists reviews_public_read on reviews;
create policy reviews_public_read on reviews
  for select
  to anon, authenticated
  using (status in ('published','corrected'));

drop policy if exists reviews_service_write on reviews;
create policy reviews_service_write on reviews
  for all
  to service_role
  using (true)
  with check (true);

-- ---------------------------------------------------------------------------
-- review_agents: world-readable iff parent review is published
-- ---------------------------------------------------------------------------
drop policy if exists review_agents_public_read on review_agents;
create policy review_agents_public_read on review_agents
  for select
  to anon, authenticated
  using (
    exists (
      select 1 from reviews r
      where r.id = review_agents.review_id
        and r.status in ('published','corrected')
    )
  );

drop policy if exists review_agents_service_write on review_agents;
create policy review_agents_service_write on review_agents
  for all
  to service_role
  using (true)
  with check (true);

-- ---------------------------------------------------------------------------
-- jobs / moderation_queue / uploads: no anon reads. service role only.
-- ---------------------------------------------------------------------------
drop policy if exists jobs_service_all on jobs;
create policy jobs_service_all on jobs
  for all
  to service_role
  using (true)
  with check (true);

drop policy if exists moderation_queue_service_all on moderation_queue;
create policy moderation_queue_service_all on moderation_queue
  for all
  to service_role
  using (true)
  with check (true);

drop policy if exists uploads_service_all on uploads;
create policy uploads_service_all on uploads
  for all
  to service_role
  using (true)
  with check (true);

-- ---------------------------------------------------------------------------
-- Storage buckets
-- Supabase Storage buckets are usually created via the dashboard / CLI:
--
--   supabase storage create-bucket pdfs    --public=false
--   supabase storage create-bucket bundles --public=true
--   supabase storage create-bucket renders --public=true
--
-- The SQL equivalent (Supabase >= 1.x exposes storage.buckets directly) is
-- provided below. Wrapped in DO blocks so re-running the migration is safe.
-- ---------------------------------------------------------------------------
do $$
begin
  if exists (
    select 1 from pg_catalog.pg_tables
    where schemaname = 'storage' and tablename = 'buckets'
  ) then
    insert into storage.buckets (id, name, public)
    values ('pdfs',    'pdfs',    false)
    on conflict (id) do nothing;

    insert into storage.buckets (id, name, public)
    values ('bundles', 'bundles', true)
    on conflict (id) do nothing;

    insert into storage.buckets (id, name, public)
    values ('renders', 'renders', true)
    on conflict (id) do nothing;

    -- Enforce desired public-flag on every re-run, in case the bucket
    -- already exists with a different visibility. `pdfs` MUST stay private
    -- (we don't redistribute arXiv PDFs).
    update storage.buckets set public = false where id = 'pdfs';
    update storage.buckets set public = true  where id in ('bundles','renders');
  end if;
end $$;
