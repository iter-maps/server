//! Glyph atlas with the required one-font fallback: any requested fontstack
//! that we don't ship resolves to `NotoSans-Regular` rather than 404 (a missing
//! glyph range would otherwise break label rendering).

use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use iter_core::ApiError;

use crate::http::ApiResult;
use crate::state::AppState;

const FALLBACK_FONTSTACK: &str = "NotoSans-Regular";

pub async fn glyph(
    Path((fontstack, range)): Path<(String, String)>,
    State(state): State<AppState>,
) -> ApiResult<Response> {
    // Reject traversal: range is `<start>-<end>.pbf`, fontstack a single
    // path component.
    if !is_valid_range(&range) || is_unsafe_component(&fontstack) {
        return Err(ApiError::bad_request("invalid glyph request").into());
    }

    let primary = state.cfg.glyphs_dir.join(&fontstack).join(&range);
    let bytes = match tokio::fs::read(&primary).await {
        Ok(b) => b,
        Err(_) => {
            let fallback = state.cfg.glyphs_dir.join(FALLBACK_FONTSTACK).join(&range);
            tokio::fs::read(&fallback)
                .await
                .map_err(|_| ApiError::not_found("glyph range not available"))?
        }
    };

    Ok((
        [
            (header::CONTENT_TYPE, "application/x-protobuf"),
            (header::CACHE_CONTROL, "public, max-age=2592000, immutable"),
        ],
        bytes,
    )
        .into_response())
}

fn is_valid_range(range: &str) -> bool {
    let Some(nums) = range.strip_suffix(".pbf") else {
        return false;
    };
    matches!(nums.split_once('-'), Some((a, b))
        if !a.is_empty() && !b.is_empty()
        && a.bytes().all(|c| c.is_ascii_digit())
        && b.bytes().all(|c| c.is_ascii_digit()))
}

fn is_unsafe_component(s: &str) -> bool {
    s.is_empty() || s.contains('/') || s.contains('\\') || s.contains("..")
}
