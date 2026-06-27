//! Offline pre-download contract (`/offline/extract`, `/offline/bundle`).
//! Because the gateway is public and unauthenticated, the caps below are the
//! only abuse protection — they are part of the contract, not tuning.

use serde::Serialize;

/// Default abuse-protection caps (overridable via `OFFLINE_MAX_*` env knobs).
pub const DEFAULT_MAX_AREA_DEG2: f64 = 6.0;
pub const DEFAULT_MAX_ZOOM: u8 = 14;
pub const DEFAULT_MAX_CONCURRENT: usize = 3;

/// Offline-specific error codes (the nested `{error:{code,..}}` envelope).
pub mod code {
    pub const BBOX_REQUIRED: &str = "BBOX_REQUIRED";
    pub const BBOX_INVALID: &str = "BBOX_INVALID";
    pub const BBOX_OUT_OF_RANGE: &str = "BBOX_OUT_OF_RANGE";
    pub const BBOX_DEGENERATE: &str = "BBOX_DEGENERATE";
    pub const ZOOM_INVALID: &str = "ZOOM_INVALID";
    pub const AREA_TOO_LARGE: &str = "AREA_TOO_LARGE";
    pub const BUSY: &str = "BUSY";
    pub const EXTRACT_FAILED: &str = "EXTRACT_FAILED";
    pub const INTERNAL: &str = "INTERNAL";
}

/// The four style names a bundle may include (whitelist).
pub const STYLE_WHITELIST: [&str; 4] = ["light", "dark", "transit-light", "transit-dark"];

/// `manifest.json` inside a bundle zip. Documents contents and the
/// `__BASE_URL__` → `file://` substitution the offline client must perform.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub generator: String,
    pub created_at: String,
    /// [minLon, minLat, maxLon, maxLat]
    pub bbox: [f64; 4],
    pub minzoom: u8,
    pub maxzoom: u8,
    /// Always "area.pmtiles" — styles are rewritten to point at it.
    pub pmtiles: String,
    pub styles: Vec<String>,
    pub glyphs: bool,
    pub sprite: bool,
    pub overlays: Vec<String>,
    pub note: String,
}
