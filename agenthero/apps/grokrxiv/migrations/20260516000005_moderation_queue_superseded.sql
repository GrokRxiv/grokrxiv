-- FP-RPT3b B6: add `superseded` to the moderation_queue state enum.
--
-- `db::insert_review` (orchestrator/src/db.rs:152–189) auto-withdraws any
-- prior active reviews for the same paper when a fresh review lands. Those
-- reviews' `moderation_queue` rows were previously orphaned — still in
-- `pending` (or `approved` / `changes_requested`) but pointing at a row
-- whose `reviews.status = 'withdrawn'`. This migration adds the missing
-- terminal state and back-fills it for already-orphaned rows.

ALTER TABLE moderation_queue
  DROP CONSTRAINT IF EXISTS moderation_queue_state_check;

ALTER TABLE moderation_queue
  ADD CONSTRAINT moderation_queue_state_check
    CHECK (state IN ('pending', 'approved', 'rejected', 'changes_requested', 'superseded'));

-- Back-fill: any moderation_queue row whose review was withdrawn via the
-- supersede path (and is still in a non-terminal mq state) becomes
-- `superseded`. `rejected` rows are left as-is — a rejected review that
-- happens to be superseded later keeps the rejection as its primary signal.
UPDATE moderation_queue mq
  SET state = 'superseded'
  FROM reviews r
  WHERE mq.review_id = r.id
    AND r.status = 'withdrawn'
    AND r.superseded_at IS NOT NULL
    AND mq.state IN ('pending', 'approved', 'changes_requested');
