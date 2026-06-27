//! Transit overlays the client draws over the map: the metro-station cutouts
//! and the street-snapped line geometries. Fail-soft — an overlay that hasn't
//! been generated yet returns an empty FeatureCollection so the client draws
//! nothing rather than erroring.

use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use iter_core::ApiError;

use crate::http::ApiResult;
use crate::state::AppState;

const EMPTY_FC: &str = r#"{"type":"FeatureCollection","features":[]}"#;

pub async fn overlay(
    Path(file): Path<String>,
    State(state): State<AppState>,
) -> ApiResult<Response> {
    // The served kinds come from the resolved region (region.overlays), so a
    // region with different overlays needs no code change (ADR 0008 / 0013).
    let stem = file
        .strip_suffix(".geojson")
        .filter(|s| state.cfg.overlay_kinds.iter().any(|k| k == s))
        .ok_or_else(|| ApiError::not_found(format!("unknown overlay '{file}'")))?;

    let path = state.cfg.overlays_dir.join(format!("{stem}.geojson"));
    let body = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(_) => EMPTY_FC.as_bytes().to_vec(),
    };

    Ok((
        [
            (header::CONTENT_TYPE, "application/geo+json"),
            (header::CACHE_CONTROL, "public, max-age=300"),
        ],
        body,
    )
        .into_response())
}
