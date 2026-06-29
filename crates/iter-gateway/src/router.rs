use axum::Router;
use axum::routing::{get, post};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::state::AppState;
use crate::{
    correlate, enrich, glyphs, health, live_trains, manifest, offline, overlays, proxy,
    reliability, sprite, styles, tiles,
};

/// Assemble the gateway router from the capability modules' routes.
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
        // reliability: read the worker-written Tier-2 archive (ADR 0024)
        .route(
            "/reliability/{route}/{direction}/{stop}",
            get(reliability::reliability),
        )
        // routing + geocoding reverse-proxy to the external engines
        .route("/otp/gtfs/v1", post(proxy::routing))
        .route("/api", get(proxy::geocode_api))
        .route("/reverse", get(proxy::geocode_reverse))
        .route("/status", get(proxy::geocode_status))
        // place enrichment (open-first fusion above geocoding)
        .route("/places/enrich", get(enrich::enrich))
        .route("/places/image", get(enrich::image))
        .route("/places/related", get(correlate::related_places))
        // live-trains: generic handlers over the region's provider (ADR 0017)
        .route(
            "/trenitalia/stations/search",
            get(live_trains::stations_search),
        )
        .route("/trenitalia/stations", get(live_trains::stations_list))
        .route("/trenitalia/departures", get(live_trains::departures))
        .route("/trenitalia/arrivals", get(live_trains::arrivals))
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
