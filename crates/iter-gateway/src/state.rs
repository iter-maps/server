use std::sync::Arc;

use crate::config::GatewayConfig;

/// Shared, cheaply-cloneable handle for axum handlers. The gateway is
/// stateless across requests (no per-client state), so replicas scale
/// horizontally with no coordination.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<GatewayConfig>,
    /// Pooled client for upstream calls (OTP, Photon, ViaggiaTreno).
    pub http: reqwest::Client,
}

impl AppState {
    pub fn new(cfg: GatewayConfig) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(cfg.upstream_timeout)
            .user_agent(concat!("iter-gateway/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { cfg: Arc::new(cfg), http })
    }
}
