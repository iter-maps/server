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
        Self { code: code.into(), message: message.into(), details: None, status }
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
        assert!(v["error"].get("status").is_none(), "status is transport-level, not serialized");
    }

    #[test]
    fn details_omitted_when_absent() {
        let v = ApiError::bad_request("missing bbox").envelope();
        assert!(v["error"].get("details").is_none());
    }
}
