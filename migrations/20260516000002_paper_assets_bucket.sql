-- Track 8i — Tier-2 Supabase Object Storage buckets (5-bucket split + review-artifacts).
--
-- Operator-locked artifact-type split. Each bucket carries its own RLS so
-- visibility and lifecycle can be tuned independently. Production-target is
-- Supabase Cloud — same SQL, only SUPABASE_URL / SUPABASE_SERVICE_ROLE_KEY
-- change. See `crates/storage/src/paper_artifacts.rs` for the routing table.

-- Buckets (all private; per-bucket RLS opens specific reads to anon).
INSERT INTO storage.buckets (id, name, public) VALUES ('raw-pdfs',            'raw-pdfs',            false) ON CONFLICT (id) DO NOTHING;
INSERT INTO storage.buckets (id, name, public) VALUES ('raw-source',          'raw-source',          false) ON CONFLICT (id) DO NOTHING;
INSERT INTO storage.buckets (id, name, public) VALUES ('extracted-markdown',  'extracted-markdown',  false) ON CONFLICT (id) DO NOTHING;
INSERT INTO storage.buckets (id, name, public) VALUES ('extracted-json',      'extracted-json',      false) ON CONFLICT (id) DO NOTHING;
INSERT INTO storage.buckets (id, name, public) VALUES ('embeddings',          'embeddings',          false) ON CONFLICT (id) DO NOTHING;
INSERT INTO storage.buckets (id, name, public) VALUES ('review-artifacts',    'review-artifacts',    false) ON CONFLICT (id) DO NOTHING;

-- Idempotent reset.
DROP POLICY IF EXISTS "storage_service_all"              ON storage.objects;
DROP POLICY IF EXISTS "raw_pdfs_anon_read"               ON storage.objects;
DROP POLICY IF EXISTS "extracted_markdown_anon_read"     ON storage.objects;
DROP POLICY IF EXISTS "extracted_json_figures_anon_read" ON storage.objects;
DROP POLICY IF EXISTS "review_artifacts_published_read"  ON storage.objects;

-- Service role: full ALL on every grokrxiv bucket.
CREATE POLICY "storage_service_all"
  ON storage.objects
  FOR ALL
  TO service_role
  USING (
    bucket_id IN (
      'raw-pdfs',
      'raw-source',
      'extracted-markdown',
      'extracted-json',
      'embeddings',
      'review-artifacts'
    )
  )
  WITH CHECK (
    bucket_id IN (
      'raw-pdfs',
      'raw-source',
      'extracted-markdown',
      'extracted-json',
      'embeddings',
      'review-artifacts'
    )
  );

-- Anon SELECT — raw-pdfs (whole bucket).
CREATE POLICY "raw_pdfs_anon_read"
  ON storage.objects
  FOR SELECT
  TO anon, authenticated
  USING (bucket_id = 'raw-pdfs');

-- Anon SELECT — extracted-markdown (whole bucket).
CREATE POLICY "extracted_markdown_anon_read"
  ON storage.objects
  FOR SELECT
  TO anon, authenticated
  USING (bucket_id = 'extracted-markdown');

-- Anon SELECT — extracted-json: only `<arxiv_id>/figures/...`.
CREATE POLICY "extracted_json_figures_anon_read"
  ON storage.objects
  FOR SELECT
  TO anon, authenticated
  USING (
    bucket_id = 'extracted-json'
    AND position('/figures/' in name) > 0
  );

-- Anon SELECT — review-artifacts: only top-level `<review_id>.json` (not
-- tool_call_log under a sub-path). Whether a specific review is actually
-- published is enforced at the reviews-table layer; the web app only ever
-- presigns this for status='published' rows.
CREATE POLICY "review_artifacts_published_read"
  ON storage.objects
  FOR SELECT
  TO anon, authenticated
  USING (
    bucket_id = 'review-artifacts'
    AND name ~ '^[^/]+\.json$'
  );

-- raw-source, embeddings, and review-artifacts/<id>/tool_call_log.jsonl
-- have no anon policy — RLS denies reads by default for those rows.
