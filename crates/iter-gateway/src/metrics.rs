//! Per-request metrics recording (ADR 0037 phase 2). A thin middleware times each
//! request and records the low-cardinality HTTP counters/histogram defined in the
//! [`iter_core::metrics`] catalog. The recording is **fail-soft**: it never
//! changes the response, and every `metrics` macro is a no-op when no recorder is
//! installed — so a request behaves identically whether or not metrics are on.
//!
//! Label cardinality is deliberately bounded (ADR 0024/0037): `method` is
//! normalized to the known HTTP verbs (any unrecognized method collapses to
//! `OTHER`), `status` is the numeric code. The raw request path, query string, and
//! any user value are NEVER used as a label.

use std::time::Instant;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use iter_core::metrics::{
    HTTP_REQUEST_DURATION_SECONDS, HTTP_REQUESTS_TOTAL, LABEL_METHOD, LABEL_STATUS,
};

/// Normalize an HTTP method to a bounded label value. The common verbs pass
/// through; anything else collapses to `OTHER`, so a hostile client can't inflate
/// the label set with arbitrary method strings.
fn method_label(method: &axum::http::Method) -> &'static str {
    use axum::http::Method;
    match *method {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        Method::PATCH => "PATCH",
        Method::HEAD => "HEAD",
        Method::OPTIONS => "OPTIONS",
        _ => "OTHER",
    }
}

/// Middleware: measure the request, run it, then record `http_requests_total`
/// (labeled `method`, numeric `status`) and `http_request_duration_seconds`
/// (labeled `method`). Fail-soft — the recording is a no-op without an installed
/// recorder and never touches the response, so it can't break or slow a request
/// beyond a couple of cheap counter updates.
pub async fn record(req: Request, next: Next) -> Response {
    let method = method_label(req.method());
    let start = Instant::now();
    let resp = next.run(req).await;
    let status = resp.status().as_u16();

    // Bounded labels only — never the path/query. `status` is the numeric code as a
    // string so it stays a small, well-known set.
    metrics::counter!(
        HTTP_REQUESTS_TOTAL,
        LABEL_METHOD => method,
        LABEL_STATUS => status.to_string(),
    )
    .increment(1);
    metrics::histogram!(HTTP_REQUEST_DURATION_SECONDS, LABEL_METHOD => method)
        .record(start.elapsed().as_secs_f64());

    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Method;

    #[test]
    fn known_methods_pass_through_unknown_collapses() {
        assert_eq!(method_label(&Method::GET), "GET");
        assert_eq!(method_label(&Method::POST), "POST");
        // A non-standard extension method is bounded to OTHER, never echoed raw.
        let custom = Method::from_bytes(b"WEIRD").unwrap();
        assert_eq!(method_label(&custom), "OTHER");
    }
}
