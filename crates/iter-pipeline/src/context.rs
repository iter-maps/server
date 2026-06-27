use std::path::PathBuf;

use anyhow::Context as _;
use iter_core::config;
use iter_region::Resolved;

/// Pipeline run context. The active region (`ITER_REGION`) is resolved once;
/// every step reads what it needs from `region`. The `FORCE_*`/`SKIP_*` knobs
/// key off step names, so a single artifact can be refreshed
/// (`FORCE_TILES=true`) or opted out (`SKIP_PHOTON=true`) without rebuilding
/// everything.
pub struct Context {
    pub data_dir: PathBuf,
    pub version: String,
    pub region: Resolved,
    pub http: reqwest::Client,
}

impl Context {
    pub fn from_env() -> anyhow::Result<Self> {
        let regions_dir = PathBuf::from(config::or("REGIONS_DIR", "regions"));
        let target = config::or("ITER_REGION", "italy/lazio/rome");
        let region = iter_region::resolve(&regions_dir, &target)
            .with_context(|| format!("resolving region '{target}'"))?;

        let http = reqwest::Client::builder()
            .user_agent(concat!("iter-pipeline/", env!("CARGO_PKG_VERSION")))
            .build()?;

        Ok(Self {
            data_dir: PathBuf::from(config::or("DATA_DIR", "/data")),
            version: config::or("ITER_VERSION", env!("CARGO_PKG_VERSION")),
            region,
            http,
        })
    }

    pub fn forced(&self, step: &str) -> bool {
        config::flag("FORCE_ALL", false) || config::flag(&format!("FORCE_{step}"), false)
    }

    pub fn skipped(&self, step: &str) -> bool {
        config::flag(&format!("SKIP_{step}"), false)
    }

    pub fn output(&self, rel: &str) -> PathBuf {
        self.data_dir.join(rel)
    }

    /// Basemap render extent: the region's, overridable by `PMTILES_BOUNDS`
    /// (e.g. to shrink an all-Italy basemap to fit a small host).
    pub fn basemap_bounds(&self) -> Option<String> {
        config::opt("PMTILES_BOUNDS").or_else(|| self.region.extents.basemap.clone())
    }

    /// The basemap artifact name, derived from the region id (`rome.pmtiles`).
    pub fn tiles_filename(&self) -> String {
        format!("{}.pmtiles", self.region.id)
    }

    /// Transit-routing clip extent: the region's `routing`, overridable by
    /// `ROUTING_BOUNDS` (e.g. to shrink a Lazio clip to central Rome on a small
    /// host). `None` means the region has no transit routing — the OTP steps
    /// then no-op, so a basemap-only deploy (e.g. `italy`) builds tiles alone.
    pub fn routing_bounds(&self) -> Option<String> {
        config::opt("ROUTING_BOUNDS").or_else(|| self.region.extents.routing.clone())
    }

    /// OTP's base directory: the clipped OSM, the GTFS feeds, `build-config.json`
    /// and the built `graph.obj` all live here, so `otp --load --serve` finds
    /// them together.
    pub fn graph_dir(&self) -> PathBuf {
        self.output("graph")
    }

    /// The clipped street-network artifact name, region-derived
    /// (`rome.osm.pbf`). OTP auto-detects the `.osm.pbf` suffix.
    pub fn clipped_osm_filename(&self) -> String {
        format!("{}.osm.pbf", self.region.id)
    }

    /// Build a context against the committed region tree, for tests.
    #[cfg(test)]
    pub fn for_test(data_dir: PathBuf, version: &str) -> Self {
        let regions = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../regions");
        let region = iter_region::resolve(&regions, "italy/lazio/rome").unwrap();
        Self {
            data_dir,
            version: version.to_string(),
            region,
            http: reqwest::Client::new(),
        }
    }
}
