mod config;
mod glyphs;
mod health;
mod http;
mod router;
mod sprite;
mod state;
mod styles;
mod tiles;

use config::GatewayConfig;
use state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    iter_core::telemetry::init("iter-gateway");

    let cfg = GatewayConfig::from_env()?;
    let bind = cfg.bind;
    let app = router::build(AppState::new(cfg));

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, version = env!("CARGO_PKG_VERSION"), "iter-gateway listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(iter_core::shutdown::signal())
        .await?;

    Ok(())
}
