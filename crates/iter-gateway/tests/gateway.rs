//! Integration tests for the gateway router, driven via `tower`'s `oneshot`
//! against a temp artifact tree — no sockets, no real upstreams.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use iter_gateway::config::{GatewayConfig, OfflineCaps};
use iter_gateway::router;
use iter_gateway::state::AppState;
use tempfile::TempDir;
use tower::ServiceExt;

fn config_for(data_dir: PathBuf) -> GatewayConfig {
    GatewayConfig {
        bind: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        // Dead ports: proxy calls fail fast with connection-refused.
        otp_url: "http://127.0.0.1:1".to_string(),
        photon_url: "http://127.0.0.1:1".to_string(),
        viaggiatreno_url: Some("http://127.0.0.1:1".to_string()),
        trenitalia_region: Some(5),
        upstream_timeout: Duration::from_secs(2),
        version: "0.0.0-test".to_string(),
        tiles_dir: data_dir.join("output/tiles"),
        tiles_basename: "rome.pmtiles".to_string(),
        styles_dir: data_dir.join("output/styles"),
        glyphs_dir: data_dir.join("static/glyphs"),
        sprite_dir: data_dir.join("static/sprite"),
        overlays_dir: data_dir.join("output/overlays"),
        reliability_dir: data_dir.join("reliability"),
        overlay_kinds: vec!["metro-stations".to_string(), "transit-lines".to_string()],
        region_country: "italy".to_string(),
        default_lang: "it".to_string(),
        health_path: data_dir.join("output/health.json"),
        offline: OfflineCaps {
            max_area_deg2: 6.0,
            max_zoom: 14,
            max_concurrent: 3,
        },
        offline_source: data_dir.join("output/tiles/rome.pmtiles"),
        places_path: data_dir.join("output/places.jsonl"),
        pmtiles_bin: "iter-pmtiles-absent".to_string(),
        data_dir,
    }
}

fn write(path: &Path, body: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// A temp data tree populated with one fixture per artifact kind.
fn populated_state() -> (TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("output/tiles/rome.pmtiles"), &[0u8; 4096]);
    write(
        &root.join("output/styles/light.json"),
        br#"{"glyphs":"__BASE_URL__/glyphs/{fontstack}/{range}.pbf","sprite":"__BASE_URL__/sprite/sprite"}"#,
    );
    write(
        &root.join("static/glyphs/NotoSans-Regular/0-255.pbf"),
        &[1u8; 64],
    );
    write(&root.join("static/sprite/sprite.json"), br#"{"shield":{}}"#);
    write(
        &root.join("output/overlays/metro-stations.geojson"),
        br#"{"type":"FeatureCollection","features":[{"type":"Feature"}]}"#,
    );
    write(
        &root.join("output/places.jsonl"),
        concat!(
            r#"{"id":"ov:a","name":"Ristorante Cavour","category":"catering.restaurant","address":"Via Cavour 1","city":"Roma","lon":12.49,"lat":41.90}"#,
            "\n",
            r#"{"id":"ov:b","name":"Bar Cavour","category":"catering.cafe","address":"V. Cavour 1","city":"Roma","lon":12.491,"lat":41.901}"#,
            "\n",
        )
        .as_bytes(),
    );
    let state = AppState::new(config_for(root.to_path_buf())).unwrap();
    (dir, state)
}

fn populated() -> (TempDir, Router) {
    let (dir, state) = populated_state();
    (dir, router::build(state))
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Option<String>, Vec<u8>) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, ct, body)
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

#[tokio::test]
async fn livez_ok() {
    let (_d, app) = populated();
    let (status, _, _) = send(&app, get("/livez")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn readyz_ok_when_data_dir_present() {
    let (_d, app) = populated();
    let (status, _, body) = send(&app, get("/readyz")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"], "ok");
}

#[tokio::test]
async fn readyz_down_when_data_dir_absent() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_for(dir.path().join("missing"));
    let app = router::build(AppState::new(cfg).unwrap());
    let (status, _, body) = send(&app, get("/readyz")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"], "down");
}

#[tokio::test]
async fn tiles_full_and_range_and_missing() {
    let (_d, app) = populated();

    let (status, _, body) = send(&app, get("/tiles/rome.pmtiles")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.len(), 4096);

    let range = Request::builder()
        .uri("/tiles/rome.pmtiles")
        .header(header::RANGE, "bytes=0-99")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(range).await.unwrap();
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(resp.headers()[header::CONTENT_RANGE], "bytes 0-99/4096");

    let (status, _, _) = send(&app, get("/tiles/nope.pmtiles")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn style_base_url_rewrite() {
    let (_d, app) = populated();
    let req = Request::builder()
        .uri("/styles/light.json")
        .header(header::HOST, "maps.test")
        .header("x-forwarded-proto", "https")
        .body(Body::empty())
        .unwrap();
    let (status, ct, body) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ct.as_deref(), Some("application/json"));
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("https://maps.test/glyphs/"));
    assert!(!text.contains("__BASE_URL__"));
}

#[tokio::test]
async fn style_unknown_is_404_envelope() {
    let (_d, app) = populated();
    let (status, _, body) = send(&app, get("/styles/bogus.json")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "NOT_FOUND");
}

#[tokio::test]
async fn glyph_fallback_to_noto() {
    let (_d, app) = populated();
    // Unknown fontstack falls back to NotoSans-Regular.
    let (status, ct, _) = send(&app, get("/glyphs/UnknownFont/0-255.pbf")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ct.as_deref(), Some("application/x-protobuf"));

    let (status, _, _) = send(&app, get("/glyphs/NotoSans-Regular/0-255.pbf")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn glyph_invalid_range_is_400() {
    let (_d, app) = populated();
    let (status, _, _) = send(&app, get("/glyphs/NotoSans-Regular/abc.pbf")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn glyph_unknown_range_is_404() {
    let (_d, app) = populated();
    let (status, _, _) = send(&app, get("/glyphs/NotoSans-Regular/999-1000.pbf")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sprite_served() {
    let (_d, app) = populated();
    let (status, _, _) = send(&app, get("/sprite/sprite.json")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn overlay_present_missing_and_unknown() {
    let (_d, app) = populated();

    let (status, ct, _) = send(&app, get("/overlays/metro-stations.geojson")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ct.as_deref(), Some("application/geo+json"));

    // Not generated yet → fail-soft empty FeatureCollection.
    let (status, _, body) = send(&app, get("/overlays/transit-lines.geojson")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["type"], "FeatureCollection");
    assert_eq!(v["features"].as_array().unwrap().len(), 0);

    let (status, _, _) = send(&app, get("/overlays/bogus.geojson")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn client_health_fallback_then_file() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_for(dir.path().to_path_buf());
    let health_path = cfg.health_path.clone();
    let app = router::build(AppState::new(cfg).unwrap());

    // No file yet → degraded "not bootstrapped".
    let (status, _, body) = send(&app, get("/health")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"], "degraded");
    assert_eq!(v["gtfsLoaded"], "unknown");

    // Pipeline writes the real document → served verbatim.
    write(&health_path, br#"{"status":"ok","version":"1.0.0","gtfsLoaded":"t","tilesBuiltAt":"t","bootstrappedAt":"t"}"#);
    let (status, _, body) = send(&app, get("/health.json")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"], "ok");
}

#[tokio::test]
async fn routing_proxy_dead_upstream_is_502() {
    let (_d, app) = populated();
    let req = Request::builder()
        .method("POST")
        .uri("/otp/gtfs/v1")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"query":"{stops{name}}"}"#))
        .unwrap();
    let (status, _, body) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "UPSTREAM_UNAVAILABLE");
}

#[tokio::test]
async fn geocode_proxy_dead_upstream_is_502() {
    let (_d, app) = populated();
    let (status, _, _) = send(&app, get("/status")).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn trenitalia_search_too_short_is_400() {
    let (_d, app) = populated();
    let (status, _, _) = send(&app, get("/trenitalia/stations/search?q=a")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn trenitalia_search_valid_reaches_dead_upstream() {
    let (_d, app) = populated();
    let (status, _, _) = send(&app, get("/trenitalia/stations/search?q=roma")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn trenitalia_departures_bad_station_is_400() {
    let (_d, app) = populated();
    let (status, _, _) = send(&app, get("/trenitalia/departures?station=nope")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn trenitalia_departures_valid_reaches_dead_upstream() {
    let (_d, app) = populated();
    let (status, _, _) = send(&app, get("/trenitalia/departures?station=S08409")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn offline_missing_bbox_is_400() {
    let (_d, app) = populated();
    let (status, _, body) = send(&app, get("/offline/extract")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "BBOX_REQUIRED");
}

#[tokio::test]
async fn offline_invalid_bbox_is_400() {
    let (_d, app) = populated();
    let (status, _, body) = send(&app, get("/offline/extract?bbox=1,2,3")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "BBOX_INVALID");
}

#[tokio::test]
async fn offline_area_too_large_is_413() {
    let (_d, app) = populated();
    let (status, _, body) = send(&app, get("/offline/extract?bbox=0,0,10,10")).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "AREA_TOO_LARGE");
}

#[tokio::test]
async fn offline_bundle_missing_bbox_is_400() {
    let (_d, app) = populated();
    let (status, _, body) = send(&app, get("/offline/bundle")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "BBOX_REQUIRED");
}

#[tokio::test]
async fn offline_concurrency_gate_returns_503_when_full() {
    let (_d, state) = populated_state();
    let gate = state.offline_gate.clone();
    let app = router::build(state);

    // Hold all permits → the next extract finds the gate full.
    let _permits = gate.acquire_many(3).await.unwrap();
    let (status, _, body) = send(&app, get("/offline/extract?bbox=12.4,41.8,12.6,42.0")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "BUSY");
}

#[tokio::test]
async fn manifest_reports_artifact_freshness() {
    let (_d, app) = populated();
    let (status, ct, body) = send(&app, get("/manifest")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ct.as_deref(), Some("application/json"));
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["apiVersion"], "1");
    assert!(v["generatedAt"].is_string());
    // The tiles fixture exists → updatedAt + a weak etag are present.
    assert!(v["artifacts"]["tiles"]["updatedAt"].is_string());
    assert!(
        v["artifacts"]["tiles"]["etag"]
            .as_str()
            .unwrap()
            .starts_with("W/")
    );
}

#[tokio::test]
async fn related_places_correlates_by_civico() {
    let (_d, app) = populated();
    // The query uses a different street-type form than the indexed POIs
    // ("Via Cavour" vs "V. Cavour") — the normalizer must still bucket them.
    let (status, ct, body) = send(
        &app,
        get("/places/related?street=Via%20Cavour&housenumber=1&city=Roma"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ct.as_deref(), Some("application/json"));
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let names: Vec<&str> = v["related"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"Ristorante Cavour"));
    assert!(names.contains(&"Bar Cavour"));
    assert_eq!(v["related"][0]["relation"], "sameAddress");
}

#[tokio::test]
async fn related_places_unknown_address_is_empty_not_error() {
    let (_d, app) = populated();
    let (status, _, body) = send(
        &app,
        get("/places/related?street=Via%20Nowhere&housenumber=99&city=Roma"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["related"].as_array().unwrap().len(), 0);
}

// --- reliability read endpoint (ADR 0024) -----------------------------------

use iter_core::reliability::rollup::{DayType, Tier2, TodBucket};
use iter_core::reliability::store_read::{TIER2_FILE, TIER2_MAX_BYTES, tier2_key};

/// Seed `reliability/tier2.json` with one AM-peak weekday cell for MEA/0/70001.
fn seed_tier2(root: &Path) {
    let mut agg = Tier2::default();
    for d in [0, 0, 0, 600] {
        agg.observe(d); // 3 on-time, 1 late → on-time rate 0.75.
    }
    let mut map = std::collections::BTreeMap::new();
    map.insert(
        tier2_key("MEA", 0, "70001", TodBucket::AmPeak, DayType::Weekday),
        agg,
    );
    write(
        &root.join("reliability").join(TIER2_FILE),
        &serde_json::to_vec(&map).unwrap(),
    );
}

#[tokio::test]
async fn reliability_returns_the_cell_for_a_present_key() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_tier2(root);
    let app = router::build(AppState::new(config_for(root.to_path_buf())).unwrap());

    let (status, ct, body) = send(&app, get("/reliability/MEA/0/70001")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ct.as_deref(), Some("application/json"));
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["route"], "MEA");
    assert_eq!(v["direction"], "0"); // echoed verbatim as the caller's token
    assert_eq!(v["stop"], "70001");
    let cells = v["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 1);
    assert_eq!(cells[0]["todBucket"], "am-peak");
    assert_eq!(cells[0]["dayType"], "weekday");
    assert_eq!(cells[0]["sampleCount"], 4);
    assert_eq!(cells[0]["onTimeRate"], 0.75);
    assert!(cells[0]["p50S"].is_number());
}

#[tokio::test]
async fn reliability_absent_key_is_fail_soft_empty() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_tier2(root);
    let app = router::build(AppState::new(config_for(root.to_path_buf())).unwrap());

    // A stop with no history → 200 with an empty cell list, never a 404/500.
    let (status, _, body) = send(&app, get("/reliability/MEA/0/99999")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["cells"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn reliability_missing_store_is_fail_soft_empty() {
    // No reliability dir at all (the worker never ran) → empty, not an error.
    let (_d, app) = populated();
    let (status, _, body) = send(&app, get("/reliability/MEA/0/70001")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["cells"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn reliability_corrupt_store_is_fail_soft_empty() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("reliability").join(TIER2_FILE), b"{ not json");
    let app = router::build(AppState::new(config_for(root.to_path_buf())).unwrap());

    let (status, _, body) = send(&app, get("/reliability/MEA/0/70001")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["cells"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn reliability_traversal_param_is_fail_soft_empty() {
    // A `..`-laden path param must not read outside the reliability dir. The
    // router URL-decodes the segment; the read sanitizes it to a flat key that
    // can only miss. Plant a file one level up to prove it is never read. The
    // authoritative containment proof is the store_read unit test; here we pin
    // that the full router+decode+handler path stays fail-soft and that the
    // handler echoes the decoded segment back rather than acting on it.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_tier2(root);
    write(&root.join("secret.json"), br#"{"leaked":true}"#);
    let app = router::build(AppState::new(config_for(root.to_path_buf())).unwrap());

    // `%2e%2e%2f` decodes to `../`. Whatever the router yields as the route
    // segment, the handler must return an empty, non-leaking 200.
    let (status, _, body) = send(&app, get("/reliability/%2e%2e%2f%2e%2e%2fsecret/0/x")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["cells"].as_array().unwrap().len(), 0);
    // The handler echoes the decoded route token verbatim — it never joined it
    // onto a path — and the direction parses cleanly.
    assert_eq!(v["route"], "../../secret");
    assert_eq!(v["direction"], "0");
    let text = String::from_utf8(body).unwrap();
    assert!(
        !text.contains("leaked"),
        "endpoint leaked an out-of-dir file"
    );
}

#[tokio::test]
async fn reliability_non_integer_direction_is_fail_soft_empty() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_tier2(root);
    let app = router::build(AppState::new(config_for(root.to_path_buf())).unwrap());

    // A non-numeric direction can't key any cell → empty, not a 400/500.
    let (status, _, body) = send(&app, get("/reliability/MEA/notanint/70001")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["cells"].as_array().unwrap().len(), 0);
    // The unparseable token is echoed verbatim, not the i32::MIN miss sentinel.
    assert_eq!(v["direction"], "notanint");
}

#[tokio::test]
async fn reliability_oversized_store_is_fail_soft_empty() {
    // A Tier-2 file past the size cap is treated as corrupt and read as empty,
    // bounding memory. Lock the cap into the served contract (the core layer
    // proves the stat-before-read; this proves the endpoint inherits it).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let blob = vec![b' '; (TIER2_MAX_BYTES + 1) as usize];
    write(&root.join("reliability").join(TIER2_FILE), &blob);
    let app = router::build(AppState::new(config_for(root.to_path_buf())).unwrap());

    let (status, _, body) = send(&app, get("/reliability/MEA/0/70001")).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["cells"].as_array().unwrap().len(), 0);
}
