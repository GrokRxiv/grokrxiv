#[test]
fn app_runtime_migration_declares_generic_tables_once() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("workspace root")
        .to_path_buf();
    let sql =
        std::fs::read_to_string(root.join("migrations/20260522000002_app_runtime_tables.sql"))
            .expect("app runtime migration should exist");
    let supabase_sql = std::fs::read_to_string(
        root.join("supabase/migrations/20260522000002_app_runtime_tables.sql"),
    )
    .expect("supabase app runtime migration should exist");
    assert_eq!(
        sql, supabase_sql,
        "root and supabase migration copies must stay identical"
    );
    let compact = sql.split_whitespace().collect::<Vec<_>>().join(" ");

    for table in [
        "app_runs",
        "dag_runs",
        "dag_run_nodes",
        "dag_artifacts",
        "dag_events",
        "worker_nodes",
        "worker_leases",
        "agent_output_cache",
        "research_sources",
        "research_reviews",
        "research_moderation_queue",
    ] {
        assert!(
            sql.contains(&format!("create table if not exists {table}")),
            "migration must create generic/projection table `{table}`"
        );
    }

    assert!(compact.contains("app_id text not null"));
    assert!(compact.contains("dag_type text not null"));
    assert!(compact.contains("node_id text not null"));
    assert!(compact.contains("references app_runs(id)"));
    assert!(compact.contains("references dag_runs(id)"));
}
