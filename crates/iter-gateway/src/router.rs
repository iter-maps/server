use axum::Router;
use axum::routing::get;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::state::AppState;
use crate::{health, tiles};

/// Assemble the gateway router. Capability modules (tiles, styles, overlays,
/// offline, live-trains, routing/geocoding proxy, client health) attach their
/// sub-routers here as they land.
pub fn build(state: AppState) -> Router {
    Router::new()
        .route("/livez", get(health::livez))
        .route("/readyz", get(health::readyz))
        .merge(tiles::router(&state))
        .layer(TraceLayer::new_for_http())
        // The wire contract is CORS `*`, no auth — an external proxy owns
        // production CORS/TLS/rate-limit (P3).
        .layer(CorsLayer::permissive())
        .with_state(state)
}
