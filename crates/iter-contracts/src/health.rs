//! Health and freshness documents. The static `health.json` has five exact
//! fields the client reads to show an "update app" banner; the gateway health
//! is operator diagnostics; the freshness manifest drives server→client
//! artifact sync (concept 18 §4).

use iter_core::Status;
use serde::Serialize;

/// `GET /health` (and `/health.json`) on the static surface — five exact fields.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticHealth {
    pub status: Status,
    pub version: String,
    pub gtfs_loaded: bool,
    pub tiles_built_at: Option<String>,
    pub bootstrapped_at: Option<String>,
}

/// `GET /health` on the gateway — diagnostics announcing mounted providers.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayHealth {
    pub status: Status,
    pub providers: Vec<String>,
    pub offline: Vec<String>,
    pub fl_gtfs: FlGtfsState,
}

/// `GET /gtfs/status` — FL NeTEx→GTFS build state.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlGtfsState {
    pub built_at: Option<String>,
    pub building: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<FlGtfsStats>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlGtfsStats {
    pub journeys_total: u64,
    pub trips: u64,
    pub stops: u64,
    pub routes: u64,
    pub shaped_routes: u64,
    pub skipped: u64,
}

/// Server→client freshness manifest: lets cache-first clients check artifact
/// staleness with one request instead of revalidating each surface.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FreshnessManifest {
    pub api_version: String,
    pub generated_at: String,
    pub artifacts: std::collections::BTreeMap<String, ArtifactFreshness>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactFreshness {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
}
