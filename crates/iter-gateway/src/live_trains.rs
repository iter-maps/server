//! Live-train boards (`/trenitalia/*`): the generic axum handlers and the
//! TTL-cache single-flight in front of the country's live-trains provider. The
//! upstream client — its endpoints, field names and date format — is a
//! country-specific driver behind [`iter_region_drivers::LiveTrainsProvider`]
//! (ADR 0017); this module
//! carries no operator/country knowledge, only the cache keys and HTTP glue.

use std::time::Duration;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use iter_core::ApiError;
use iter_region_drivers::BoardKind;
use serde::Deserialize;

use crate::http::{ApiErr, ApiResult};
use crate::state::AppState;

/// Map a provider's [`anyhow::Error`] to the gateway's error envelope. The
/// drivers (now in `iter-region-drivers`) report failures as `anyhow::Error`
/// rather than [`ApiError`], so the boundary lives here: a station-id validation
/// failure is a client error (400), and everything else is an upstream failure
/// the caller can retry (503).
fn provider_err(e: anyhow::Error) -> ApiErr {
    let msg = e.to_string();
    let api = if msg.contains("must match") {
        // A station-id validation failure is the caller's fault (400), not an
        // upstream failure — it stays off `upstream_errors_total`.
        ApiError::bad_request(msg)
    } else {
        let err = ApiError::new(503, iter_core::code::UPSTREAM_UNAVAILABLE, msg);
        // Mirror the genuine upstream failure as a metric, matching the catalog's
        // `upstream=viaggiatreno` (ADR 0037 phase 2). Bounded labels only:
        // `upstream` is the fixed engine name, `code` the stable `ApiError` code —
        // never the message/query. Fail-soft: a no-op without a recorder, and it
        // never alters the error surfaced to the caller.
        metrics::counter!(
            iter_core::metrics::UPSTREAM_ERRORS_TOTAL,
            iter_core::metrics::LABEL_UPSTREAM => "viaggiatreno",
            iter_core::metrics::LABEL_CODE => err.code.clone(),
        )
        .increment(1);
        err
    };
    ApiErr::from(api)
}

const SEARCH_TTL: Duration = Duration::from_secs(60 * 60);
const LIST_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const BOARD_TTL: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
pub struct SearchQuery {
    q: Option<String>,
}

#[derive(Deserialize)]
pub struct ListQuery {
    region: Option<i64>,
}

#[derive(Deserialize)]
pub struct BoardQuery {
    station: Option<String>,
}

pub async fn stations_search(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> ApiResult<Response> {
    let term = query.q.unwrap_or_default().trim().to_string();
    if term.chars().count() < 2 {
        return Err(iter_core::ApiError::bad_request("q must be at least 2 characters").into());
    }

    let key = format!("search:{}", term.to_lowercase());
    let provider = state.live_trains.clone();
    let http = state.http.clone();
    let result = state
        .stations
        .get_or_fetch(&key, SEARCH_TTL, || async move {
            provider.search(&http, &term).await.map_err(provider_err)
        })
        .await?;
    Ok(cached(Json(result), 600))
}

pub async fn stations_list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Response> {
    // The driver owns its own default region; `None` lets it fall back.
    let region = query.region;
    let key = format!("list:{}", region.unwrap_or_default());
    let provider = state.live_trains.clone();
    let http = state.http.clone();
    let result = state
        .stations
        .get_or_fetch(&key, LIST_TTL, || async move {
            provider.list(&http, region).await.map_err(provider_err)
        })
        .await?;
    Ok(cached(Json(result), 3600))
}

pub async fn departures(
    State(state): State<AppState>,
    Query(query): Query<BoardQuery>,
) -> ApiResult<Response> {
    board(state, query.station, BoardKind::Departures).await
}

pub async fn arrivals(
    State(state): State<AppState>,
    Query(query): Query<BoardQuery>,
) -> ApiResult<Response> {
    board(state, query.station, BoardKind::Arrivals).await
}

async fn board(state: AppState, station: Option<String>, kind: BoardKind) -> ApiResult<Response> {
    let station = station.unwrap_or_default();
    // The cache key buckets per minute; within a minute, callers coalesce onto
    // one upstream fetch. The wall clock is the gateway's — providers re-derive
    // their own upstream date param.
    let bucket = jiff::Zoned::now().strftime("%Y%m%d%H%M").to_string();
    let prefix = match kind {
        BoardKind::Departures => "dep",
        BoardKind::Arrivals => "arr",
    };
    let key = format!("{prefix}:{station}:{bucket}");
    let provider = state.live_trains.clone();
    let http = state.http.clone();
    let result = state
        .boards
        .get_or_fetch(&key, BOARD_TTL, || async move {
            provider
                .board(&http, &station, kind)
                .await
                .map_err(provider_err)
        })
        .await?;
    Ok(cached(Json(result), 20))
}

fn cached(body: impl IntoResponse, max_age: u32) -> Response {
    let mut resp = body.into_response();
    let value = HeaderValue::from_str(&format!("public, max-age={max_age}")).unwrap();
    resp.headers_mut().insert(header::CACHE_CONTROL, value);
    resp
}
