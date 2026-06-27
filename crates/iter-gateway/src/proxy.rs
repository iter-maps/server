//! Reverse proxy to the external engines. Routing (`POST /otp/gtfs/v1`,
//! GraphQL) goes to OTP; geocoding (`/api`, `/reverse`, `/status`) goes to
//! Photon. The BFF passes these through today; itinerary re-ranking and
//! place-enrichment will hook in here later. Upstream responses are streamed so
//! large payloads don't buffer.

use axum::body::{Body, Bytes};
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, header};
use axum::response::Response;
use iter_core::ApiError;

use crate::http::ApiResult;
use crate::state::AppState;

pub async fn routing(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let url = format!("{}/otp/gtfs/v1", state.cfg.otp_url);
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_owned();

    let sent = state
        .http
        .post(&url)
        .header(header::CONTENT_TYPE, content_type)
        .body(body)
        .send()
        .await;
    relay(sent, "otp").await
}

pub async fn geocode_api(State(state): State<AppState>, RawQuery(q): RawQuery) -> ApiResult<Response> {
    geocode(&state, "/api", q).await
}

pub async fn geocode_reverse(
    State(state): State<AppState>,
    RawQuery(q): RawQuery,
) -> ApiResult<Response> {
    geocode(&state, "/reverse", q).await
}

pub async fn geocode_status(State(state): State<AppState>) -> ApiResult<Response> {
    geocode(&state, "/status", None).await
}

async fn geocode(state: &AppState, path: &str, query: Option<String>) -> ApiResult<Response> {
    let url = match query {
        Some(q) if !q.is_empty() => format!("{}{}?{}", state.cfg.photon_url, path, q),
        _ => format!("{}{}", state.cfg.photon_url, path),
    };
    let sent = state.http.get(&url).send().await;
    relay(sent, "photon").await
}

/// Map an upstream reqwest result onto a streamed axum response, translating
/// connection/timeout failures into the contract's error envelope.
async fn relay(
    sent: Result<reqwest::Response, reqwest::Error>,
    upstream: &str,
) -> ApiResult<Response> {
    let resp = sent.map_err(|e| upstream_error(&e, upstream))?;
    let status = resp.status();
    let content_type = resp.headers().get(header::CONTENT_TYPE).cloned();

    let mut builder = Response::builder().status(status.as_u16());
    if let Some(ct) = content_type {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    builder
        .body(Body::from_stream(resp.bytes_stream()))
        .map_err(|_| ApiError::internal("failed to build proxied response").into())
}

fn upstream_error(e: &reqwest::Error, upstream: &str) -> ApiError {
    if e.is_timeout() {
        ApiError::timeout(format!("{upstream} request timed out"))
    } else {
        ApiError::upstream_unavailable(format!("{upstream} is unavailable"))
    }
}
