//! Reverse proxy to the external engines. Routing (`POST /otp/gtfs/v1`,
//! GraphQL) goes to OTP; geocoding (`/api`, `/reverse`, `/status`) goes to
//! Photon. The BFF passes these through today; place-enrichment hooks in
//! elsewhere. Upstream responses are streamed so large payloads don't buffer —
//! except on the opt-in `?rerank=reliability` routing path, which buffers the
//! plan to reorder its itineraries (ADR 0026).

use axum::body::{Body, Bytes};
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use iter_core::ApiError;
use iter_core::reliability::store_read::read_tier2_on_time_index;

use crate::http::ApiResult;
use crate::rerank::rerank_plan;
use crate::state::AppState;

/// The opt-in flag that turns the routing passthrough into a reliability rerank.
/// Absent (the default) keeps the handler a byte-for-byte streaming passthrough.
const RERANK_RELIABILITY: &str = "reliability";

/// Upper bound on the OTP plan body we will buffer to rerank (16 MiB), mirroring
/// the Tier-2 read cap. A plan past this is streamed through unchanged rather than
/// buffered+cloned — the rerank is a soft enhancement, not worth an unbounded
/// allocation from a buggy or hostile upstream.
const RERANK_MAX_BODY_BYTES: u64 = 16 * 1024 * 1024;

pub async fn routing(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
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

    // Default path: stream the upstream response through unchanged. Only when the
    // caller opts in do we buffer+rerank (ADR 0026) — so existing routing never
    // regresses.
    if rerank_requested(query.as_deref()) {
        rerank_routing(&state, sent).await
    } else {
        relay(sent, "otp").await
    }
}

/// True when the raw query string carries `rerank=reliability`. Other `rerank`
/// values (or none) leave the handler a passthrough.
fn rerank_requested(query: Option<&str>) -> bool {
    let Some(q) = query else { return false };
    q.split('&')
        .filter_map(|pair| pair.split_once('='))
        .any(|(k, v)| k == "rerank" && v == RERANK_RELIABILITY)
}

/// Opt-in routing path: buffer the OTP plan, reorder its itineraries by
/// reliability, and return the rewritten JSON. Fail-soft at every step — a
/// transport error, a non-`200`, a non-JSON or non-plan body, or a body that the
/// rerank core declines all return the original response unchanged. Never 500s,
/// never drops an itinerary.
async fn rerank_routing(
    state: &AppState,
    sent: Result<reqwest::Response, reqwest::Error>,
) -> ApiResult<Response> {
    let resp = sent.map_err(|e| upstream_error(&e, "otp"))?;
    let status = resp.status();

    // A non-`200`, or a plan whose advertised size blows past the buffer cap, is
    // streamed straight through — we never buffer those. Only a successful,
    // plausibly-sized plan is buffered+reranked.
    let oversize = resp
        .content_length()
        .is_some_and(|len| len > RERANK_MAX_BODY_BYTES);
    if status != StatusCode::OK || oversize {
        return relay(Ok(resp), "otp").await;
    }

    let content_type = resp.headers().get(header::CONTENT_TYPE).cloned();
    // Buffer the upstream body. A read error here is a genuine upstream failure.
    let bytes = resp.bytes().await.map_err(|e| upstream_error(&e, "otp"))?;

    // Rerank on a blocking worker (a Tier-2 file read plus a JSON re-parse). The
    // closure owns the buffered bytes and returns either the rewritten body or the
    // original verbatim — an OTP error envelope or non-JSON body declines the
    // rerank and passes through unchanged. No second copy of the body is kept.
    let root = state.cfg.reliability_dir.clone();
    let body = tokio::task::spawn_blocking(move || {
        try_rerank(&root, &bytes).unwrap_or_else(|| bytes.to_vec())
    })
    .await
    .map_err(|_| ApiError::internal("rerank worker panicked"))?;

    let mut builder = Response::builder().status(status.as_u16());
    if let Some(ct) = content_type {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    builder
        .body(Body::from(body))
        .map_err(|_| ApiError::internal("failed to build reranked response").into())
}

/// Parse `bytes` as an OTP plan and reorder its itineraries by reliability,
/// returning the rewritten JSON bytes — or `None` when the body isn't a plan or
/// the core declines (in which case the caller returns the original bytes). Reads
/// the shared Tier-2 archive once (same dir as the read endpoint, ADR 0024) into
/// a per-stop on-time-rate index; the rerank core looks legs up against it. This
/// runs on a blocking worker (it touches the filesystem).
fn try_rerank(root: &std::path::Path, bytes: &[u8]) -> Option<Vec<u8>> {
    let mut plan: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    // One bounded, fail-soft file read; an absent/corrupt store yields an empty
    // index, so every leg misses and itineraries hold their order.
    let index = read_tier2_on_time_index(root);
    let lookup = |route: &str, direction: i32, stop: &str| {
        index
            .get(&(route.to_string(), direction, stop.to_string()))
            .copied()
    };
    if rerank_plan(&mut plan, &lookup) {
        serde_json::to_vec(&plan).ok()
    } else {
        None
    }
}

pub async fn geocode_api(
    State(state): State<AppState>,
    RawQuery(q): RawQuery,
) -> ApiResult<Response> {
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

#[cfg(test)]
mod tests {
    use super::rerank_requested;

    #[test]
    fn rerank_requested_only_fires_for_the_exact_flag() {
        // The opt-in boundary: only `rerank=reliability`, in any position, triggers.
        // Everything else stays a passthrough (default-off contract, ADR 0026).
        for absent in [
            None,
            Some(""),
            Some("rerank=foo"),          // wrong value
            Some("rerank="),             // bare value
            Some("rerankX=reliability"), // not the rerank key
            Some("rerank=reliabilityX"), // value is a superstring, not equal
            Some("other=1"),
        ] {
            assert!(!rerank_requested(absent), "should not fire: {absent:?}");
        }
        for present in [
            "rerank=reliability",
            "x=1&rerank=reliability", // flag not first
            "rerank=reliability&y=2", // extra param after
            "a=b&rerank=reliability&c=d",
        ] {
            assert!(rerank_requested(Some(present)), "should fire: {present}");
        }
    }
}
