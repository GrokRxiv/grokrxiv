-- Make review agent identity DAG-scoped instead of a fixed six-role enum.
-- Existing rows are backfilled as the paper-review DAG.

alter table reviews
  add column if not exists dag_type text not null default 'paper-review';

alter table review_inputs
  add column if not exists dag_type text not null default 'paper-review';

alter table review_agents
  add column if not exists dag_type text not null default 'paper-review',
  add column if not exists node_id text,
  add column if not exists agent_type text not null default 'critic',
  add column if not exists node_kind text not null default 'agent';

alter table review_agents
  drop constraint if exists review_agents_role_check;

update review_agents
set node_id = coalesce(node_id, role),
    agent_type = case when role = 'meta_reviewer' then 'synthesizer' else agent_type end,
    node_kind = case when role = 'meta_reviewer' then 'synthesizer' else node_kind end
where node_id is null
   or role = 'meta_reviewer';

alter table review_cache
  add column if not exists dag_type text not null default 'paper-review';

alter table moderation_queue
  add column if not exists dag_type text not null default 'paper-review';

alter table jobs
  add column if not exists dag_type text not null default 'paper-review';

alter table moderation_queue
  add column if not exists dag_type text not null default 'paper-review';

alter table jobs
  add column if not exists dag_type text not null default 'paper-review';

drop index if exists idx_review_cache_lookup;
create unique index if not exists idx_review_cache_lookup
  on review_cache(dag_type, paper_id, role, content_hash);

create index if not exists review_agents_dag_role_idx
  on review_agents(dag_type, review_id, role);

create index if not exists review_agents_dag_node_idx
  on review_agents(dag_type, review_id, node_id);

create index if not exists reviews_dag_type_idx
  on reviews(dag_type);

create index if not exists moderation_queue_dag_type_idx
  on moderation_queue(dag_type);

create index if not exists jobs_dag_type_idx
  on jobs(dag_type);

create index if not exists moderation_queue_dag_type_idx
  on moderation_queue(dag_type);

create index if not exists jobs_dag_type_idx
  on jobs(dag_type);
