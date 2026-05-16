-- RPT2 Track F: per-agent revision artifact storage.
--
-- One row per agent that emitted a revision_artifact (i.e. one row per
-- specialist when mode=review_and_revise + the meta_reviewer if it also
-- proposed patches). The supervisor's `apply_revisions` function reads these
-- rows to materialise a draft PR with per-patch accept/reject checkboxes.

CREATE TABLE IF NOT EXISTS revision_patches (
    id                BIGSERIAL PRIMARY KEY,
    review_id         UUID NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    review_agent_id   UUID NOT NULL REFERENCES review_agents(id) ON DELETE CASCADE,
    target            TEXT NOT NULL CHECK (target IN ('paper_latex', 'grokrxiv_review_output')),
    patches           JSONB NOT NULL,
    accepted_indices  INTEGER[] NOT NULL DEFAULT '{}',
    applied_pr_url    TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    applied_at        TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS revision_patches_review_id_idx ON revision_patches(review_id);

-- RLS: service-role only for now. Public read comes with FP7/FP8.
ALTER TABLE revision_patches ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS revision_patches_service_only ON revision_patches;
CREATE POLICY revision_patches_service_only ON revision_patches
    FOR ALL TO service_role USING (true) WITH CHECK (true);
