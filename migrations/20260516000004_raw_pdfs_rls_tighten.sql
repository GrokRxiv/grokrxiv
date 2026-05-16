-- FP-RPT3c C1 — tighten `raw-pdfs` anon SELECT to per-key gating.
--
-- The previous policy (see `20260516000002_paper_assets_bucket.sql:50–54`)
-- allowed anon SELECT on the whole `raw-pdfs` bucket, which leaked every
-- ingested PDF to anyone who could guess (or enumerate) an arXiv id. This
-- migration replaces it with a per-row policy that only permits anon SELECT
-- when there is a corresponding review in a publicly visible status.
--
-- Object naming convention (see `crates/storage/src/paper_artifacts.rs:196`):
--   bucket=`raw-pdfs`, name=`<arxiv_id>.pdf`
--
-- `paper_assets.storage_prefix` is set to `<arxiv_id>` (no extension). The
-- arXiv id itself contains a dot (e.g. `2605.00403`), so `SPLIT_PART(name,
-- '.', 1)` would lop off the second segment. We strip the trailing `.pdf`
-- with a regex instead.
--
-- Implementation note: the policy uses a `SECURITY DEFINER` helper because
-- the anon role does NOT have SELECT on `paper_assets` / `papers` /
-- `reviews` (see `20250513000002_rls.sql`). The helper runs under postgres
-- and only exposes a boolean — no row data leaks.

DROP POLICY IF EXISTS "raw_pdfs_anon_read"  ON storage.objects;
DROP POLICY IF EXISTS "anon_pdf_read"       ON storage.objects;

CREATE OR REPLACE FUNCTION public.raw_pdfs_object_is_public(object_name text)
RETURNS boolean
LANGUAGE sql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
  SELECT EXISTS (
    SELECT 1
    FROM paper_assets pa
    JOIN papers  p ON p.id  = pa.paper_id
    JOIN reviews r ON r.paper_id = p.id
    WHERE pa.storage_prefix = regexp_replace(object_name, '\.pdf$', '')
      AND r.status IN ('published', 'corrected')
  );
$$;

REVOKE ALL ON FUNCTION public.raw_pdfs_object_is_public(text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.raw_pdfs_object_is_public(text) TO anon, authenticated, service_role;

CREATE POLICY "raw_pdfs_anon_read"
  ON storage.objects
  FOR SELECT
  TO anon, authenticated
  USING (
    bucket_id = 'raw-pdfs'
    AND public.raw_pdfs_object_is_public(name)
  );
