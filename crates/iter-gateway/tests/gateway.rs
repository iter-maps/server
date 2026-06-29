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
    // The unparsable token is echoed verbatim, not the i32::MIN miss sentinel.
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

// --- opt-in itinerary reranking (ADR 0026) ----------------------------------

/// Stand up a throwaway OTP stub on a loopback port that answers every request
/// with `(status, body)`, and return its base URL. The gateway's outbound
/// reqwest hits this real socket; the inbound side is still driven via `oneshot`.
async fn otp_stub(status: StatusCode, body: &'static str) -> (String, tokio::task::JoinHandle<()>) {
    use axum::routing::post;
    let app = Router::new().route("/otp/gtfs/v1", post(move || async move { (status, body) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), handle)
}

/// Like [`otp_stub`] but sets an explicit `Content-Type` on the response, so a
/// test can prove the handler's body handling is content-type-agnostic.
async fn otp_stub_ct(
    status: StatusCode,
    body: &'static str,
    ct: &'static str,
) -> (String, tokio::task::JoinHandle<()>) {
    use axum::routing::post;
    let app = Router::new().route(
        "/otp/gtfs/v1",
        post(move || async move { ([(header::CONTENT_TYPE, ct)], (status, body)) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), handle)
}

/// A populated gateway whose `otp_url` points at `otp_base` and whose reliability
/// dir holds `tier2` (already seeded under `<root>/reliability/`).
fn gateway_with_otp(otp_base: String, root: PathBuf) -> Router {
    let mut cfg = config_for(root);
    cfg.otp_url = otp_base;
    router::build(AppState::new(cfg).unwrap())
}

/// POST a routing query to the gateway, optionally with the `reliability` flag.
fn routing_req(rerank: bool) -> Request<Body> {
    if rerank {
        routing_req_profile("reliability")
    } else {
        routing_req_plain()
    }
}

/// POST a routing query with no rerank flag (the default passthrough path).
fn routing_req_plain() -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/otp/gtfs/v1")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"query":"{plan{itineraries{legs{mode}}}}"}"#))
        .unwrap()
}

/// POST a routing query opting into the given rerank profile.
fn routing_req_profile(profile: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/otp/gtfs/v1?rerank={profile}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"query":"{plan{itineraries{legs{mode}}}}"}"#))
        .unwrap()
}

/// A two-itinerary plan distinguishing modes for the eco profile: itinerary 0
/// rides a bus (higher carbon), itinerary 1 rides the metro (lower carbon), both
/// the same distance. Carries `mode`/`distance` so the composite factors apply.
const ECO_PLAN: &str = r#"{"data":{"plan":{"itineraries":[
  {"duration":600,"legs":[{"transitLeg":true,"mode":"BUS","distance":5000.0,"route":{"gtfsId":"BUSLINE"},"trip":{"directionId":0},"from":{"stop":{"gtfsId":"s1"}}}]},
  {"duration":700,"legs":[{"transitLeg":true,"mode":"SUBWAY","distance":5000.0,"route":{"gtfsId":"METRO"},"trip":{"directionId":0},"from":{"stop":{"gtfsId":"s2"}}}]}
]}}}"#;

/// A two-itinerary OTP plan: itinerary 0 rides route SLOW, itinerary 1 rides
/// route FAST, both boarding stop 70001 in direction 0.
const TWO_ITIN_PLAN: &str = r#"{"data":{"plan":{"itineraries":[
  {"duration":600,"legs":[{"transitLeg":true,"route":{"gtfsId":"SLOW"},"trip":{"directionId":0},"from":{"stop":{"gtfsId":"70001"}}}]},
  {"duration":700,"legs":[{"transitLeg":true,"route":{"gtfsId":"FAST"},"trip":{"directionId":0},"from":{"stop":{"gtfsId":"70001"}}}]}
]}}}"#;

/// Seed reliability so FAST is reliable (on-time) and SLOW is not, both at stop
/// 70001 / direction 0.
fn seed_rerank_reliability(root: &Path) {
    let mut on_time = Tier2::default();
    for _ in 0..10 {
        on_time.observe(0); // all on-time → rate 1.0
    }
    let mut late = Tier2::default();
    for _ in 0..10 {
        late.observe(900); // all late → rate 0.0
    }
    let mut map = std::collections::BTreeMap::new();
    map.insert(
        tier2_key("FAST", 0, "70001", TodBucket::AmPeak, DayType::Weekday),
        on_time,
    );
    map.insert(
        tier2_key("SLOW", 0, "70001", TodBucket::AmPeak, DayType::Weekday),
        late,
    );
    write(
        &root.join("reliability").join(TIER2_FILE),
        &serde_json::to_vec(&map).unwrap(),
    );
}

/// A two-itinerary plan where itinerary 0 (MID) rides two transit legs and
/// itinerary 1 (SINGLE) rides one, all in direction 0. Lets a test exercise the
/// count-weighted multi-leg mean through the real handler.
const MULTILEG_PLAN: &str = r#"{"data":{"plan":{"itineraries":[
  {"duration":600,"legs":[
    {"transitLeg":true,"route":{"gtfsId":"MID_A"},"trip":{"directionId":0},"from":{"stop":{"gtfsId":"S1"}}},
    {"transitLeg":true,"route":{"gtfsId":"MID_B"},"trip":{"directionId":0},"from":{"stop":{"gtfsId":"S2"}}}
  ]},
  {"duration":700,"legs":[{"transitLeg":true,"route":{"gtfsId":"SINGLE"},"trip":{"directionId":0},"from":{"stop":{"gtfsId":"S3"}}}]}
]}}}"#;

/// Seed reliability for [`MULTILEG_PLAN`]: MID_A on-time 1.0 and MID_B 0.6 (equal
/// observation counts → mean 0.80), SINGLE 0.30. So MID's mean beats SINGLE.
fn seed_multileg_reliability(root: &Path) {
    // 10 obs each so the per-leg rates are exact and counts are equal.
    let leg = |on_time: usize| {
        let mut t = Tier2::default();
        for i in 0..10 {
            t.observe(if i < on_time { 0 } else { 900 });
        }
        t
    };
    let mut map = std::collections::BTreeMap::new();
    map.insert(
        tier2_key("MID_A", 0, "S1", TodBucket::AmPeak, DayType::Weekday),
        leg(10), // 1.0
    );
    map.insert(
        tier2_key("MID_B", 0, "S2", TodBucket::AmPeak, DayType::Weekday),
        leg(6), // 0.6
    );
    map.insert(
        tier2_key("SINGLE", 0, "S3", TodBucket::AmPeak, DayType::Weekday),
        leg(3), // 0.3
    );
    write(
        &root.join("reliability").join(TIER2_FILE),
        &serde_json::to_vec(&map).unwrap(),
    );
}

/// Read back the order of first-leg route ids from a plan body.
fn itinerary_routes(body: &[u8]) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_slice(body).unwrap();
    v["data"]["plan"]["itineraries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|it| {
            it["legs"][0]["route"]["gtfsId"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn routing_default_is_byte_for_byte_passthrough() {
    // Without the flag the handler must not parse or reorder — the body comes
    // back exactly as the upstream sent it, even though reliability data exists.
    let (otp, _h) = otp_stub(StatusCode::OK, TWO_ITIN_PLAN).await;
    let dir = tempfile::tempdir().unwrap();
    seed_rerank_reliability(dir.path());
    let app = gateway_with_otp(otp, dir.path().to_path_buf());

    let (status, ct, body) = send(&app, routing_req(false)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ct.as_deref(), Some("text/plain; charset=utf-8"));
    // Original order preserved AND no additive field injected.
    assert_eq!(body, TWO_ITIN_PLAN.as_bytes());
    assert_eq!(itinerary_routes(&body), vec!["SLOW", "FAST"]);
}

#[tokio::test]
async fn routing_rerank_orders_reliable_itinerary_first() {
    let (otp, _h) = otp_stub(StatusCode::OK, TWO_ITIN_PLAN).await;
    let dir = tempfile::tempdir().unwrap();
    seed_rerank_reliability(dir.path());
    let app = gateway_with_otp(otp, dir.path().to_path_buf());

    let (status, _, body) = send(&app, routing_req(true)).await;
    assert_eq!(status, StatusCode::OK);
    // FAST (on-time 1.0) now leads SLOW (0.0).
    assert_eq!(itinerary_routes(&body), vec!["FAST", "SLOW"]);
    // The additive score is present and ordered.
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let its = v["data"]["plan"]["itineraries"].as_array().unwrap();
    assert_eq!(its[0]["reliabilityScore"], serde_json::json!(1.0));
    assert_eq!(its[1]["reliabilityScore"], serde_json::json!(0.0));
    // Schema preserved: legs/duration untouched.
    assert_eq!(its[0]["duration"], 700);
    assert_eq!(its[0]["legs"][0]["route"]["gtfsId"], "FAST");
}

#[tokio::test]
async fn routing_rerank_with_no_reliability_data_keeps_original_order() {
    // The flag is set but the store is empty → every itinerary scores neutral and
    // the stable sort preserves OTP's original order.
    let (otp, _h) = otp_stub(StatusCode::OK, TWO_ITIN_PLAN).await;
    let dir = tempfile::tempdir().unwrap(); // no reliability/ written
    let app = gateway_with_otp(otp, dir.path().to_path_buf());

    let (status, _, body) = send(&app, routing_req(true)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(itinerary_routes(&body), vec!["SLOW", "FAST"]);
}

#[tokio::test]
async fn routing_rerank_non_plan_body_passes_through_untouched() {
    // An OTP GraphQL error envelope on the opt-in path is not a plan → returned
    // verbatim, never a 500.
    let err_body = r#"{"errors":[{"message":"no path found"}]}"#;
    let (otp, _h) = otp_stub(StatusCode::OK, err_body).await;
    let dir = tempfile::tempdir().unwrap();
    seed_rerank_reliability(dir.path());
    let app = gateway_with_otp(otp, dir.path().to_path_buf());

    let (status, _, body) = send(&app, routing_req(true)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, err_body.as_bytes());
}

#[tokio::test]
async fn routing_rerank_malformed_json_passes_through_untouched() {
    let (otp, _h) = otp_stub(StatusCode::OK, "{ not json at all").await;
    let dir = tempfile::tempdir().unwrap();
    let app = gateway_with_otp(otp, dir.path().to_path_buf());

    let (status, _, body) = send(&app, routing_req(true)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"{ not json at all");
}

#[tokio::test]
async fn routing_rerank_preserves_upstream_error_status() {
    // A non-200 from OTP on the opt-in path is relayed with its status and body,
    // never reranked.
    let (otp, _h) = otp_stub(StatusCode::BAD_REQUEST, r#"{"errors":[]}"#).await;
    let dir = tempfile::tempdir().unwrap();
    let app = gateway_with_otp(otp, dir.path().to_path_buf());

    let (status, _, body) = send(&app, routing_req(true)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, br#"{"errors":[]}"#);
}

#[tokio::test]
async fn routing_rerank_dead_upstream_is_still_502() {
    // The opt-in path inherits the same transport fail-soft as the passthrough.
    let (_d, app) = populated();
    let (status, _, body) = send(&app, routing_req(true)).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "UPSTREAM_UNAVAILABLE");
}

#[tokio::test]
async fn routing_default_passthrough_ignores_json_content_type() {
    // The default branch never inspects the body, regardless of content-type: a
    // 200 application/json plan still streams untouched, no reliabilityScore added.
    let (otp, _h) = otp_stub_ct(StatusCode::OK, TWO_ITIN_PLAN, "application/json").await;
    let dir = tempfile::tempdir().unwrap();
    seed_rerank_reliability(dir.path());
    let app = gateway_with_otp(otp, dir.path().to_path_buf());

    let (status, ct, body) = send(&app, routing_req(false)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ct.as_deref(), Some("application/json"));
    assert_eq!(body, TWO_ITIN_PLAN.as_bytes());
    assert!(!String::from_utf8_lossy(&body).contains("reliabilityScore"));
}

#[tokio::test]
async fn routing_rerank_multi_leg_mean_drives_end_to_end_order() {
    // End-to-end: itinerary TWOLEG has two transit legs (one high, one low) whose
    // count-weighted mean must beat a single-leg sibling, proving the real Tier-2
    // index feeds the multi-leg mean through the handler, not just the unit core.
    let (otp, _h) = otp_stub_ct(StatusCode::OK, MULTILEG_PLAN, "application/json").await;
    let dir = tempfile::tempdir().unwrap();
    seed_multileg_reliability(dir.path());
    let app = gateway_with_otp(otp, dir.path().to_path_buf());

    let (status, _, body) = send(&app, routing_req(true)).await;
    assert_eq!(status, StatusCode::OK);
    // MID's two legs mean 0.80; SINGLE is 0.30 → MID leads.
    assert_eq!(itinerary_routes(&body), vec!["MID_A", "SINGLE"]);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let its = v["data"]["plan"]["itineraries"].as_array().unwrap();
    assert_eq!(its[0]["reliabilityScore"], serde_json::json!(0.8));
    assert_eq!(its[1]["reliabilityScore"], serde_json::json!(0.3));
}

#[tokio::test]
async fn routing_rerank_eco_profile_orders_low_carbon_first() {
    // End-to-end: the eco profile reorders a bus-first plan so the metro (lower
    // gCO2e/p-km over the same distance) leads, with no reliability history at
    // all — proving a non-reliability composite factor drives the order through
    // the real handler (ADR 0028).
    let (otp, _h) = otp_stub_ct(StatusCode::OK, ECO_PLAN, "application/json").await;
    let dir = tempfile::tempdir().unwrap(); // no reliability/ seeded
    let app = gateway_with_otp(otp, dir.path().to_path_buf());

    let (status, _, body) = send(&app, routing_req_profile("eco")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(itinerary_routes(&body), vec!["METRO", "BUSLINE"]);
    // The additive composite score is present; metro outscores the bus.
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let its = v["data"]["plan"]["itineraries"].as_array().unwrap();
    assert!(its[0]["rerankScore"].as_f64().unwrap() >= its[1]["rerankScore"].as_f64().unwrap());
}

#[tokio::test]
async fn routing_unknown_rerank_profile_is_a_passthrough() {
    // An unrecognized profile must not buffer/reorder — it stays the byte-for-byte
    // passthrough, exactly like the default path (ADR 0028).
    let (otp, _h) = otp_stub(StatusCode::OK, ECO_PLAN).await;
    let dir = tempfile::tempdir().unwrap();
    let app = gateway_with_otp(otp, dir.path().to_path_buf());

    let (status, _, body) = send(&app, routing_req_profile("nonsense")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, ECO_PLAN.as_bytes());
    assert!(!String::from_utf8_lossy(&body).contains("rerankScore"));
}

// --- opt-in no-RT historical delay prediction (ADR 0030) --------------------

/// POST a routing query opting into historical delay prediction.
fn routing_req_predict() -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/otp/gtfs/v1?predict=historical")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"query":"{plan{itineraries{legs{mode}}}}"}"#))
        .unwrap()
}

/// A two-itinerary plan for the predict path: itinerary 0's leg has NO live RT
/// (`realTime: false`, the annotatable case); itinerary 1's leg carries live RT
/// (`realTime: true`) and must never be annotated. Both ride route DELAYED at
/// stop 70001 / direction 0, so they share a Tier-2 key — the only difference is
/// the live flag, proving the authoritative floor.
const PREDICT_PLAN: &str = r#"{"data":{"plan":{"itineraries":[
  {"duration":600,"legs":[{"transitLeg":true,"mode":"BUS","realTime":false,"route":{"gtfsId":"DELAYED"},"trip":{"directionId":0},"from":{"stop":{"gtfsId":"70001"}}}]},
  {"duration":700,"legs":[{"transitLeg":true,"mode":"BUS","realTime":true,"arrivalDelay":42,"route":{"gtfsId":"DELAYED"},"trip":{"directionId":0},"from":{"stop":{"gtfsId":"70001"}}}]}
]}}}"#;

/// Seed reliability so route DELAYED at 70001/0 has a clearly positive typical
/// delay (all observations ~600s late) across an am-peak weekday cell.
fn seed_predict_reliability(root: &Path) {
    let mut late = Tier2::default();
    for _ in 0..10 {
        late.observe(600);
    }
    let mut map = std::collections::BTreeMap::new();
    map.insert(
        tier2_key("DELAYED", 0, "70001", TodBucket::AmPeak, DayType::Weekday),
        late,
    );
    write(
        &root.join("reliability").join(TIER2_FILE),
        &serde_json::to_vec(&map).unwrap(),
    );
}

#[tokio::test]
async fn routing_predict_annotates_rtless_leg_but_not_the_live_one() {
    // End-to-end authoritative-floor proof: same Tier-2 key, two legs differing
    // only by the live `realTime` flag. The RT-less leg gains a historical
    // `predictedDelay`; the live leg is left exactly as upstream sent it.
    let (otp, _h) = otp_stub_ct(StatusCode::OK, PREDICT_PLAN, "application/json").await;
    let dir = tempfile::tempdir().unwrap();
    seed_predict_reliability(dir.path());
    let app = gateway_with_otp(otp, dir.path().to_path_buf());

    let (status, _, body) = send(&app, routing_req_predict()).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let its = v["data"]["plan"]["itineraries"].as_array().unwrap();
    // Order preserved — annotation never reorders.
    assert_eq!(its[0]["legs"][0]["route"]["gtfsId"], "DELAYED");
    assert_eq!(its[1]["legs"][0]["route"]["gtfsId"], "DELAYED");

    // Itinerary 0: RT-less → annotated with a positive historical delay + tag.
    let pd = &its[0]["legs"][0]["predictedDelay"];
    assert_eq!(pd["source"], "historical");
    assert!(pd["seconds"].as_f64().unwrap() > 0.0, "expected a late p85");
    assert!(pd["sampleCount"].as_u64().unwrap() >= 10);
    assert!(its[0].get("predictedDelaySummary").is_some());

    // Itinerary 1: live RT → never annotated, live field intact.
    assert!(its[1]["legs"][0].get("predictedDelay").is_none());
    assert_eq!(its[1]["legs"][0]["arrivalDelay"], 42);
    assert!(its[1].get("predictedDelaySummary").is_none());
}

#[tokio::test]
async fn routing_predict_with_no_history_adds_no_field() {
    // The flag is set but the store is empty → no leg can resolve a typical delay,
    // so no annotation is added (a gap with no history stays a gap).
    let (otp, _h) = otp_stub_ct(StatusCode::OK, PREDICT_PLAN, "application/json").await;
    let dir = tempfile::tempdir().unwrap(); // no reliability/ written
    let app = gateway_with_otp(otp, dir.path().to_path_buf());

    let (status, _, body) = send(&app, routing_req_predict()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!String::from_utf8_lossy(&body).contains("predictedDelay"));
}

#[tokio::test]
async fn routing_predict_feed_prefixed_ids_match_unprefixed_index() {
    // OTP sends `FEED:LOCAL` ids; the index is keyed by the bare locals. Prove the
    // ADR 0027 normalization lands the annotation through the real handler.
    const PREFIXED: &str = r#"{"data":{"plan":{"itineraries":[
      {"duration":600,"legs":[{"transitLeg":true,"mode":"BUS","realTime":false,"route":{"gtfsId":"ATAC:DELAYED"},"trip":{"directionId":0},"from":{"stop":{"gtfsId":"ATAC:70001"}}}]}
    ]}}}"#;
    let (otp, _h) = otp_stub_ct(StatusCode::OK, PREFIXED, "application/json").await;
    let dir = tempfile::tempdir().unwrap();
    seed_predict_reliability(dir.path()); // keyed by the bare "DELAYED"/"70001"
    let app = gateway_with_otp(otp, dir.path().to_path_buf());

    let (status, _, body) = send(&app, routing_req_predict()).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let pd = &v["data"]["plan"]["itineraries"][0]["legs"][0]["predictedDelay"];
    assert_eq!(pd["source"], "historical");
    assert!(pd["seconds"].as_f64().unwrap() > 0.0);
}

#[tokio::test]
async fn routing_predict_non_plan_body_passes_through_untouched() {
    // An OTP error envelope on the predict path is not a plan → returned verbatim.
    let err_body = r#"{"errors":[{"message":"no path found"}]}"#;
    let (otp, _h) = otp_stub(StatusCode::OK, err_body).await;
    let dir = tempfile::tempdir().unwrap();
    seed_predict_reliability(dir.path());
    let app = gateway_with_otp(otp, dir.path().to_path_buf());

    let (status, _, body) = send(&app, routing_req_predict()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, err_body.as_bytes());
}

#[tokio::test]
async fn routing_predict_malformed_json_passes_through_untouched() {
    let (otp, _h) = otp_stub(StatusCode::OK, "{ not json at all").await;
    let dir = tempfile::tempdir().unwrap();
    let app = gateway_with_otp(otp, dir.path().to_path_buf());

    let (status, _, body) = send(&app, routing_req_predict()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"{ not json at all");
}

#[tokio::test]
async fn routing_default_does_not_predict() {
    // Without any flag the body is the byte-for-byte passthrough even though
    // history exists — no `predictedDelay` injected.
    let (otp, _h) = otp_stub_ct(StatusCode::OK, PREDICT_PLAN, "application/json").await;
    let dir = tempfile::tempdir().unwrap();
    seed_predict_reliability(dir.path());
    let app = gateway_with_otp(otp, dir.path().to_path_buf());

    let (status, _, body) = send(&app, routing_req_plain()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, PREDICT_PLAN.as_bytes());
    assert!(!String::from_utf8_lossy(&body).contains("predictedDelay"));
}

#[tokio::test]
async fn routing_rerank_and_predict_compose_on_one_buffer() {
    // Both opt-ins together: the reranker reorders by reliability AND the annotator
    // fills RT-less legs with a historical delay, on a single buffered plan.
    let (otp, _h) = otp_stub_ct(StatusCode::OK, TWO_ITIN_PLAN, "application/json").await;
    let dir = tempfile::tempdir().unwrap();
    seed_rerank_reliability(dir.path()); // FAST on-time, SLOW late
    let app = gateway_with_otp(otp, dir.path().to_path_buf());

    let req = Request::builder()
        .method("POST")
        .uri("/otp/gtfs/v1?rerank=reliability&predict=historical")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"query":"{plan{itineraries{legs{mode}}}}"}"#))
        .unwrap();
    let (status, _, body) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    // Rerank applied: FAST (on-time) leads SLOW.
    assert_eq!(itinerary_routes(&body), vec!["FAST", "SLOW"]);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let its = v["data"]["plan"]["itineraries"].as_array().unwrap();
    // Rerank score present.
    assert_eq!(its[0]["reliabilityScore"], serde_json::json!(1.0));
    // Predict applied: the RT-less legs (TWO_ITIN_PLAN has no realTime flag) carry
    // a historical annotation from the seeded history.
    assert_eq!(its[0]["legs"][0]["predictedDelay"]["source"], "historical");
    assert_eq!(its[1]["legs"][0]["predictedDelay"]["source"], "historical");
}
