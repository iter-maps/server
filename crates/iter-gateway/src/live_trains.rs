//! Live-train boards (`/trenitalia/*`): the generic axum handlers and the
//! TTL-cache single-flight in front of the country's live-trains provider. The
//! upstream client — its endpoints, field names and date format — is a
//! country-specific driver behind [`crate::regions::LiveTrainsProvider`] (ADR
//! 0017); this module
//! carries no operator/country knowledge, only the cache keys and HTTP glue.

use std::time::Duration;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::http::{ApiErr, ApiResult};
use crate::regions::BoardKind;
use crate::state::AppState;

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
            provider.search(&http, &term).await.map_err(ApiErr::from)
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
            provider.list(&http, region).await.map_err(ApiErr::from)
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
                .map_err(ApiErr::from)
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
