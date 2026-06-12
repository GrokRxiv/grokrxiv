-- GrokRxiv projection/cache tables contain in-progress review state and paper
-- extracts. They are private executor/product state; public reads go through
-- the legacy `papers`/`reviews` projections with explicit published-review
-- policies.

alter table if exists grokrxiv_sources enable row level security;
alter table if exists grokrxiv_reviews enable row level security;
alter table if exists grokrxiv_moderation_queue enable row level security;
alter table if exists review_inputs enable row level security;
alter table if exists review_cache enable row level security;

revoke all on table
  grokrxiv_sources,
  grokrxiv_reviews,
  grokrxiv_moderation_queue,
  review_inputs,
  review_cache
from anon, authenticated;

drop policy if exists grokrxiv_sources_service_all on grokrxiv_sources;
create policy grokrxiv_sources_service_all on grokrxiv_sources
  for all to service_role using (true) with check (true);

drop policy if exists grokrxiv_reviews_service_all on grokrxiv_reviews;
create policy grokrxiv_reviews_service_all on grokrxiv_reviews
  for all to service_role using (true) with check (true);

drop policy if exists grokrxiv_moderation_queue_service_all on grokrxiv_moderation_queue;
create policy grokrxiv_moderation_queue_service_all on grokrxiv_moderation_queue
  for all to service_role using (true) with check (true);

drop policy if exists review_inputs_service_all on review_inputs;
create policy review_inputs_service_all on review_inputs
  for all to service_role using (true) with check (true);

drop policy if exists review_cache_service_all on review_cache;
create policy review_cache_service_all on review_cache
  for all to service_role using (true) with check (true);
