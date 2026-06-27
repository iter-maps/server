//! HTTP glue: render the shared error envelope as an axum response, plus the
//! per-request base-URL used for `__BASE_URL__` substitution. The axum
//! dependency stays in the gateway so `iter-core` remains framework-agnostic.

use axum::Json;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use iter_core::ApiError;

/// Wraps [`ApiError`] so handlers can `return Err(..)` and get the
/// `{error:{code,message,details?}}` envelope with the right status.
pub struct ApiErr(pub ApiError);

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(self.0.envelope())).into_response()
    }
}

impl From<ApiError> for ApiErr {
    fn from(e: ApiError) -> Self {
        ApiErr(e)
    }
}

pub type ApiResult<T> = Result<T, ApiErr>;

/// Reconstruct the public base URL for `__BASE_URL__` rewriting: scheme from
/// `X-Forwarded-Proto` when the external proxy sets it, else the connection
/// scheme (http); host from the `Host` header.
pub fn base_url(headers: &HeaderMap) -> String {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    format!("{scheme}://{host}")
}
