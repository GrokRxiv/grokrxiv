-- Rename the first DAG app projection tables from generic "research" wording
-- to the concrete GrokRxiv app namespace. Execution state remains in the
-- generic AgentHero app_runs/dag_runs/dag_run_nodes tables.

do $$
begin
  if to_regclass('public.research_moderation_queue') is not null
     and to_regclass('public.grokrxiv_moderation_queue') is null then
    alter table research_moderation_queue rename to grokrxiv_moderation_queue;
  end if;

  if to_regclass('public.research_reviews') is not null
     and to_regclass('public.grokrxiv_reviews') is null then
    alter table research_reviews rename to grokrxiv_reviews;
  end if;

  if to_regclass('public.research_sources') is not null
     and to_regclass('public.grokrxiv_sources') is null then
    alter table research_sources rename to grokrxiv_sources;
  end if;
end $$;

alter index if exists research_sources_source_uidx
  rename to grokrxiv_sources_source_uidx;
alter index if exists research_sources_field_idx
  rename to grokrxiv_sources_field_idx;

alter index if exists research_reviews_source_created_idx
  rename to grokrxiv_reviews_source_created_idx;
alter index if exists research_reviews_state_idx
  rename to grokrxiv_reviews_state_idx;
alter index if exists research_reviews_app_run_idx
  rename to grokrxiv_reviews_app_run_idx;
alter index if exists research_reviews_dag_run_idx
  rename to grokrxiv_reviews_dag_run_idx;

alter index if exists research_moderation_queue_state_idx
  rename to grokrxiv_moderation_queue_state_idx;
alter index if exists research_moderation_queue_review_idx
  rename to grokrxiv_moderation_queue_review_idx;
