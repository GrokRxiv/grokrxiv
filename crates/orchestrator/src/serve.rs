//! Generic AgentHero HTTP service.

/// Run the generic platform HTTP API.
pub async fn run() -> anyhow::Result<()> {
    let cfg = crate::config::Config::from_env()?;
    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    tracing::info!(bind = %cfg.bind, "serving AgentHero platform API");
    axum::serve(listener, crate::router()).await?;
    Ok(())
}
