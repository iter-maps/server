use std::sync::Arc;

use iter_contracts::live_trains::{BoardEntry, Station};
use iter_contracts::places::Place;

use iter_region_drivers::{LiveTrainsProvider, address_normalizer, live_trains_provider};

use crate::cache::TtlCache;
use crate::config::GatewayConfig;
use crate::correlate::CorrelationIndex;
use crate::reliability_cache::Tier2Cache;
use crate::weather::{WeatherCache, WeatherClient};

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
    /// Build-time addressed-POI index for place correlation (loaded once).
    pub correlations: Arc<CorrelationIndex>,
    /// Country-specific live-trains provider, selected from the resolved region's
    /// country (ADR 0017). The generic handlers dispatch through it.
    pub live_trains: Arc<dyn LiveTrainsProvider>,
    /// mtime-validated memo of the parsed Tier-2 reliability map, shared by the
    /// read endpoint, the reranker, and the no-RT annotator (ADR 0032). Derived,
    /// disposable soft-state — rebuilt from `tier2.json` on restart.
    pub reliability: Arc<Tier2Cache>,
    /// Open-Meteo client for the opt-in weather rerank factor (ADR 0033), or
    /// `None` when `WEATHER_API_URL` is unset — the default-off posture, in which
    /// case the factor is always neutral and no outbound call is ever made.
    pub weather_client: Option<WeatherClient>,
    /// Bounded TTL memo of forecasts keyed by coarse `(lat, lon, hour)`. Derived,
    /// disposable soft-state holding only coarse public weather (ADR 0033).
    pub weather_cache: Arc<WeatherCache>,
}

impl AppState {
    pub fn new(cfg: GatewayConfig) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(cfg.upstream_timeout)
            .user_agent(concat!("iter-gateway/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let offline_gate = Arc::new(tokio::sync::Semaphore::new(cfg.offline.max_concurrent));
        // The correlation bucketing is country-specific; pick the driver for the
        // resolved region's country (ADR 0017).
        let normalizer = address_normalizer(&cfg.region_country);
        let correlations = Arc::new(CorrelationIndex::load(&cfg.places_path, normalizer));
        tracing::info!(
            addressed_places = correlations.len(),
            country = %cfg.region_country,
            "loaded place correlation index"
        );
        // The live-trains upstream is country-specific; pick the provider for the
        // resolved region's country and hand it the env-supplied base URL/region
        // (it owns the defaults for both) — same pattern as the normalizer above.
        let live_trains = live_trains_provider(
            &cfg.region_country,
            cfg.viaggiatreno_url.clone(),
            cfg.trenitalia_region,
        );
        let reliability = Arc::new(Tier2Cache::new(cfg.reliability_dir.clone()));
        // The weather client is built only when a base URL is configured; absent
        // it, weather stays default-off (ADR 0033). It reuses the pooled `http`.
        let weather_client = cfg
            .weather_api_url
            .clone()
            .map(|url| WeatherClient::new(http.clone(), url));
        Ok(Self {
            cfg: Arc::new(cfg),
            http,
            boards: Arc::new(TtlCache::new()),
            stations: Arc::new(TtlCache::new()),
            places: Arc::new(TtlCache::new()),
            offline_gate,
            correlations,
            live_trains,
            reliability,
            weather_client,
            weather_cache: Arc::new(WeatherCache::new()),
        })
    }
}
