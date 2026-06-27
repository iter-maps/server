use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use iter_contracts::offline;
use iter_core::config;

/// Gateway configuration, entirely env-derived (`.env` for "clone + up").
/// Fields are added as the capabilities that consume them land.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub bind: SocketAddr,
    /// Internal URL of the OTP routing engine.
    pub otp_url: String,
    /// Internal URL of the Photon geocoder.
    pub photon_url: String,
    /// ViaggiaTreno base (RFI's unofficial live-train API).
    pub viaggiatreno_url: String,
    /// Default ViaggiaTreno region code for the station list (Lazio = 5).
    pub trenitalia_region: i64,
    /// Upstream request timeout.
    pub upstream_timeout: std::time::Duration,
    /// Reported in the client health document.
    pub version: String,
    /// Root of the read-only artifact tree the pipeline produces.
    pub data_dir: PathBuf,
    /// Pipeline-written client health document.
    pub health_path: PathBuf,
    pub tiles_dir: PathBuf,
    /// The basemap archive's filename, derived from the resolved region id
    /// (`rome.pmtiles`) — kept in sync with what the pipeline writes (ADR 0008).
    pub tiles_basename: String,
    pub styles_dir: PathBuf,
    pub glyphs_dir: PathBuf,
    pub sprite_dir: PathBuf,
    pub overlays_dir: PathBuf,
    /// Overlay kinds the resolved region declares (drives the served allowlist).
    pub overlay_kinds: Vec<String>,
    /// The region's country (first segment of the region path, e.g. `italy`).
    /// Selects the country-specific drivers — address normalizer today (ADR 0017).
    pub region_country: String,
    pub offline: OfflineCaps,
    /// Build-time addressed-POI index for place correlation (`/places/related`).
    pub places_path: PathBuf,
    /// The clustered PMTiles archive offline range-extracts read from.
    pub offline_source: PathBuf,
    /// `go-pmtiles` binary used for the range-extract (concept ADR 0010).
    pub pmtiles_bin: String,
}

/// Abuse-protection caps — the only protection on the public, auth-less
/// offline surface.
#[derive(Debug, Clone)]
pub struct OfflineCaps {
    pub max_area_deg2: f64,
    pub max_zoom: u8,
    pub max_concurrent: usize,
}

impl GatewayConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let host = config::or("GATEWAY_HOST", "0.0.0.0");
        let port: u16 = config::parse("GATEWAY_PORT", 8090);
        let data_dir = PathBuf::from(config::or("DATA_DIR", "/data"));
        let tiles_dir = dir("TILES_DIR", &data_dir, "output/tiles");

        // Resolve the region the same way the pipeline does, so the gateway and
        // the build tier read one source of truth — the basemap name, overlay
        // kinds, and live-train region can never drift (ADR 0008 / 0013).
        let regions_dir = PathBuf::from(config::or("REGIONS_DIR", "regions"));
        let target = config::or("ITER_REGION", "italy/lazio/rome");
        let region = iter_region::resolve(&regions_dir, &target)
            .with_context(|| format!("resolving region '{target}'"))?;
        let tiles_basename = format!("{}.pmtiles", region.id);
        let overlay_kinds: Vec<String> = region.overlays.iter().map(|o| o.kind.clone()).collect();
        // The country is the root of the region path (`italy/lazio/rome` → `italy`).
        let region_country = target.split('/').next().unwrap_or_default().to_string();

        Ok(Self {
            bind: format!("{host}:{port}").parse()?,
            otp_url: config::or("OTP_URL", "http://otp:8080"),
            photon_url: config::or("PHOTON_URL", "http://photon:2322"),
            viaggiatreno_url: config::or(
                "VIAGGIATRENO_URL",
                "http://www.viaggiatreno.it/infomobilita/resteasy/viaggiatreno",
            ),
            // Seed from the region's live-train code; env still overrides.
            trenitalia_region: config::parse(
                "TRENITALIA_REGION",
                region.live_trains.region_code.unwrap_or(5),
            ),
            upstream_timeout: std::time::Duration::from_secs(config::parse(
                "UPSTREAM_TIMEOUT_SECS",
                30,
            )),
            version: config::or("ITER_VERSION", env!("CARGO_PKG_VERSION")),
            styles_dir: dir("STYLES_DIR", &data_dir, "output/styles"),
            glyphs_dir: dir("GLYPHS_DIR", &data_dir, "static/glyphs"),
            sprite_dir: dir("SPRITE_DIR", &data_dir, "static/sprite"),
            overlays_dir: dir("OVERLAYS_DIR", &data_dir, "output/overlays"),
            health_path: dir("HEALTH_PATH", &data_dir, "output/health.json"),
            offline: OfflineCaps {
                max_area_deg2: config::parse(
                    "OFFLINE_MAX_AREA_DEG2",
                    offline::DEFAULT_MAX_AREA_DEG2,
                ),
                max_zoom: config::parse("OFFLINE_MAX_ZOOM", offline::DEFAULT_MAX_ZOOM),
                max_concurrent: config::parse(
                    "OFFLINE_MAX_CONCURRENT",
                    offline::DEFAULT_MAX_CONCURRENT,
                ),
            },
            places_path: dir("PLACES_PATH", &data_dir, "output/places.jsonl"),
            offline_source: config::opt("OFFLINE_PMTILES_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| tiles_dir.join(&tiles_basename)),
            pmtiles_bin: config::or("OFFLINE_PMTILES_BIN", "pmtiles"),
            overlay_kinds,
            region_country,
            tiles_basename,
            tiles_dir,
            data_dir,
        })
    }
}

/// An artifact directory: explicit env override, else derived from `data_dir`.
fn dir(env_key: &str, data_dir: &Path, rel: &str) -> PathBuf {
    config::opt(env_key)
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join(rel))
}
