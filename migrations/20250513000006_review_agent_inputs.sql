-- FP4: persist the structured INPUT each agent saw, alongside its OUTPUT.
-- The supervisor's parallel typed-DAG now hands each specialist a copy of the
-- `PaperExtract` shape and hands the meta-reviewer the full bundle of the five
-- specialist outputs. We record both so consumers can audit which artifact a
-- given agent reasoned over (and so the renderer can read inputs back out
-- during artifact rebuilds).
alter table review_agents add column if not exists input_artifact jsonb;

-- A (review_id, role) composite is the natural lookup for "what did agent X
-- see for review Y" and is cheap to maintain.
create index if not exists review_agents_input_role_idx
  on review_agents (review_id, role);
