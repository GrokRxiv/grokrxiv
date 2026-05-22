#[test]
fn app_runtime_migration_declares_generic_tables_once() {
    let root = repo_root();
    let sql = std::fs::read_to_string(
        root.join("agenthero/migrations/20260522000002_app_runtime_tables.sql"),
    )
    .expect("app runtime migration should exist");
    let supabase_sql = std::fs::read_to_string(
        root.join("supabase/migrations/20260522000002_app_runtime_tables.sql"),
    )
    .expect("supabase app runtime migration should exist");
    assert_eq!(
        sql, supabase_sql,
        "platform and supabase migration views must stay identical"
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
    ] {
        assert!(
            sql.contains(&format!("create table if not exists {table}")),
            "migration must create generic runtime table `{table}`"
        );
    }

    for app_table in [
        "research_sources",
        "research_reviews",
        "research_moderation_queue",
        "grokrxiv_sources",
        "grokrxiv_reviews",
        "grokrxiv_moderation_queue",
    ] {
        assert!(
            !sql.contains(app_table),
            "platform migration must not create app projection table `{app_table}`"
        );
    }

    assert!(compact.contains("app_id text not null"));
    assert!(compact.contains("dag_type text not null"));
    assert!(compact.contains("node_id text not null"));
    assert!(compact.contains("references app_runs(id)"));
    assert!(compact.contains("references dag_runs(id)"));
}

#[test]
fn grokrxiv_projection_migration_owns_grokrxiv_tables() {
    let root = repo_root();
    let sql = std::fs::read_to_string(
        root.join("agenthero/apps/grokrxiv/migrations/20260522000003_agenthero_grokrxiv_projection_rename.sql"),
    )
    .expect("AgentHero projection rename migration should exist");
    let supabase_sql = std::fs::read_to_string(
        root.join("supabase/migrations/20260522000003_agenthero_grokrxiv_projection_rename.sql"),
    )
    .expect("supabase AgentHero projection rename migration should exist");
    assert_eq!(
        sql, supabase_sql,
        "app and supabase migration views must stay identical"
    );

    for required in [
        "create table if not exists grokrxiv_sources",
        "create table if not exists grokrxiv_reviews",
        "create table if not exists grokrxiv_moderation_queue",
        "research_sources rename to grokrxiv_sources",
        "research_reviews rename to grokrxiv_reviews",
        "research_moderation_queue rename to grokrxiv_moderation_queue",
        "research_reviews_state_idx",
        "grokrxiv_reviews_state_idx",
        "insert into grokrxiv_sources",
        "insert into grokrxiv_reviews",
        "insert into grokrxiv_moderation_queue",
    ] {
        assert!(
            sql.contains(required),
            "migration must contain `{required}`"
        );
    }
}

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(5)
        .expect("repo root")
        .to_path_buf()
}
