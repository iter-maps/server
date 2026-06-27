//! MapLibre styles with per-request `__BASE_URL__` substitution. Every
//! host-absolute URL in a style is the literal token `__BASE_URL__`, rewritten
//! to `scheme://host` here so one file serves over LAN, Tailscale, the public
//! domain, and (after the client rewrites it to `file://`) offline.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use iter_contracts::offline::STYLE_WHITELIST;
use iter_core::ApiError;

use crate::http::{ApiResult, base_url};
use crate::state::AppState;

pub async fn style(
    Path(file): Path<String>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> ApiResult<Response> {
    let stem = file
        .strip_suffix(".json")
        .filter(|s| STYLE_WHITELIST.contains(s))
        .ok_or_else(|| ApiError::not_found(format!("unknown style '{file}'")))?;

    let path = state.cfg.styles_dir.join(format!("{stem}.json"));
    let raw = tokio::fs::read_to_string(&path)
        .await
        .map_err(|_| ApiError::not_found("style not available"))?;

    let body = raw.replace("__BASE_URL__", &base_url(&headers));

    Ok((
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "public, max-age=300"),
        ],
        body,
    )
        .into_response())
}
