-- GrokRxiv — review corrections / withdrawals / clarifications.
-- A moderator-issued record attached to a previously-published review.
-- When a review enters `corrected` status, one or more rows in this table
-- explain why; rows are public for public reviews.

create table if not exists corrections (
  id                  uuid primary key default gen_random_uuid(),
  review_id           uuid not null references reviews(id) on delete cascade,
  kind                text not null
                        check (kind in ('correction','withdrawal','clarification')),
  rationale_md        text not null,
  author_dispute_url  text,
  created_by          text not null,  -- moderator handle
  created_at          timestamptz not null default now()
);

create index if not exists corrections_review_created_idx
  on corrections(review_id, created_at desc);

alter table corrections enable row level security;

drop policy if exists corrections_public_read on corrections;
create policy corrections_public_read on corrections
  for select
  to anon, authenticated
  using (
    exists (
      select 1 from reviews r
      where r.id = corrections.review_id
        and r.status in ('published','corrected')
    )
  );

drop policy if exists corrections_service_write on corrections;
create policy corrections_service_write on corrections
  for all
  to service_role
  using (true)
  with check (true);
