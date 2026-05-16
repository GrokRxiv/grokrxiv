-- Add a `submitted_date` column to `papers` so the orchestrator's scheduler
-- can decide whether to auto-enqueue a review job after ingest. Papers
-- submitted on or after `AUTO_REVIEW_FROM` (default 2026-04-01) are
-- auto-reviewed; older papers stay in the table without an automatic review.

alter table papers add column if not exists submitted_date date;

create index if not exists papers_submitted_date_idx on papers (submitted_date);
