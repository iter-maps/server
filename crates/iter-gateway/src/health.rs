//! Orchestration probes. Liveness is process-up; readiness gates traffic on the
//! artifact tree being present. The internal `/metrics` endpoint lives here too:
//! it shares the same operator-local posture as `/livez`/`/readyz` (ADR 0037).

use axum::Json;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use iter_contracts::health::StaticHealth;
use iter_core::Status;
use iter_core::health::{Check, Readiness};

use crate::state::AppState;

pub async fn livez() -> impl IntoResponse {
    StatusCode::OK
}

pub async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    let data_dir = &state.cfg.data_dir;
    let data_check = if data_dir.is_dir() {
        Check::ok("data_dir")
    } else {
        Check::down("data_dir", format!("{} not present", data_dir.display()))
    };

    let report = Readiness::from_checks(vec![data_check]);
    let code = match report.status {
        Status::Down => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::OK,
    };
    (code, Json(report))
}

/// **Internal** `GET /metrics`: the Prometheus exposition of the operator-local
/// metrics (ADR 0037 phase 2). This is NOT for public exposure — it carries no
/// user data, but it is operator-monitoring surface gated behind the external
/// proxy exactly like `/livez`/`/readyz`; the proxy must not route it publicly.
///
/// Renders the process-wide recorder's snapshot as Prometheus text 0.0.4. Total
/// and fail-soft: when metrics are disabled (`METRICS_ENABLED=0`) it 404s, and
/// when no recorder handle is installed (a lost install race, or metrics off) it
/// returns an empty body rather than panicking — a scrape simply sees no series.
pub async fn metrics(State(state): State<AppState>) -> Response {
    if !state.cfg.metrics_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    // Prometheus text exposition, version 0.0.4 (the format the handle renders).
    let ct = (header::CONTENT_TYPE, "text/plain; version=0.0.4");
    match iter_core::metrics::prometheus_handle() {
        Some(handle) => ([ct], handle.render()).into_response(),
        None => ([ct], String::new()).into_response(),
    }
}

/// Client-facing `GET /health` (and `/health.json`): the freshness document the
/// app reads to prompt an update. The pipeline writes the real file from
/// artifact mtimes; until then we answer a degraded "not bootstrapped" body.
pub async fn client_health(State(state): State<AppState>) -> Response {
    let cache = (header::CACHE_CONTROL, "public, max-age=60");
    match tokio::fs::read(&state.cfg.health_path).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, "application/json"), cache], bytes).into_response(),
        Err(_) => ([cache], Json(StaticHealth::not_ready(&state.cfg.version))).into_response(),
    }
}
