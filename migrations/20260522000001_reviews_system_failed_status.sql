-- Add a terminal system failure state for reviews whose DAG/runtime failed
-- before moderation or publication.

ALTER TABLE reviews
  ADD COLUMN IF NOT EXISTS failure_code text,
  ADD COLUMN IF NOT EXISTS failure_message text,
  ADD COLUMN IF NOT EXISTS failure_retryable boolean,
  ADD COLUMN IF NOT EXISTS failed_at timestamptz;

ALTER TABLE reviews
  DROP CONSTRAINT IF EXISTS reviews_status_check;

ALTER TABLE reviews
  ADD CONSTRAINT reviews_status_check
  CHECK (status IN (
    'draft','in_review','awaiting_moderation',
    'pr_open','published','corrected','withdrawn','rejected',
    'system_failed'
  ));

UPDATE moderation_queue mq
SET state = 'superseded',
    notes = CASE
      WHEN notes IS NULL OR btrim(notes) = ''
        THEN 'review moved to system_failed before moderation'
      ELSE notes || E'\nreview moved to system_failed before moderation'
    END,
    decided_at = coalesce(decided_at, now())
FROM reviews r
WHERE mq.review_id = r.id
  AND r.status = 'system_failed'
  AND mq.state = 'pending';
