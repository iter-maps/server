use axum::Router;
use axum::routing::{get, post};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::state::AppState;
use crate::{
    correlate, enrich, glyphs, health, manifest, offline, overlays, proxy, sprite, styles, tiles,
    trenitalia,
};

/// Assemble the gateway router. Capability modules (tiles, styles, overlays,
/// offline, live-trains, routing/geocoding proxy, client health) attach their
/// sub-routers here as they land.
pub fn build(state: AppState) -> Router {
    Router::new()
        .route("/livez", get(health::livez))
        .route("/readyz", get(health::readyz))
        .route("/health", get(health::client_health))
        .route("/health.json", get(health::client_health))
        .route("/manifest", get(manifest::manifest))
        .route("/styles/{file}", get(styles::style))
        .route("/glyphs/{fontstack}/{range}", get(glyphs::glyph))
        .route("/overlays/{file}", get(overlays::overlay))
        // routing + geocoding reverse-proxy to the external engines
        .route("/otp/gtfs/v1", post(proxy::routing))
        .route("/api", get(proxy::geocode_api))
        .route("/reverse", get(proxy::geocode_reverse))
        .route("/status", get(proxy::geocode_status))
        // place enrichment (open-first fusion above geocoding)
        .route("/places/enrich", get(enrich::enrich))
        .route("/places/image", get(enrich::image))
        .route("/places/related", get(correlate::related_places))
        // live-trains (ViaggiaTreno proxy)
        .route(
            "/trenitalia/stations/search",
            get(trenitalia::stations_search),
        )
        .route("/trenitalia/stations", get(trenitalia::stations_list))
        .route("/trenitalia/departures", get(trenitalia::departures))
        .route("/trenitalia/arrivals", get(trenitalia::arrivals))
        .route("/offline/extract", get(offline::extract))
        .route("/offline/bundle", get(offline::bundle))
        .merge(tiles::router(&state))
        .merge(sprite::router(&state))
        .layer(TraceLayer::new_for_http())
        // The wire contract is CORS `*`, no auth — an external proxy owns
        // production CORS/TLS/rate-limit (P3).
        .layer(CorsLayer::permissive())
        .with_state(state)
}
