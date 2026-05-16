-- Add supersede tracking + enforce one-active-review-per-paper.
--
-- Before this migration it was legal to publish multiple reviews of the same
-- paper in parallel (we discovered two reviews of arXiv:2605.00403 — c5155ecf
-- and a58c9982 — both with status='published' and both with open PRs on the
-- GrokRxiv/grokrxiv-reviews mirror at #1 and #17 respectively). New semantics:
-- re-reviewing a paper transitions any prior active review to 'withdrawn' with
-- `superseded_at` set, and the partial unique index below makes that the only
-- legal state going forward.

ALTER TABLE reviews
  ADD COLUMN IF NOT EXISTS superseded_at TIMESTAMPTZ;

-- Backfill FIRST so the unique index below can succeed: for each paper with
-- > 1 non-withdrawn review, keep the most-recent one ACTIVE and withdraw the
-- rest. Idempotent (re-runs are no-ops once at most one row per paper is
-- active).
WITH ranked AS (
  SELECT id,
         row_number() OVER (PARTITION BY paper_id ORDER BY created_at DESC) AS rn
    FROM reviews
   WHERE status IN ('draft','in_review','awaiting_moderation','pr_open','published','corrected')
)
UPDATE reviews r
   SET status = 'withdrawn',
       superseded_at = now()
  FROM ranked
 WHERE r.id = ranked.id
   AND ranked.rn > 1;

-- Partial unique index: at most one ACTIVE review per paper. 'withdrawn' is
-- terminal and explicitly excluded so superseded reviews can pile up freely.
CREATE UNIQUE INDEX IF NOT EXISTS reviews_one_active_per_paper
  ON reviews (paper_id)
  WHERE status IN ('draft','in_review','awaiting_moderation','pr_open','published','corrected');
