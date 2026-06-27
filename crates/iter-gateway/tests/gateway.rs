//! Integration tests for the gateway router, driven via `tower`'s `oneshot`
//! against a temp artifact tree — no sockets, no real upstreams.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use iter_gateway::config::GatewayConfig;
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
        upstream_timeout: Duration::from_secs(2),
        version: "0.0.0-test".to_string(),
        tiles_dir: data_dir.join("output/tiles"),
        styles_dir: data_dir.join("output/styles"),
        glyphs_dir: data_dir.join("static/glyphs"),
        sprite_dir: data_dir.join("static/sprite"),
        overlays_dir: data_dir.join("output/overlays"),
        health_path: data_dir.join("output/health.json"),
        data_dir,
    }
}

fn write(path: &Path, body: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// A temp data tree populated with one fixture per artifact kind.
fn populated() -> (TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("output/tiles/roma.pmtiles"), &[0u8; 4096]);
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
    let app = router::build(AppState::new(config_for(root.to_path_buf())).unwrap());
    (dir, app)
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

    let (status, _, body) = send(&app, get("/tiles/roma.pmtiles")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.len(), 4096);

    let range = Request::builder()
        .uri("/tiles/roma.pmtiles")
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
