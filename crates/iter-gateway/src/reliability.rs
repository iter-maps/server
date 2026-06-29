//! Reliability read endpoint: serves the worker-written Tier-2 archive over
//! HTTP. `GET /reliability/{route}/{direction}/{stop}` returns every stored
//! (tod_bucket, day_type) cell for that stop — p50/p85/p90 delay seconds,
//! on-time rate, and the sample count behind them.
//!
//! Fail-soft like the overlays handler: an absent key, a missing store, or a
//! corrupt store all return `200` with an empty `cells` list — never a `500`,
//! never a panic. The client reads an empty list as "no history yet".
//!
//! SECURITY: the `{route}/{direction}/{stop}` path params are external. They are
//! never joined onto a filesystem path here — the shared read side sanitizes
//! every component into a flat Tier-2 map key (`iter_core::reliability`), so a
//! `../../` param can only ever miss, never traverse out of the reliability dir.

use axum::Json;
use axum::extract::{Path, State};
use iter_contracts::reliability::{ReliabilityCell, ReliabilityResponse};
use iter_core::reliability::store_read::read_tier2_cells;

use crate::state::AppState;

/// Serve the Tier-2 reliability cells for a (route, direction, stop). A
/// non-integer `direction` is treated as "no such slice" → empty cells (the
/// store keys direction as an integer), keeping the handler fail-soft. The
/// caller's raw `direction` token is echoed back verbatim; only the lookup uses
/// the parsed integer, so an unparsable direction never surfaces as a sentinel.
pub async fn reliability(
    Path((route, direction, stop)): Path<(String, String, String)>,
    State(state): State<AppState>,
) -> Json<ReliabilityResponse> {
    // i32::MIN is an in-band "won't match" sentinel for the integer-keyed store;
    // it never reaches the response body.
    let direction_id: i32 = direction.parse().unwrap_or(i32::MIN);
    // The read is bounded and fail-soft inside iter-core: a missing/corrupt store
    // yields no cells. The blocking file read is small (Tier-2 is tiny) but we
    // still hop it off the async worker to keep the runtime clean.
    let root = state.cfg.reliability_dir.clone();
    let (route_q, stop_q) = (route.clone(), stop.clone());
    let cells = tokio::task::spawn_blocking(move || {
        read_tier2_cells(&root, &route_q, direction_id, &stop_q)
    })
    .await
    .unwrap_or_default();

    let cells = cells
        .iter()
        .map(|c| {
            ReliabilityCell::from_readout(c.tod_bucket.token(), c.day_type.token(), &c.readout)
        })
        .collect();

    Json(ReliabilityResponse {
        route,
        direction,
        stop,
        cells,
    })
}
