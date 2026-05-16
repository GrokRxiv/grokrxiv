-- RPT2 Track F: agent execution mode column on reviews.
--
-- Reviews are now produced in one of two modes:
--   - `review_only`         (today's default — produce reviews, nothing else)
--   - `review_and_revise`   (also emit a revision_artifact proposing patches to
--                            either the paper's LaTeX source or the review's
--                            own output)
--
-- The column is non-null with a permissive default so existing rows are
-- transparently classified as the historical `review_only` mode.

ALTER TABLE reviews
  ADD COLUMN IF NOT EXISTS mode TEXT NOT NULL DEFAULT 'review_only'
    CHECK (mode IN ('review_only', 'review_and_revise'));

CREATE INDEX IF NOT EXISTS reviews_mode_idx ON reviews(mode);
