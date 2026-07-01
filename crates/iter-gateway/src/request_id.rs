//! Request correlation (ADR 0037). Per request the gateway reads an inbound
//! `x-request-id` (or the trace-id of a W3C `traceparent`) when present and
//! valid, else mints a fresh short id. The id is recorded on the request's
//! tracing span as `request_id` — so every log line during the request carries
//! it — echoed on the response `x-request-id` header, and stashed in a request
//! extension so the proxy can forward it to the engines.
//!
//! Fail-soft by construction: a missing, malformed, or oversized inbound id is
//! never rejected — it just gets replaced by a minted one. Minting uses no
//! network and no heavy dependency: a monotonic counter mixed with the process
//! start nanoseconds, rendered as hex. Uniqueness only needs to hold within one
//! operator's log stream for correlation, not to be a cryptographic token.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::Request;
use axum::http::HeaderValue;
use axum::http::header::HeaderName;
use axum::middleware::Next;
use axum::response::Response;

/// The correlation header, both inbound and on the echoed response.
pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// Longest inbound id we echo verbatim; longer values are treated as absent and
/// a fresh id is minted (keeps the label bounded and low-cardinality-friendly).
const MAX_ID_LEN: usize = 128;

/// A minted id carried through the request as an extension so the proxy can put
/// it on outbound engine calls.
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

/// A monotonically increasing counter seeded from the process start time, so ids
/// minted across restarts don't collide within one log stream.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Mint a short, stable hex id. Not cryptographic — a monotonic counter mixed
/// with the process start nanos, enough to correlate lines within one instance.
fn mint() -> String {
    static SEED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let seed = *SEED.get_or_init(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    });
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    // Mix the counter into the seed's high bits with a cheap xorshift-style
    // scramble so successive ids don't look sequential.
    let mut x = seed ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 33;
    format!("{x:016x}")
}

/// Extract a usable correlation id from the request headers, or `None` to mint
/// one. Accepts a valid `x-request-id`, else the trace-id of a W3C
/// `traceparent`. "Valid" = ASCII, non-empty, within [`MAX_ID_LEN`], and only
/// id-safe characters (alphanumeric, `-`, `_`); anything else is ignored.
fn inbound_id(headers: &axum::http::HeaderMap) -> Option<String> {
    if let Some(id) = headers
        .get(&REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| is_valid_id(s))
    {
        return Some(id.to_owned());
    }
    headers
        .get("traceparent")
        .and_then(|v| v.to_str().ok())
        .and_then(trace_id_from_traceparent)
}

/// True when `s` is a safe, bounded correlation id.
fn is_valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_ID_LEN
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Pull the 32-hex trace-id (second field) out of a W3C `traceparent`:
/// `version-traceid-parentid-flags`. Returns it only when well-formed and not
/// the all-zero invalid trace-id.
fn trace_id_from_traceparent(tp: &str) -> Option<String> {
    let trace = tp.split('-').nth(1)?;
    if trace.len() == 32
        && trace.bytes().all(|b| b.is_ascii_hexdigit())
        && trace.bytes().any(|b| b != b'0')
    {
        Some(trace.to_ascii_lowercase())
    } else {
        None
    }
}

/// Middleware: resolve the request id (accept-or-mint), record it on the current
/// span as `request_id`, run the handler, and set `x-request-id` on the response.
/// The id is also stashed as a [`RequestId`] extension for downstream forwarding.
pub async fn propagate(mut req: Request, next: Next) -> Response {
    let id = inbound_id(req.headers()).unwrap_or_else(mint);

    // Record on the current span so every line during this request carries it.
    tracing::Span::current().record("request_id", id.as_str());
    req.extensions_mut().insert(RequestId(id.clone()));

    let mut resp = next.run(req).await;
    if let Ok(value) = HeaderValue::from_str(&id) {
        resp.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    #[test]
    fn minted_ids_are_hex_and_distinct() {
        let a = mint();
        let b = mint();
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "successive ids must differ");
    }

    #[test]
    fn accepts_a_valid_inbound_x_request_id() {
        let mut h = HeaderMap::new();
        h.insert(&REQUEST_ID_HEADER, HeaderValue::from_static("abc-123_XY"));
        assert_eq!(inbound_id(&h).as_deref(), Some("abc-123_XY"));
    }

    #[test]
    fn rejects_empty_oversized_or_unsafe_inbound_id() {
        for bad in ["", "has space", "semi;colon", "slash/here"] {
            let mut h = HeaderMap::new();
            h.insert(&REQUEST_ID_HEADER, HeaderValue::from_str(bad).unwrap());
            assert_eq!(inbound_id(&h), None, "should reject {bad:?}");
        }
        let mut h = HeaderMap::new();
        let long = "a".repeat(MAX_ID_LEN + 1);
        h.insert(&REQUEST_ID_HEADER, HeaderValue::from_str(&long).unwrap());
        assert_eq!(inbound_id(&h), None, "should reject oversized id");
    }

    #[test]
    fn falls_back_to_traceparent_trace_id() {
        let mut h = HeaderMap::new();
        h.insert(
            "traceparent",
            HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
        );
        assert_eq!(
            inbound_id(&h).as_deref(),
            Some("4bf92f3577b34da6a3ce929d0e0e4736")
        );
    }

    #[test]
    fn ignores_malformed_or_zero_traceparent() {
        for bad in [
            "00-notlongenough-x-01",
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01", // all-zero
            "garbage",
        ] {
            let mut h = HeaderMap::new();
            h.insert("traceparent", HeaderValue::from_str(bad).unwrap());
            assert_eq!(inbound_id(&h), None, "should ignore {bad:?}");
        }
    }

    #[test]
    fn x_request_id_wins_over_traceparent() {
        let mut h = HeaderMap::new();
        h.insert(&REQUEST_ID_HEADER, HeaderValue::from_static("chosen"));
        h.insert(
            "traceparent",
            HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
        );
        assert_eq!(inbound_id(&h).as_deref(), Some("chosen"));
    }
}
