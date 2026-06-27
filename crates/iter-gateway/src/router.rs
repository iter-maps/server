use axum::Router;
use axum::routing::{get, post};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::state::AppState;
use crate::{glyphs, health, overlays, proxy, sprite, styles, tiles};

/// Assemble the gateway router. Capability modules (tiles, styles, overlays,
/// offline, live-trains, routing/geocoding proxy, client health) attach their
/// sub-routers here as they land.
pub fn build(state: AppState) -> Router {
    Router::new()
        .route("/livez", get(health::livez))
        .route("/readyz", get(health::readyz))
        .route("/styles/{file}", get(styles::style))
        .route("/glyphs/{fontstack}/{range}", get(glyphs::glyph))
        .route("/overlays/{file}", get(overlays::overlay))
        // routing + geocoding reverse-proxy to the external engines
        .route("/otp/gtfs/v1", post(proxy::routing))
        .route("/api", get(proxy::geocode_api))
        .route("/reverse", get(proxy::geocode_reverse))
        .route("/status", get(proxy::geocode_status))
        .merge(tiles::router(&state))
        .merge(sprite::router(&state))
        .layer(TraceLayer::new_for_http())
        // The wire contract is CORS `*`, no auth — an external proxy owns
        // production CORS/TLS/rate-limit (P3).
        .layer(CorsLayer::permissive())
        .with_state(state)
}
