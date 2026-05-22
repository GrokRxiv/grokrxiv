-- Track 8h — storage-split-aware pointers on paper_assets.
-- Postgres holds the index + status. Bulk content lives in grokrxiv-data (Git)
-- and the per-artifact-type Supabase Object Storage buckets (raw-pdfs,
-- raw-source, extracted-markdown, extracted-json, embeddings, review-artifacts).

ALTER TABLE paper_assets
  ADD COLUMN IF NOT EXISTS git_path            TEXT,
  ADD COLUMN IF NOT EXISTS git_commit_sha      TEXT,
  ADD COLUMN IF NOT EXISTS storage_prefix      TEXT,
  ADD COLUMN IF NOT EXISTS extraction_status   TEXT NOT NULL DEFAULT 'pending',
  ADD COLUMN IF NOT EXISTS extraction_cost_usd NUMERIC(10,4);

CREATE INDEX IF NOT EXISTS paper_assets_status_idx
  ON paper_assets(extraction_status);

CREATE INDEX IF NOT EXISTS paper_assets_extraction_pending_idx
  ON paper_assets(paper_id) WHERE extraction_status = 'pending';

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint WHERE conname = 'paper_assets_extraction_status_check'
  ) THEN
    ALTER TABLE paper_assets
      ADD CONSTRAINT paper_assets_extraction_status_check
        CHECK (extraction_status IN ('pending','running','ready','failed'));
  END IF;
END $$;
