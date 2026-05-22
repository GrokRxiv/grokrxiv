-- Normalize dormant runner values before tightening the GrokRxiv app contract.

update review_agents
   set runner = 'cli'
 where runner in ('cloud', 'local_inference');

update review_cache
   set runner = 'cli'
 where runner in ('cloud', 'local_inference');

alter table review_agents drop constraint if exists review_agents_runner_check;
alter table review_agents
  add constraint review_agents_runner_check check (runner in ('api', 'cli'));

alter table review_cache drop constraint if exists review_cache_runner_check;
alter table review_cache
  add constraint review_cache_runner_check check (runner in ('api', 'cli'));
