-- Preserve the runner backend that produced each persisted review-agent row
-- and each reusable cache entry. Legacy rows predate runner attribution, so
-- they are backfilled as `api`; new writes bind the resolved runtime runner.

alter table review_agents
  add column if not exists runner text not null default 'api'
    check (runner in ('api', 'cli', 'cloud', 'local_inference'));

alter table review_cache
  add column if not exists runner text not null default 'api'
    check (runner in ('api', 'cli', 'cloud', 'local_inference'));
