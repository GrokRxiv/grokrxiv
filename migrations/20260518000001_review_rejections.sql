-- Phase 4: rejection is a public terminal state.
--
-- Today `grokrxiv reject` writes to `moderation_queue` (service-role only RLS)
-- and leaves `reviews.status='awaiting_moderation'`. Anon has no idea the
-- paper was rejected. This migration makes rejection a discoverable artifact:
--   - `rejected` joins the reviews.status check constraint and the
--     `reviews_one_active_per_paper` partial unique index;
--   - new `rejections` table holds the public rationale, mirroring the
--     `corrections` shape;
--   - anon RLS on `reviews` widens to include `rejected`;
--   - anon RLS on `rejections` gates on the parent review being `rejected`.
--
-- `grokrxiv reject` will transition the row to `rejected` + insert a
-- rejections record. The web app surfaces the row with a red "Rejected" badge
-- and renders `rationale_md`.

-- 1) Extend the reviews.status CHECK to allow 'rejected'.
ALTER TABLE reviews
  DROP CONSTRAINT IF EXISTS reviews_status_check;
ALTER TABLE reviews
  ADD CONSTRAINT reviews_status_check
  CHECK (status IN (
    'draft','in_review','awaiting_moderation',
    'pr_open','published','corrected','withdrawn','rejected'
  ));

-- 2) `rejected` is terminal: drop it from the one-active-per-paper partial
--    index so a paper can be re-reviewed even after a rejection (creates a new
--    review row at draft → awaiting_moderation alongside the rejected row).
DROP INDEX IF EXISTS reviews_one_active_per_paper;
CREATE UNIQUE INDEX reviews_one_active_per_paper
  ON reviews (paper_id)
  WHERE status IN ('draft','in_review','awaiting_moderation','pr_open','published','corrected');

-- 3) New `rejections` table for the public rationale.
CREATE TABLE IF NOT EXISTS rejections (
  id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  review_id     UUID NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
  rationale_md  TEXT NOT NULL,
  created_by    TEXT NOT NULL,
  created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS rejections_review_created_idx
  ON rejections(review_id, created_at DESC);

ALTER TABLE rejections ENABLE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS rejections_public_read ON rejections;
CREATE POLICY rejections_public_read ON rejections
  FOR SELECT
  TO anon, authenticated
  USING (
    EXISTS (
      SELECT 1 FROM reviews r
      WHERE r.id = rejections.review_id
        AND r.status = 'rejected'
    )
  );

DROP POLICY IF EXISTS rejections_service_write ON rejections;
CREATE POLICY rejections_service_write ON rejections
  FOR ALL
  TO service_role
  USING (true)
  WITH CHECK (true);

-- 4) Widen reviews_public_read to expose rejected reviews.
DROP POLICY IF EXISTS reviews_public_read ON reviews;
CREATE POLICY reviews_public_read ON reviews
  FOR SELECT
  TO anon, authenticated
  USING (status IN ('pr_open','published','corrected','rejected'));

-- 5) Widen papers_public_read so the rejected review's paper is also visible.
DROP POLICY IF EXISTS papers_public_read ON papers;
CREATE POLICY papers_public_read ON papers
  FOR SELECT
  TO anon, authenticated
  USING (
    EXISTS (
      SELECT 1 FROM reviews r
      WHERE r.paper_id = papers.id
        AND r.status IN ('pr_open','published','corrected','rejected')
    )
  );

-- 6) Widen review_agents_public_read so the rejected review's specialist
--    outputs (which explain WHY it was rejected) are visible on the page.
DROP POLICY IF EXISTS review_agents_public_read ON review_agents;
CREATE POLICY review_agents_public_read ON review_agents
  FOR SELECT
  TO anon, authenticated
  USING (
    EXISTS (
      SELECT 1 FROM reviews r
      WHERE r.id = review_agents.review_id
        AND r.status IN ('pr_open','published','corrected','rejected')
    )
  );
