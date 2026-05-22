//! Generic AgentHero HTTP service.

/// Run the generic platform HTTP API.
pub async fn run() -> anyhow::Result<()> {
    let cfg = crate::config::Config::from_env()?;
    let pool = match cfg.database_url.as_deref() {
        Some(url) => Some(
            sqlx::postgres::PgPoolOptions::new()
                .max_connections((cfg.scheduler_workers as u32).saturating_add(4))
                .connect_lazy(url)?,
        ),
        None => None,
    };
    if let Some(pool) = pool.clone() {
        crate::scheduler::spawn(
            pool,
            crate::scheduler::SchedulerConfig {
                workers: cfg.scheduler_workers,
                poll_interval: std::time::Duration::from_secs(2),
            },
        );
    } else {
        tracing::warn!("scheduler disabled because DATABASE_URL is not configured");
    }

    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    tracing::info!(bind = %cfg.bind, "serving AgentHero platform API");
    axum::serve(
        listener,
        crate::router_with_state(crate::PlatformState {
            pool,
            service_token: cfg.service_token,
        }),
    )
    .await?;
    Ok(())
}
