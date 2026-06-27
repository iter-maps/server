//! Orchestration probes. Liveness is process-up; readiness gates traffic on
//! the artifact tree being present, and grows more checks (upstream
//! reachability, per-capability artifacts) as capabilities land.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
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
