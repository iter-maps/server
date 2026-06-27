//! One node of the region tree, as declared in a `region.toml`. Every field a
//! node doesn't own is omitted; the resolver fills it from an ancestor.

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub extents: Extents,
    pub geocoding: Option<Geocoding>,
    pub live_trains: Option<LiveTrains>,
    pub civici: Option<Civici>,
    #[serde(default)]
    pub feeds: Vec<Feed>,
    #[serde(default)]
    pub overlays: Vec<Overlay>,
}

/// The three independent extents (WGS84 `minLon,minLat,maxLon,maxLat`). They
/// are deliberately decoupled: basemap typically lives at the country root,
/// routing/overlay at a region or city.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Extents {
    pub basemap: Option<String>,
    pub routing: Option<String>,
    pub overlay: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Geocoding {
    /// URL of the Photon dump (e.g. the Komoot Italy dump).
    pub photon_dump: String,
    #[serde(default = "it")]
    pub country_codes: String,
    #[serde(default = "it_en")]
    pub languages: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveTrains {
    pub provider: Option<String>,
    pub region_code: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Civici {
    pub bbox: Option<String>,
    pub enable: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feed {
    /// The `feedId` prefix the client renders by (e.g. `ATAC`, `COTRAL`).
    pub id: String,
    #[serde(default)]
    pub url: Option<String>,
    /// `gtfs` (default) or `netex` (gateway-converted).
    #[serde(default)]
    pub source: Option<String>,
    /// Download over TLS without verifying the cert (a documented upstream gap).
    #[serde(default)]
    pub insecure: bool,
    /// Failure to fetch is a warning, not a hard error.
    #[serde(default)]
    pub optional: bool,
    /// Defaults to true when absent.
    pub enabled: Option<bool>,
    pub license: Option<String>,
    /// GTFS-RT channels this feed publishes.
    #[serde(default)]
    pub realtime: Vec<String>,
}

impl Feed {
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overlay {
    /// `metro-stations` or `transit-lines`.
    pub kind: String,
    #[serde(default)]
    pub lines: Vec<String>,
}

fn it() -> String {
    "it".to_string()
}

fn it_en() -> String {
    "it,en".to_string()
}
