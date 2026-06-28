//! Orchestration probes. Liveness is process-up; readiness gates traffic on the
//! artifact tree being present.

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
