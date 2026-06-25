-- Add durable approval/provenance columns to existing AgentHero runtime DBs.

alter table app_runs drop constraint if exists app_runs_state_check;
alter table app_runs add constraint app_runs_state_check
  check (state in (
    'queued','running','awaiting_approval','partial','done','failed',
    'cancelled','system_failed'
  ));

alter table dag_runs drop constraint if exists dag_runs_state_check;
alter table dag_runs add constraint dag_runs_state_check
  check (state in (
    'queued','running','awaiting_approval','partial','done','failed',
    'cancelled','system_failed'
  ));

alter table dag_run_nodes drop constraint if exists dag_run_nodes_state_check;
alter table dag_run_nodes add constraint dag_run_nodes_state_check
  check (state in (
    'queued','running','awaiting_approval','ok','degraded','skipped',
    'failed','cancelled','system_failed'
  ));

alter table dag_run_nodes add column if not exists prompt_hash text;
alter table dag_run_nodes add column if not exists command jsonb not null default '[]'::jsonb;
alter table dag_run_nodes add column if not exists exit_status int;
alter table dag_run_nodes add column if not exists policy jsonb not null default '{}'::jsonb;
alter table dag_run_nodes add column if not exists input_refs jsonb not null default '{}'::jsonb;
alter table dag_run_nodes add column if not exists output_refs jsonb not null default '{}'::jsonb;
alter table dag_run_nodes add column if not exists diagnostic_refs jsonb not null default '{}'::jsonb;
