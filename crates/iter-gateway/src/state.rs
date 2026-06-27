use std::sync::Arc;

use crate::config::GatewayConfig;

/// Shared, cheaply-cloneable handle for axum handlers. The gateway is
/// stateless across requests (no per-client state), so replicas scale
/// horizontally with no coordination.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<GatewayConfig>,
}

impl AppState {
    pub fn new(cfg: GatewayConfig) -> Self {
        Self { cfg: Arc::new(cfg) }
    }
}
