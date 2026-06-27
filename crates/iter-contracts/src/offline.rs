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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn manifest_camel_case_keys() {
        let m = Manifest {
            generator: "itermaps".to_string(),
            created_at: "2026-06-27T00:00:00Z".to_string(),
            bbox: [11.3, 41.1, 14.05, 43.35],
            minzoom: 6,
            maxzoom: 14,
            pmtiles: "area.pmtiles".to_string(),
            styles: vec!["light".to_string(), "dark".to_string()],
            glyphs: true,
            sprite: false,
            overlays: vec!["stations".to_string()],
            note: "__BASE_URL__ rewrite required".to_string(),
        };
        let v = to_value(&m).unwrap();
        assert_eq!(
            v,
            json!({
                "generator": "itermaps",
                "createdAt": "2026-06-27T00:00:00Z",
                "bbox": [11.3, 41.1, 14.05, 43.35],
                "minzoom": 6,
                "maxzoom": 14,
                "pmtiles": "area.pmtiles",
                "styles": ["light", "dark"],
                "glyphs": true,
                "sprite": false,
                "overlays": ["stations"],
                "note": "__BASE_URL__ rewrite required",
            })
        );
    }

    #[test]
    fn error_codes_equal_string_values() {
        assert_eq!(code::BBOX_REQUIRED, "BBOX_REQUIRED");
        assert_eq!(code::BBOX_INVALID, "BBOX_INVALID");
        assert_eq!(code::BBOX_OUT_OF_RANGE, "BBOX_OUT_OF_RANGE");
        assert_eq!(code::BBOX_DEGENERATE, "BBOX_DEGENERATE");
        assert_eq!(code::ZOOM_INVALID, "ZOOM_INVALID");
        assert_eq!(code::AREA_TOO_LARGE, "AREA_TOO_LARGE");
        assert_eq!(code::BUSY, "BUSY");
        assert_eq!(code::EXTRACT_FAILED, "EXTRACT_FAILED");
        assert_eq!(code::INTERNAL, "INTERNAL");
    }

    #[test]
    fn style_whitelist_contents() {
        assert_eq!(
            STYLE_WHITELIST,
            ["light", "dark", "transit-light", "transit-dark"]
        );
    }

    #[test]
    fn default_caps() {
        assert_eq!(DEFAULT_MAX_AREA_DEG2, 6.0);
        assert_eq!(DEFAULT_MAX_ZOOM, 14);
        assert_eq!(DEFAULT_MAX_CONCURRENT, 3);
    }
}
