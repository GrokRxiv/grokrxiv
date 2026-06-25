fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crate lives under crates/orchestrator")
        .to_path_buf()
}

#[test]
fn runtime_tables_allow_approval_and_dedicated_node_provenance() {
    let root = repo_root();
    let sql = std::fs::read_to_string(
        root.join("agenthero/migrations/20260522000002_app_runtime_tables.sql"),
    )
    .expect("platform runtime migration should exist");
    let supabase_sql = std::fs::read_to_string(
        root.join("supabase/migrations/20260522000002_app_runtime_tables.sql"),
    )
    .expect("supabase runtime migration should exist");
    assert_eq!(
        sql, supabase_sql,
        "platform and supabase migration views must stay identical"
    );
    let compact = sql.split_whitespace().collect::<Vec<_>>().join(" ");

    for state in [
        "'queued'",
        "'running'",
        "'awaiting_approval'",
        "'partial'",
        "'done'",
        "'failed'",
        "'cancelled'",
        "'system_failed'",
    ] {
        assert!(
            compact.contains(state),
            "runtime state constraints must include {state}"
        );
    }

    for column in [
        "manifest_version int",
        "manifest_hash text",
        "input jsonb not null default '{}'::jsonb",
        "model text",
        "prompt_hash text",
        "command jsonb not null default '[]'::jsonb",
        "exit_status int",
        "policy jsonb not null default '{}'::jsonb",
        "input_refs jsonb not null default '{}'::jsonb",
        "output_refs jsonb not null default '{}'::jsonb",
        "diagnostic_refs jsonb not null default '{}'::jsonb",
    ] {
        assert!(
            compact.contains(column),
            "runtime migration must expose dedicated provenance column `{column}`"
        );
    }
}

#[test]
fn runtime_provenance_forward_migration_upgrades_existing_tables() {
    let root = repo_root();
    let sql = std::fs::read_to_string(
        root.join("agenthero/migrations/20260624000001_runtime_provenance_approval.sql"),
    )
    .expect("platform provenance migration should exist");
    let supabase_sql = std::fs::read_to_string(
        root.join("supabase/migrations/20260624000001_runtime_provenance_approval.sql"),
    )
    .expect("supabase provenance migration should exist");
    assert_eq!(
        sql, supabase_sql,
        "platform and supabase provenance migrations must stay identical"
    );
    let compact = sql.split_whitespace().collect::<Vec<_>>().join(" ");

    for required in [
        "alter table app_runs drop constraint if exists app_runs_state_check",
        "alter table app_runs add constraint app_runs_state_check",
        "alter table dag_runs drop constraint if exists dag_runs_state_check",
        "alter table dag_runs add constraint dag_runs_state_check",
        "alter table dag_run_nodes drop constraint if exists dag_run_nodes_state_check",
        "alter table dag_run_nodes add constraint dag_run_nodes_state_check",
        "'awaiting_approval'",
        "alter table dag_run_nodes add column if not exists prompt_hash text",
        "alter table dag_run_nodes add column if not exists command jsonb not null default '[]'::jsonb",
        "alter table dag_run_nodes add column if not exists exit_status int",
        "alter table dag_run_nodes add column if not exists policy jsonb not null default '{}'::jsonb",
        "alter table dag_run_nodes add column if not exists input_refs jsonb not null default '{}'::jsonb",
        "alter table dag_run_nodes add column if not exists output_refs jsonb not null default '{}'::jsonb",
        "alter table dag_run_nodes add column if not exists diagnostic_refs jsonb not null default '{}'::jsonb",
    ] {
        assert!(
            compact.contains(required),
            "provenance migration must contain `{required}`"
        );
    }
}
