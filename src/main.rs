use realtime_ai_translation::{config::AppConfig, server};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let config = AppConfig::from_env()?;
    let listen = config.listen;
    let state = server::AppState::new(config)?;
    let listener = tokio::net::TcpListener::bind(listen).await?;
    info!(url = %format!("http://{listen}"), "real-time translation server ready");
    axum::serve(listener, server::router(state)).await?;
    Ok(())
}
