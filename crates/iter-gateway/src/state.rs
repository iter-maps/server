use std::sync::Arc;

use iter_contracts::live_trains::{BoardEntry, Station};
use iter_contracts::places::Place;

use crate::cache::TtlCache;
use crate::config::GatewayConfig;

/// Shared, cheaply-cloneable handle for axum handlers. The gateway is
/// stateless across requests (the caches below are derived, disposable upstream
/// responses — not user state), so replicas scale horizontally.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<GatewayConfig>,
    /// Pooled client for upstream calls (OTP, Photon, ViaggiaTreno).
    pub http: reqwest::Client,
    /// TTL + single-flight cache for live-train boards.
    pub boards: Arc<TtlCache<Vec<BoardEntry>>>,
    /// TTL + single-flight cache for station lookups.
    pub stations: Arc<TtlCache<Vec<Station>>>,
    /// TTL + single-flight cache for enriched places (facts change slowly; this
    /// also shields the rate-limited Wikimedia upstreams).
    pub places: Arc<TtlCache<Place>>,
    /// Concurrency gate for the heavy offline extracts (no queue → 503 BUSY).
    pub offline_gate: Arc<tokio::sync::Semaphore>,
}

impl AppState {
    pub fn new(cfg: GatewayConfig) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(cfg.upstream_timeout)
            .user_agent(concat!("iter-gateway/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let offline_gate = Arc::new(tokio::sync::Semaphore::new(cfg.offline.max_concurrent));
        Ok(Self {
            cfg: Arc::new(cfg),
            http,
            boards: Arc::new(TtlCache::new()),
            stations: Arc::new(TtlCache::new()),
            places: Arc::new(TtlCache::new()),
            offline_gate,
        })
    }
}
