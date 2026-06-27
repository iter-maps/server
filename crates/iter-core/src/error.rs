//! The uniform error envelope shared by every non-GraphQL surface:
//! `{ "error": { "code", "message", "details"? } }`.

use serde::Serialize;
use serde_json::Value;

/// Stable error codes used across the wire contract.
pub mod code {
    pub const BAD_REQUEST: &str = "BAD_REQUEST";
    pub const NOT_FOUND: &str = "NOT_FOUND";
    pub const AREA_TOO_LARGE: &str = "AREA_TOO_LARGE";
    pub const BUSY: &str = "BUSY";
    pub const UPSTREAM_UNAVAILABLE: &str = "UPSTREAM_UNAVAILABLE";
    pub const UPSTREAM_ERROR: &str = "UPSTREAM_ERROR";
    pub const TIMEOUT: &str = "TIMEOUT";
    pub const INTERNAL: &str = "INTERNAL";
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip)]
    pub status: u16,
}

impl ApiError {
    pub fn new(status: u16, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
            status,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, code::BAD_REQUEST, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(404, code::NOT_FOUND, message)
    }

    pub fn area_too_large(message: impl Into<String>) -> Self {
        Self::new(413, code::AREA_TOO_LARGE, message)
    }

    pub fn busy(message: impl Into<String>) -> Self {
        Self::new(503, code::BUSY, message)
    }

    pub fn upstream_unavailable(message: impl Into<String>) -> Self {
        Self::new(502, code::UPSTREAM_UNAVAILABLE, message)
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(504, code::TIMEOUT, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(500, code::INTERNAL, message)
    }

    /// Render the full `{ "error": { .. } }` envelope.
    pub fn envelope(&self) -> Value {
        serde_json::json!({ "error": self })
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}): {}", self.status, self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_shape_matches_contract() {
        let err = ApiError::area_too_large("bbox area 7.0 deg^2 exceeds cap 6")
            .with_details(serde_json::json!({ "cap": 6 }));
        let v = err.envelope();
        assert_eq!(v["error"]["code"], "AREA_TOO_LARGE");
        assert_eq!(v["error"]["details"]["cap"], 6);
        assert!(
            v["error"].get("status").is_none(),
            "status is transport-level, not serialized"
        );
    }

    #[test]
    fn details_omitted_when_absent() {
        let v = ApiError::bad_request("missing bbox").envelope();
        assert!(v["error"].get("details").is_none());
    }

    #[test]
    fn new_sets_all_fields() {
        let err = ApiError::new(418, "TEAPOT", "short and stout");
        assert_eq!(err.status, 418);
        assert_eq!(err.code, "TEAPOT");
        assert_eq!(err.message, "short and stout");
        assert!(err.details.is_none());
    }

    #[test]
    fn bad_request_status_and_code() {
        let err = ApiError::bad_request("nope");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, code::BAD_REQUEST);
        assert_eq!(err.message, "nope");
    }

    #[test]
    fn not_found_status_and_code() {
        let err = ApiError::not_found("gone");
        assert_eq!(err.status, 404);
        assert_eq!(err.code, code::NOT_FOUND);
    }

    #[test]
    fn area_too_large_status_and_code() {
        let err = ApiError::area_too_large("too big");
        assert_eq!(err.status, 413);
        assert_eq!(err.code, code::AREA_TOO_LARGE);
    }

    #[test]
    fn busy_status_and_code() {
        let err = ApiError::busy("later");
        assert_eq!(err.status, 503);
        assert_eq!(err.code, code::BUSY);
    }

    #[test]
    fn upstream_unavailable_status_and_code() {
        let err = ApiError::upstream_unavailable("down");
        assert_eq!(err.status, 502);
        assert_eq!(err.code, code::UPSTREAM_UNAVAILABLE);
    }

    #[test]
    fn timeout_status_and_code() {
        let err = ApiError::timeout("slow");
        assert_eq!(err.status, 504);
        assert_eq!(err.code, code::TIMEOUT);
    }

    #[test]
    fn internal_status_and_code() {
        let err = ApiError::internal("boom");
        assert_eq!(err.status, 500);
        assert_eq!(err.code, code::INTERNAL);
    }

    #[test]
    fn code_constants_are_stable() {
        assert_eq!(code::BAD_REQUEST, "BAD_REQUEST");
        assert_eq!(code::NOT_FOUND, "NOT_FOUND");
        assert_eq!(code::AREA_TOO_LARGE, "AREA_TOO_LARGE");
        assert_eq!(code::BUSY, "BUSY");
        assert_eq!(code::UPSTREAM_UNAVAILABLE, "UPSTREAM_UNAVAILABLE");
        assert_eq!(code::UPSTREAM_ERROR, "UPSTREAM_ERROR");
        assert_eq!(code::TIMEOUT, "TIMEOUT");
        assert_eq!(code::INTERNAL, "INTERNAL");
    }

    #[test]
    fn with_details_sets_details() {
        let err = ApiError::internal("boom").with_details(serde_json::json!({ "trace": "abc" }));
        assert_eq!(err.details, Some(serde_json::json!({ "trace": "abc" })));
    }

    #[test]
    fn with_details_overwrites_previous() {
        let err = ApiError::internal("boom")
            .with_details(serde_json::json!(1))
            .with_details(serde_json::json!(2));
        assert_eq!(err.details, Some(serde_json::json!(2)));
    }

    #[test]
    fn envelope_carries_message() {
        let v = ApiError::not_found("no such tile").envelope();
        assert_eq!(v["error"]["code"], "NOT_FOUND");
        assert_eq!(v["error"]["message"], "no such tile");
    }

    #[test]
    fn envelope_only_has_error_key() {
        let v = ApiError::busy("queue full").envelope();
        let obj = v.as_object().expect("envelope is an object");
        assert_eq!(obj.len(), 1);
        assert!(obj.contains_key("error"));
    }

    #[test]
    fn status_never_serialized_even_with_details() {
        let v = ApiError::timeout("slow")
            .with_details(serde_json::json!({ "ms": 500 }))
            .envelope();
        assert!(v["error"].get("status").is_none());
    }

    #[test]
    fn display_format() {
        let err = ApiError::bad_request("missing bbox");
        assert_eq!(err.to_string(), "400 (BAD_REQUEST): missing bbox");
    }

    #[test]
    fn display_uses_custom_status_and_code() {
        let err = ApiError::new(418, "TEAPOT", "no coffee");
        assert_eq!(err.to_string(), "418 (TEAPOT): no coffee");
    }
}
