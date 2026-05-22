-- Add a terminal system failure state for reviews whose DAG/runtime failed
-- before moderation or publication.

ALTER TABLE reviews
  DROP CONSTRAINT IF EXISTS reviews_status_check;

ALTER TABLE reviews
  ADD CONSTRAINT reviews_status_check
  CHECK (status IN (
    'draft','in_review','awaiting_moderation',
    'pr_open','published','corrected','withdrawn','rejected',
    'system_failed'
  ));
