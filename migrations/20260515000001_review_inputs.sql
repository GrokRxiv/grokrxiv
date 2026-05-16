-- FP6 A2: dedupe input_artifact storage.
--
-- The supervisor's typed DAG previously persisted the full ~52K-token
-- `PaperExtract` shape onto every specialist row in `review_agents.input_artifact`
-- (5 rows per review = ~260K tokens of duplicated JSONB per paper). Each
-- specialist sees the same input artifact, so a single row per review is
-- sufficient. The meta-reviewer no longer reasons over the paper extract
-- (see A1) so we only need the shared specialist input here.

create table if not exists review_inputs (
    review_id  uuid primary key references reviews(id) on delete cascade,
    paper_id   uuid not null references papers(id),
    artifact   jsonb not null,
    created_at timestamptz not null default now()
);

create index if not exists idx_review_inputs_paper_id on review_inputs(paper_id);

-- Migrate existing data: take input_artifact from the first specialist row per
-- review. The meta-reviewer's input artifact is intentionally dropped — A1
-- trims meta input to the specialist outputs alone, which are still recorded
-- on each specialist's `review_agents.output` column.
insert into review_inputs (review_id, paper_id, artifact)
select distinct on (ra.review_id)
    ra.review_id,
    r.paper_id,
    ra.input_artifact
from review_agents ra
join reviews r on r.id = ra.review_id
where ra.input_artifact is not null
  and ra.role <> 'meta_reviewer'
order by ra.review_id, ra.created_at
on conflict (review_id) do nothing;

-- Drop the now-redundant column from review_agents. The supporting index from
-- migration 0006 goes with it.
drop index if exists review_agents_input_role_idx;
alter table review_agents drop column if exists input_artifact;
