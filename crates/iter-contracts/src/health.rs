//! Health and freshness documents. The static `health.json` has five exact
//! fields the client reads to show an "update app" banner; the gateway health
//! is operator diagnostics; the freshness manifest drives server→client
//! artifact sync.

use iter_core::Status;
use serde::Serialize;

/// `GET /health` (and `/health.json`) on the static surface — five exact fields
/// the client reads to show an "update app" banner. `gtfs_loaded` is an
/// ISO-8601 timestamp or the literal `"unknown"`; the timestamps are ISO-8601
/// or null. There is deliberately no `graphBuiltAt`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticHealth {
    pub status: Status,
    pub version: String,
    pub gtfs_loaded: String,
    pub tiles_built_at: Option<String>,
    pub bootstrapped_at: Option<String>,
}

impl StaticHealth {
    /// The "not yet bootstrapped" answer the gateway returns until the pipeline
    /// has written the real `health.json`.
    pub fn not_ready(version: impl Into<String>) -> Self {
        Self {
            status: Status::Degraded,
            version: version.into(),
            gtfs_loaded: "unknown".to_string(),
            tiles_built_at: None,
            bootstrapped_at: None,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json, to_value};

    #[test]
    fn static_health_exact_camel_case_keys() {
        let h = StaticHealth {
            status: Status::Ok,
            version: "1.2.3".to_string(),
            gtfs_loaded: "2026-06-27T00:00:00Z".to_string(),
            tiles_built_at: Some("2026-06-27T01:00:00Z".to_string()),
            bootstrapped_at: Some("2026-06-27T02:00:00Z".to_string()),
        };
        let v = to_value(&h).unwrap();
        assert_eq!(
            v,
            json!({
                "status": "ok",
                "version": "1.2.3",
                "gtfsLoaded": "2026-06-27T00:00:00Z",
                "tilesBuiltAt": "2026-06-27T01:00:00Z",
                "bootstrappedAt": "2026-06-27T02:00:00Z",
            })
        );
    }

    #[test]
    fn static_health_null_timestamps_serialize_as_null() {
        let h = StaticHealth {
            status: Status::Ok,
            version: "1.0.0".to_string(),
            gtfs_loaded: "unknown".to_string(),
            tiles_built_at: None,
            bootstrapped_at: None,
        };
        let v = to_value(&h).unwrap();
        assert_eq!(v["tilesBuiltAt"], Value::Null);
        assert_eq!(v["bootstrappedAt"], Value::Null);
        assert!(v.as_object().unwrap().contains_key("tilesBuiltAt"));
        assert!(v.as_object().unwrap().contains_key("bootstrappedAt"));
    }

    #[test]
    fn static_health_not_ready() {
        let h = StaticHealth::not_ready("9.9.9");
        assert_eq!(h.status, Status::Degraded);
        assert_eq!(h.gtfs_loaded, "unknown");
        assert!(h.tiles_built_at.is_none());
        assert!(h.bootstrapped_at.is_none());
        let v = to_value(&h).unwrap();
        assert_eq!(v["status"], "degraded");
        assert_eq!(v["version"], "9.9.9");
        assert_eq!(v["gtfsLoaded"], "unknown");
        assert_eq!(v["tilesBuiltAt"], Value::Null);
        assert_eq!(v["bootstrappedAt"], Value::Null);
    }

    #[test]
    fn gateway_health_keys() {
        let h = GatewayHealth {
            status: Status::Ok,
            providers: vec!["trenitalia".to_string(), "tiles".to_string()],
            offline: vec!["extract".to_string()],
            fl_gtfs: FlGtfsState::default(),
        };
        let v = to_value(&h).unwrap();
        let obj = v.as_object().unwrap();
        assert!(obj.contains_key("status"));
        assert!(obj.contains_key("providers"));
        assert!(obj.contains_key("offline"));
        assert!(obj.contains_key("flGtfs"));
        assert_eq!(v["providers"], json!(["trenitalia", "tiles"]));
        assert_eq!(v["offline"], json!(["extract"]));
    }

    #[test]
    fn fl_gtfs_state_default_omits_error_and_stats() {
        let s = FlGtfsState::default();
        let v = to_value(&s).unwrap();
        assert_eq!(
            v,
            json!({
                "builtAt": Value::Null,
                "building": false,
            })
        );
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("error"));
        assert!(!obj.contains_key("stats"));
    }

    #[test]
    fn fl_gtfs_state_with_error_and_stats() {
        let s = FlGtfsState {
            built_at: Some("2026-06-27T00:00:00Z".to_string()),
            building: true,
            error: Some("boom".to_string()),
            stats: Some(FlGtfsStats {
                journeys_total: 100,
                trips: 90,
                stops: 50,
                routes: 10,
                shaped_routes: 8,
                skipped: 2,
            }),
        };
        let v = to_value(&s).unwrap();
        assert_eq!(v["builtAt"], "2026-06-27T00:00:00Z");
        assert_eq!(v["building"], true);
        assert_eq!(v["error"], "boom");
        assert_eq!(
            v["stats"],
            json!({
                "journeysTotal": 100,
                "trips": 90,
                "stops": 50,
                "routes": 10,
                "shapedRoutes": 8,
                "skipped": 2,
            })
        );
    }

    #[test]
    fn freshness_manifest_keys_and_artifact() {
        let mut artifacts = std::collections::BTreeMap::new();
        artifacts.insert(
            "tiles".to_string(),
            ArtifactFreshness {
                updated_at: Some("2026-06-27T00:00:00Z".to_string()),
                etag: Some("\"abc\"".to_string()),
            },
        );
        let m = FreshnessManifest {
            api_version: "v1".to_string(),
            generated_at: "2026-06-27T03:00:00Z".to_string(),
            artifacts,
        };
        let v = to_value(&m).unwrap();
        assert_eq!(
            v,
            json!({
                "apiVersion": "v1",
                "generatedAt": "2026-06-27T03:00:00Z",
                "artifacts": {
                    "tiles": {
                        "updatedAt": "2026-06-27T00:00:00Z",
                        "etag": "\"abc\"",
                    }
                },
            })
        );
    }

    #[test]
    fn artifact_freshness_default_omits_both() {
        let a = ArtifactFreshness::default();
        let v = to_value(&a).unwrap();
        assert_eq!(v, json!({}));
    }
}
