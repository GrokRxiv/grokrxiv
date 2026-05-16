-- FP6 A4: per-paper agent output cache.
--
-- Re-reviewing the same paper previously re-ran all six agents. The cache
-- keys on (paper_id, role, content_hash) so an identical input artifact for
-- a role returns the previous successful output instead of round-tripping
-- through the LLM. We only cache `verifier_status = 'pass'` rows; warn/fail
-- results stay uncached so a corrected verifier run actually re-executes the
-- agent.

create table if not exists review_cache (
    id              bigserial primary key,
    paper_id        uuid not null references papers(id) on delete cascade,
    role            text not null,
    content_hash    text not null,
    output          jsonb not null,
    verifier_status text not null,
    model           text not null,
    tokens_in       integer,
    tokens_out      integer,
    created_at      timestamptz not null default now(),
    expires_at      timestamptz not null default (now() + interval '30 days')
);

create unique index if not exists idx_review_cache_lookup
    on review_cache(paper_id, role, content_hash);
create index if not exists idx_review_cache_expires
    on review_cache(expires_at);
