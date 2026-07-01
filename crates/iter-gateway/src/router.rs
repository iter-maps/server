use axum::Router;
use axum::extract::Request;
use axum::http::Response;
use axum::routing::{get, post};
use std::time::Duration;
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::Span;

use crate::state::AppState;
use crate::{
    correlate, enrich, glyphs, health, live_trains, manifest, metrics, offline, overlays, proxy,
    reliability, request_id, sprite, styles, tiles,
};

/// Assemble the gateway router from the capability modules' routes.
pub fn build(state: AppState) -> Router {
    Router::new()
        .route("/livez", get(health::livez))
        .route("/readyz", get(health::readyz))
        // Internal operator metrics (ADR 0037): same proxy-gated posture as the
        // health probes — the external proxy must not expose it publicly.
        .route("/metrics", get(health::metrics))
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
        // Observability (ADR 0037): the TraceLayer opens one `gateway.request`
        // span per request and logs exactly one INFO outcome line with status +
        // latency; the request-id middleware runs inside that span so it records
        // `request_id` onto it and every request-scoped line carries it.
        //
        // The span carries `event="gateway.request"`, the method, the path, and
        // an empty `request_id` the middleware fills in; `on_response` logs status
        // + latency at INFO, `on_failure` a failed response at WARN — one clean
        // line per request without lowering the global filter.
        .layer(
            ServiceBuilder::new()
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(|req: &Request| {
                            tracing::info_span!(
                                "gateway.request",
                                event = "gateway.request",
                                method = %req.method(),
                                route = %req.uri().path(),
                                request_id = tracing::field::Empty,
                            )
                        })
                        .on_request(())
                        .on_body_chunk(())
                        .on_eos(())
                        .on_response(
                            |resp: &Response<axum::body::Body>, latency: Duration, _s: &Span| {
                                tracing::info!(
                                    outcome = "ok",
                                    status = resp.status().as_u16(),
                                    latency_ms = latency.as_millis() as u64,
                                    "request"
                                );
                            },
                        )
                        .on_failure(
                            |err: tower_http::classify::ServerErrorsFailureClass,
                             latency: Duration,
                             _s: &Span| {
                                tracing::warn!(
                                    outcome = "fail",
                                    error = %err,
                                    latency_ms = latency.as_millis() as u64,
                                    "request failed"
                                );
                            },
                        ),
                )
                .layer(axum::middleware::from_fn(request_id::propagate))
                // Per-request metrics (ADR 0037 phase 2): records the bounded
                // {method,status} counter + latency histogram. Fail-soft — a no-op
                // without a recorder, never alters the response.
                .layer(axum::middleware::from_fn(metrics::record)),
        )
        // The wire contract is CORS `*`, no auth — an external proxy owns
        // production CORS/TLS/rate-limit (P3).
        .layer(CorsLayer::permissive())
        .with_state(state)
}
