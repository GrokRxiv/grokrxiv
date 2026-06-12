-- AgentHero generic runtime tables are executor state, not public API data.
-- Service-role workers may read/write them; anon/authenticated PostgREST
-- clients must not enumerate app runs, DAG artifacts, leases, or cached agent
-- outputs.

alter table if exists app_runs enable row level security;
alter table if exists dag_runs enable row level security;
alter table if exists dag_run_nodes enable row level security;
alter table if exists dag_artifacts enable row level security;
alter table if exists dag_events enable row level security;
alter table if exists worker_nodes enable row level security;
alter table if exists worker_leases enable row level security;
alter table if exists agent_output_cache enable row level security;

revoke all on table
  app_runs,
  dag_runs,
  dag_run_nodes,
  dag_artifacts,
  dag_events,
  worker_nodes,
  worker_leases,
  agent_output_cache
from anon, authenticated;

drop policy if exists app_runs_service_all on app_runs;
create policy app_runs_service_all on app_runs
  for all to service_role using (true) with check (true);

drop policy if exists dag_runs_service_all on dag_runs;
create policy dag_runs_service_all on dag_runs
  for all to service_role using (true) with check (true);

drop policy if exists dag_run_nodes_service_all on dag_run_nodes;
create policy dag_run_nodes_service_all on dag_run_nodes
  for all to service_role using (true) with check (true);

drop policy if exists dag_artifacts_service_all on dag_artifacts;
create policy dag_artifacts_service_all on dag_artifacts
  for all to service_role using (true) with check (true);

drop policy if exists dag_events_service_all on dag_events;
create policy dag_events_service_all on dag_events
  for all to service_role using (true) with check (true);

drop policy if exists worker_nodes_service_all on worker_nodes;
create policy worker_nodes_service_all on worker_nodes
  for all to service_role using (true) with check (true);

drop policy if exists worker_leases_service_all on worker_leases;
create policy worker_leases_service_all on worker_leases
  for all to service_role using (true) with check (true);

drop policy if exists agent_output_cache_service_all on agent_output_cache;
create policy agent_output_cache_service_all on agent_output_cache
  for all to service_role using (true) with check (true);
