//! Reverse proxy to the external engines. Routing (`POST /otp/gtfs/v1`,
//! GraphQL) goes to OTP; geocoding (`/api`, `/reverse`, `/status`) goes to
//! Photon. The BFF passes these through today; place-enrichment hooks in
//! elsewhere. Upstream responses are streamed so large payloads don't buffer —
//! except on the opt-in routing post-processing paths, which buffer the plan to
//! reorder its itineraries by a soft composite score (`?rerank=<profile>`, ADR
//! 0026, 0028) and/or annotate RT-less transit legs with a historical typical
//! delay (`?predict=historical`, ADR 0030). Both compose on one buffered plan.

use axum::body::{Body, Bytes};
use axum::extract::{Extension, RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use iter_core::ApiError;
use iter_core::reliability::rollup::Tier2;
use iter_core::reliability::store_read::{on_time_index_from, typical_delay_index_from};

use crate::annotate::{DelayLookup, TypicalDelay, annotate_plan};
use crate::http::ApiResult;
use crate::request_id::{REQUEST_ID_HEADER, RequestId};
use crate::rerank::{Profile, rerank_plan};
use crate::state::AppState;
use crate::weather::{JourneyWeather, journey_sample_points};

/// Upper bound on the OTP plan body we will buffer to rerank (16 MiB), mirroring
/// the Tier-2 read cap. A plan past this is streamed through unchanged rather than
/// buffered+cloned — the rerank is a soft enhancement, not worth an unbounded
/// allocation from a buggy or hostile upstream.
const RERANK_MAX_BODY_BYTES: u64 = 16 * 1024 * 1024;

pub async fn routing(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    request_id: Option<Extension<RequestId>>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let url = format!("{}/otp/gtfs/v1", state.cfg.otp_url);
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_owned();

    let sent = with_request_id(
        state
            .http
            .post(&url)
            .header(header::CONTENT_TYPE, content_type)
            .body(body),
        request_id.as_ref(),
    )
    .send()
    .await;

    // Default path: stream the upstream response through unchanged. Only when the
    // caller opts into a post-processing step — a recognized `rerank=<profile>`
    // (ADR 0026, 0028) and/or `predict=historical` (ADR 0030) — do we buffer the
    // plan and transform it, so existing routing never regresses. Both compose on
    // the same single buffer.
    let profile = rerank_profile(query.as_deref());
    let predict = predict_requested(query.as_deref());
    if profile.is_some() || predict {
        postprocess_routing(&state, sent, profile, predict).await
    } else {
        relay(sent, "otp").await
    }
}

/// Whether the raw query string opts into historical no-RT delay prediction via
/// `predict=historical` (ADR 0030). Any other value (or its absence) is `false`,
/// keeping the default path a passthrough. The token is matched exactly so a
/// superstring like `historicalX` does not trigger.
fn predict_requested(query: Option<&str>) -> bool {
    let Some(q) = query else {
        return false;
    };
    q.split('&')
        .filter_map(|pair| pair.split_once('='))
        .any(|(k, v)| k == "predict" && v == "historical")
}

/// The rerank [`Profile`] requested by the raw query string's `rerank=<profile>`
/// flag, or `None` when the flag is absent or names an unknown profile — in which
/// case the handler stays a passthrough. `rerank=reliability` preserves the
/// wave-1 contract (ADR 0026); `balanced`/`eco`/`comfort` select composite
/// weightings (ADR 0028).
fn rerank_profile(query: Option<&str>) -> Option<Profile> {
    let q = query?;
    q.split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == "rerank")
        .and_then(|(_, v)| Profile::from_flag(v))
}

/// Whether a proxied routing response should be buffered and post-processed: only
/// a `200` advertising a `Content-Length` within the cap. A missing length (we
/// will not buffer an unbounded body) or an oversized one streams through
/// unchanged, so the buffered body is always bounded by `RERANK_MAX_BODY_BYTES`.
fn rerankable(status: StatusCode, content_length: Option<u64>) -> bool {
    status == StatusCode::OK && content_length.is_some_and(|len| len <= RERANK_MAX_BODY_BYTES)
}

/// Opt-in routing path: buffer the OTP plan once and apply the requested
/// post-processing steps — reranking (ADR 0026/0028) and/or historical no-RT
/// delay annotation (ADR 0030) — then return the rewritten JSON. Fail-soft at
/// every step: a transport error surfaces as the contract's upstream envelope; a
/// non-`200`, an oversized/unbounded body, a non-JSON or non-plan body, or a body
/// that every transform declines all return the original response unchanged.
/// Never 500s, never drops or reorders an itinerary other than the rerank's own
/// stable reorder.
async fn postprocess_routing(
    state: &AppState,
    sent: Result<reqwest::Response, reqwest::Error>,
    profile: Option<Profile>,
    predict: bool,
) -> ApiResult<Response> {
    let resp = sent.map_err(|e| upstream_error(&e, "otp"))?;
    let status = resp.status();

    // Only a successful plan advertising a within-cap `Content-Length` is
    // buffered+transformed. A non-`200`, an oversized body, or a body with no
    // advertised length (which we will not buffer unbounded) streams straight
    // through unchanged — fail-soft, and the buffered body is always bounded.
    if !rerankable(status, resp.content_length()) {
        return relay(Ok(resp), "otp").await;
    }

    let content_type = resp.headers().get(header::CONTENT_TYPE).cloned();
    // Buffer the upstream body. A read error here is a genuine upstream failure.
    let bytes = resp.bytes().await.map_err(|e| upstream_error(&e, "otp"))?;

    // Resolve the journey's per-segment weather for the rerank factor (ADR 0033,
    // 0036) before the blocking transform, since the fetch is async. Only the rerank
    // path needs it; it is empty when weather is disabled, the plan has no usable
    // sample points, or the fetch fails — in every case the factor stays neutral and
    // the rerank still completes. The fetch is one short-timeout, fail-soft
    // multi-location call, so it never stalls or fails the routing response.
    let weather = if profile.is_some() {
        journey_weather(state, &bytes).await
    } else {
        JourneyWeather::new()
    };

    // Transform on a blocking worker (a Tier-2 map fetch plus a JSON re-parse). The
    // closure owns the buffered bytes and returns either the rewritten body or the
    // original verbatim — an OTP error envelope or non-JSON body declines every
    // transform and passes through unchanged. No second copy of the body is kept.
    // The Tier-2 map comes from the mtime-validated cache (ADR 0032): a cheap Arc
    // clone on a hit, a re-read+parse only when the worker rewrote the file. The
    // cache lock is released inside `map()`, so it is never held across an await.
    let cache = state.reliability.clone();
    let body = tokio::task::spawn_blocking(move || {
        try_postprocess(&cache.map(), &bytes, profile, predict, &weather)
            .unwrap_or_else(|| bytes.to_vec())
    })
    .await
    .map_err(|_| ApiError::internal("routing post-process worker panicked"))?;

    let mut builder = Response::builder().status(status.as_u16());
    if let Some(ct) = content_type {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    builder
        .body(Body::from(body))
        .map_err(|_| ApiError::internal("failed to build post-processed response").into())
}

/// Resolve the journey's per-segment sampled weather for the rerank factor (ADR
/// 0033, 0036): the distinct sample points (origin, transfers, destination, walk
/// legs), fetched in one bounded multi-location call with per-point caching. Empty
/// when weather is disabled, the body has no parseable plan/sample points, or the
/// fetch fails — in which case every segment stays neutral and the rerank still
/// completes. Each sample point is coarsened to ~1 km before it leaves for the third
/// party (privacy posture, ADR 0033). Fail-soft and short-timeout at every step.
async fn journey_weather(state: &AppState, bytes: &[u8]) -> JourneyWeather {
    let Some(client) = state.weather_client.as_ref() else {
        return JourneyWeather::new();
    };
    let Ok(plan) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return JourneyWeather::new();
    };
    let points = journey_sample_points(&plan);
    if points.is_empty() {
        return JourneyWeather::new();
    }
    state
        .weather_cache
        .get_or_fetch_journey(client, &points)
        .await
}

/// Parse `bytes` as an OTP plan and apply the requested transforms in order:
/// rerank the itineraries (when a profile is set) and annotate RT-less transit
/// legs with their historical typical delay (when `predict`). Returns the
/// rewritten JSON bytes — or `None` when the body isn't a plan or no transform
/// touched it (in which case the caller returns the original bytes). Each enabled
/// transform derives its index from the same already-parsed Tier-2 map (served by
/// the gateway's mtime-validated cache, ADR 0032 — same archive as the read
/// endpoint, ADR 0024). `weather` carries the journey's per-segment sampled weather
/// for the rerank's weather factor (ADR 0033, 0036), empty to leave that factor
/// neutral. This runs on a blocking worker.
fn try_postprocess(
    map: &std::collections::BTreeMap<String, Tier2>,
    bytes: &[u8],
    profile: Option<Profile>,
    predict: bool,
    weather: &JourneyWeather,
) -> Option<Vec<u8>> {
    let mut plan: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let mut touched = false;

    if let Some(profile) = profile {
        // Derive the on-time index from the cached map; an absent/corrupt store
        // yields an empty map, so every leg misses and itineraries hold their order.
        let index = on_time_index_from(map);
        let lookup = |route: &str, direction: i32, stop: &str| {
            index
                .get(&(route.to_string(), direction, stop.to_string()))
                .copied()
        };
        // A non-plan body makes `rerank_plan` return false; we leave `touched` so
        // the response passes through verbatim unless another transform fires.
        touched |= rerank_plan(&mut plan, &lookup, profile, weather);
    }

    if predict {
        // The typical-delay index from the same cached map. Annotation never
        // reorders or overrides live RT; it only fills gaps additively.
        let index = typical_delay_index_from(map);
        let lookup: &DelayLookup<'_> = &|route: &str, direction: i32, stop: &str| {
            index
                .get(&(route.to_string(), direction, stop.to_string()))
                .map(|td| TypicalDelay {
                    p50_s: td.p50_s,
                    p85_s: td.p85_s,
                    count: td.count,
                })
        };
        touched |= annotate_plan(&mut plan, lookup);
    }

    if touched {
        serde_json::to_vec(&plan).ok()
    } else {
        None
    }
}

pub async fn geocode_api(
    State(state): State<AppState>,
    RawQuery(q): RawQuery,
    request_id: Option<Extension<RequestId>>,
) -> ApiResult<Response> {
    geocode(&state, "/api", q, request_id.as_ref()).await
}

pub async fn geocode_reverse(
    State(state): State<AppState>,
    RawQuery(q): RawQuery,
    request_id: Option<Extension<RequestId>>,
) -> ApiResult<Response> {
    geocode(&state, "/reverse", q, request_id.as_ref()).await
}

pub async fn geocode_status(
    State(state): State<AppState>,
    request_id: Option<Extension<RequestId>>,
) -> ApiResult<Response> {
    geocode(&state, "/status", None, request_id.as_ref()).await
}

async fn geocode(
    state: &AppState,
    path: &str,
    query: Option<String>,
    request_id: Option<&Extension<RequestId>>,
) -> ApiResult<Response> {
    let url = match query {
        Some(q) if !q.is_empty() => format!("{}{}?{}", state.cfg.photon_url, path, q),
        _ => format!("{}{}", state.cfg.photon_url, path),
    };
    let sent = with_request_id(state.http.get(&url), request_id)
        .send()
        .await;
    relay(sent, "photon").await
}

/// Attach the current request's `x-request-id` to an outbound engine call so a
/// gateway log line correlates with the engine-side one (ADR 0037). A no-op when
/// the id is absent (the request-id middleware always sets it in practice).
fn with_request_id(
    builder: reqwest::RequestBuilder,
    request_id: Option<&Extension<RequestId>>,
) -> reqwest::RequestBuilder {
    match request_id {
        Some(Extension(RequestId(id))) => builder.header(REQUEST_ID_HEADER.as_str(), id),
        None => builder,
    }
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
    let err = if e.is_timeout() {
        ApiError::timeout(format!("{upstream} request timed out"))
    } else {
        ApiError::upstream_unavailable(format!("{upstream} is unavailable"))
    };
    // Surface the previously-silent upstream failure (ADR 0037). The response
    // envelope is unchanged; this only adds one correlated log line.
    //
    // Log a scrubbed failure *kind*, never `%e`: reqwest attaches the full
    // request URL to send/timeout/connect errors (`.with_url`), and its `Display`
    // appends ` for url (...)`. On the Photon geocode hop that URL carries the raw
    // user query (free-text place search on `/api`, lat/lon on `/reverse`), so
    // `%e` would leak user data into the operator log — which ADR 0037/0024
    // forbid. `error.code` + `upstream` already identify the failure class.
    tracing::warn!(
        event = "proxy.upstream",
        outcome = "fail",
        upstream,
        error.code = %err.code,
        cause = failure_kind(e),
        "upstream request failed"
    );
    // Mirror the WARN as a metric (ADR 0037 phase 2). Bounded labels only:
    // `upstream` is the small engine set, `code` the stable `ApiError` code — never
    // the URL/query. Fail-soft: a no-op without a recorder, never alters the error.
    metrics::counter!(
        iter_core::metrics::UPSTREAM_ERRORS_TOTAL,
        iter_core::metrics::LABEL_UPSTREAM => upstream.to_owned(),
        iter_core::metrics::LABEL_CODE => err.code.clone(),
    )
    .increment(1);
    err
}

/// A fixed, URL-free label for a reqwest failure, so the log never echoes the
/// request URL (and, on the geocode hop, the user's query embedded in it).
fn failure_kind(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else if e.is_request() {
        "request"
    } else if e.is_body() {
        "body"
    } else if e.is_decode() {
        "decode"
    } else {
        "other"
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::{Profile, RERANK_MAX_BODY_BYTES, predict_requested, rerank_profile, rerankable};

    #[test]
    fn rerankable_only_for_ok_within_cap_advertised_length() {
        // Bounded-buffer contract: rerank only a 200 advertising a within-cap
        // length. A missing length (chunked) is never buffered — it would be an
        // unbounded read — so it streams through unchanged, as do oversized and
        // non-200 bodies.
        assert!(rerankable(StatusCode::OK, Some(0)));
        assert!(rerankable(StatusCode::OK, Some(RERANK_MAX_BODY_BYTES)));
        assert!(!rerankable(StatusCode::OK, Some(RERANK_MAX_BODY_BYTES + 1)));
        assert!(!rerankable(StatusCode::OK, None));
        assert!(!rerankable(StatusCode::BAD_GATEWAY, Some(10)));
        assert!(!rerankable(StatusCode::NOT_FOUND, None));
    }

    #[test]
    fn rerank_profile_is_none_for_absent_or_unknown_flags() {
        // The opt-in boundary: only a recognized profile triggers; everything else
        // stays a passthrough (default-off contract, ADR 0026/0028).
        for absent in [
            None,
            Some(""),
            Some("rerank=foo"),          // unknown profile
            Some("rerank="),             // bare value
            Some("rerankX=reliability"), // not the rerank key
            Some("rerank=reliabilityX"), // value is a superstring, not a profile
            Some("other=1"),
        ] {
            assert_eq!(rerank_profile(absent), None, "should not fire: {absent:?}");
        }
    }

    #[test]
    fn rerank_profile_parses_each_profile_in_any_position() {
        for (q, want) in [
            ("rerank=reliability", Profile::Reliability),
            ("x=1&rerank=reliability", Profile::Reliability), // flag not first
            ("rerank=balanced&y=2", Profile::Balanced),       // extra param after
            ("a=b&rerank=eco&c=d", Profile::Eco),
            ("rerank=comfort", Profile::Comfort),
        ] {
            assert_eq!(rerank_profile(Some(q)), Some(want), "should fire: {q}");
        }
    }

    #[test]
    fn predict_requested_only_for_the_exact_historical_token() {
        // Fires only on `predict=historical`, in any position.
        for q in [
            "predict=historical",
            "x=1&predict=historical",
            "predict=historical&y=2",
            "rerank=eco&predict=historical", // composes with rerank
        ] {
            assert!(predict_requested(Some(q)), "should fire: {q}");
        }
        // Stays off otherwise (default-off contract, ADR 0030).
        for q in [
            None,
            Some(""),
            Some("predict=foo"),         // unknown value
            Some("predict="),            // bare value
            Some("predict=historicalX"), // superstring, not the token
            Some("predictX=historical"), // not the predict key
            Some("rerank=eco"),          // a different opt-in only
        ] {
            assert!(!predict_requested(q), "should not fire: {q:?}");
        }
    }
}
