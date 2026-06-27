//! Offline map pre-download. `/offline/extract` range-reads the clustered
//! basemap PMTiles into a bbox-clipped archive (no tile re-render) via the
//! pinned `go-pmtiles` CLI (concept ADR 0010). Because the surface is public
//! and auth-less, the validation caps below — bbox sanity, a maximum area, a
//! zoom clamp, and a concurrency gate — are the only abuse protection.

use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use iter_contracts::geo::{BBox, BBoxError};
use iter_contracts::offline::code;
use iter_core::ApiError;
use serde::Deserialize;

use crate::config::{GatewayConfig, OfflineCaps};
use crate::http::{ApiErr, ApiResult};
use crate::state::AppState;

#[derive(Debug, PartialEq)]
pub struct ExtractParams {
    pub bbox: BBox,
    pub minzoom: u8,
    pub maxzoom: u8,
}

#[derive(Deserialize)]
pub struct ExtractQuery {
    bbox: Option<String>,
    minzoom: Option<u8>,
    maxzoom: Option<u8>,
}

/// Validate and normalize the request against the caps. Pure — the unit tests
/// exercise every rejection path; the handler adds the concurrency gate and the
/// CLI shell-out.
pub fn validate(
    bbox: Option<&str>,
    minzoom: Option<u8>,
    maxzoom: Option<u8>,
    caps: &OfflineCaps,
) -> Result<ExtractParams, ApiError> {
    let raw = bbox
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::new(400, code::BBOX_REQUIRED, "bbox is required"))?;

    let bbox = BBox::parse(raw).map_err(|e| {
        let (c, msg) = match e {
            BBoxError::Invalid => (
                code::BBOX_INVALID,
                "bbox must be minLon,minLat,maxLon,maxLat",
            ),
            BBoxError::OutOfRange => (code::BBOX_OUT_OF_RANGE, "bbox coordinates out of range"),
            BBoxError::Degenerate => (code::BBOX_DEGENERATE, "bbox min must be below max"),
        };
        ApiError::new(400, c, msg)
    })?;

    if bbox.area_deg2() > caps.max_area_deg2 {
        return Err(ApiError::new(
            413,
            code::AREA_TOO_LARGE,
            format!(
                "bbox area {:.2} deg^2 exceeds cap {}",
                bbox.area_deg2(),
                caps.max_area_deg2
            ),
        ));
    }

    // Zoom is silently clamped (not rejected): maxzoom to the cap, minzoom into
    // [0, maxzoom].
    let maxzoom = maxzoom.unwrap_or(caps.max_zoom).min(caps.max_zoom);
    let minzoom = minzoom.unwrap_or(0).min(maxzoom);

    Ok(ExtractParams {
        bbox,
        minzoom,
        maxzoom,
    })
}

pub async fn extract(
    State(state): State<AppState>,
    Query(query): Query<ExtractQuery>,
) -> ApiResult<Response> {
    let params = validate(
        query.bbox.as_deref(),
        query.minzoom,
        query.maxzoom,
        &state.cfg.offline,
    )
    .map_err(ApiErr)?;

    // Concurrency gate, no queue.
    let _permit = state
        .offline_gate
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::new(503, code::BUSY, "too many concurrent extracts"))?;

    let bytes = run_extract(&state.cfg, &params).await?;
    let filename = format!("iter-offline-z{}.pmtiles", params.maxzoom);

    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

async fn run_extract(cfg: &GatewayConfig, params: &ExtractParams) -> Result<Vec<u8>, ApiErr> {
    let dir = tempfile::tempdir().map_err(|_| extract_failed())?;
    let out = dir.path().join("area.pmtiles");
    let b = &params.bbox;

    let output = tokio::process::Command::new(&cfg.pmtiles_bin)
        .arg("extract")
        .arg(&cfg.offline_source)
        .arg(&out)
        .arg(format!(
            "--bbox={},{},{},{}",
            b.min_lon, b.min_lat, b.max_lon, b.max_lat
        ))
        .arg(format!("--maxzoom={}", params.maxzoom))
        .output()
        .await
        .map_err(|_| extract_failed())?;

    if !output.status.success() {
        return Err(extract_failed().into());
    }
    let bytes = tokio::fs::read(&out).await.map_err(|_| extract_failed())?;
    Ok(bytes)
}

fn extract_failed() -> ApiError {
    ApiError::new(500, code::EXTRACT_FAILED, "PMTiles extract failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> OfflineCaps {
        OfflineCaps {
            max_area_deg2: 6.0,
            max_zoom: 14,
            max_concurrent: 3,
        }
    }

    #[test]
    fn missing_bbox_is_required() {
        let err = validate(None, None, None, &caps()).unwrap_err();
        assert_eq!(err.status, 400);
        assert_eq!(err.code, code::BBOX_REQUIRED);
        let err = validate(Some("  "), None, None, &caps()).unwrap_err();
        assert_eq!(err.code, code::BBOX_REQUIRED);
    }

    #[test]
    fn bbox_error_codes() {
        assert_eq!(
            validate(Some("1,2,3"), None, None, &caps())
                .unwrap_err()
                .code,
            code::BBOX_INVALID
        );
        assert_eq!(
            validate(Some("0,0,200,1"), None, None, &caps())
                .unwrap_err()
                .code,
            code::BBOX_OUT_OF_RANGE
        );
        assert_eq!(
            validate(Some("5,5,5,6"), None, None, &caps())
                .unwrap_err()
                .code,
            code::BBOX_DEGENERATE
        );
    }

    #[test]
    fn area_cap_rejects_oversized() {
        // 10x10 deg = 100 deg^2 > 6.
        let err = validate(Some("0,0,10,10"), None, None, &caps()).unwrap_err();
        assert_eq!(err.status, 413);
        assert_eq!(err.code, code::AREA_TOO_LARGE);
    }

    #[test]
    fn zoom_is_clamped_not_rejected() {
        let p = validate(Some("12.4,41.8,12.6,42.0"), Some(99), Some(99), &caps()).unwrap();
        assert_eq!(p.maxzoom, 14, "maxzoom clamped to the cap");
        assert_eq!(p.minzoom, 14, "minzoom clamped into [0, maxzoom]");

        let p = validate(Some("12.4,41.8,12.6,42.0"), None, None, &caps()).unwrap();
        assert_eq!(p.maxzoom, 14);
        assert_eq!(p.minzoom, 0);
    }

    #[test]
    fn accepts_small_bbox() {
        let p = validate(Some("12.4,41.8,12.6,42.0"), Some(5), Some(12), &caps()).unwrap();
        assert_eq!(p.minzoom, 5);
        assert_eq!(p.maxzoom, 12);
        assert!((p.bbox.min_lon - 12.4).abs() < 1e-9);
    }
}
