-- Show reviews on the public site as soon as a PR is open on the mirror repo.
-- Web UI labels the badge "In Review" for pr_open, "Published" for
-- published/corrected. Visibility widens from {published,corrected} to
-- {pr_open,published,corrected} for: reviews, papers (via inner-join),
-- review_agents (via inner-join). The withdrawn / awaiting_moderation /
-- in_review / draft statuses remain hidden.
--
-- Corrections stay gated on {published,corrected} only — a `pr_open` review
-- has no published artifact to correct yet.

drop policy if exists papers_public_read on papers;
create policy papers_public_read on papers
  for select
  to anon, authenticated
  using (
    exists (
      select 1 from reviews r
      where r.paper_id = papers.id
        and r.status in ('pr_open','published','corrected')
    )
  );

drop policy if exists reviews_public_read on reviews;
create policy reviews_public_read on reviews
  for select
  to anon, authenticated
  using (status in ('pr_open','published','corrected'));

drop policy if exists review_agents_public_read on review_agents;
create policy review_agents_public_read on review_agents
  for select
  to anon, authenticated
  using (
    exists (
      select 1 from reviews r
      where r.id = review_agents.review_id
        and r.status in ('pr_open','published','corrected')
    )
  );
