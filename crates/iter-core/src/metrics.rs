//! Operator-local metrics: the Prometheus recorder + the metric catalog (ADR
//! 0037, phase 2). Metrics stay LOCAL to the operator's host and NEVER phone home
//! — same posture as the logs (ADR 0024, `docs/TELEMETRY.md`). The recorder feeds
//! an internal `/metrics` endpoint on the gateway; there is no central collector.
//!
//! # Design
//!
//! - The recorder is installed once at startup via [`install_recorder`] (called
//!   from [`crate::telemetry::init`]). It is idempotent and **fail-soft**: a lost
//!   install race logs and continues rather than aborting the process, and every
//!   `metrics::counter!`/`histogram!` call is a harmless no-op when no recorder is
//!   installed (a unit test, or a binary that never called `init`).
//! - The rendered exposition is served by whoever holds the [`PrometheusHandle`]
//!   ([`prometheus_handle`] returns it). Recording anywhere in the workspace goes
//!   through the global recorder, so a call site just uses the `metrics` macros
//!   with the names/labels below — no handle needed.
//!
//! # Metric catalog (low-cardinality labels ONLY)
//!
//! Labels are a **bounded** set — never the raw request path, query, coordinates,
//! or any user value (that would blow up cardinality and leak user data, which
//! ADR 0024/0037 forbid). The catalog is also mirrored in `docs/TELEMETRY.md`.
//!
//! | Metric | Type | Labels | Meaning |
//! |---|---|---|---|
//! | [`HTTP_REQUESTS_TOTAL`] | counter | `method`, `status` | one per served request; `status` is the numeric HTTP code |
//! | [`HTTP_REQUEST_DURATION_SECONDS`] | histogram | `method` | per-request wall latency the request path already measured |
//! | [`UPSTREAM_ERRORS_TOTAL`] | counter | `upstream`, `code` | a proxy upstream failure; `upstream=otp\|photon\|viaggiatreno`, `code` = [`crate::error::code`] |
//! | [`WEATHER_CACHE_LOOKUPS_TOTAL`] | counter | `outcome` (`hit\|miss`) | weather-forecast cache lookups on the rerank path |

use std::sync::OnceLock;

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

// --- metric names + label keys (one place, so call sites stay consistent) ----

/// Counter: one increment per served HTTP request, labeled by `method` and the
/// numeric `status` code. Bounded: `method` is the small set of HTTP verbs and
/// `status` the small set of codes the gateway returns — never the path/query.
pub const HTTP_REQUESTS_TOTAL: &str = "http_requests_total";

/// Histogram: per-request wall latency in seconds, labeled by `method`. Uses the
/// same latency the request path already computes for its outcome log line.
pub const HTTP_REQUEST_DURATION_SECONDS: &str = "http_request_duration_seconds";

/// Counter: one increment per proxy upstream failure, labeled by `upstream`
/// (`otp|photon|viaggiatreno`) and `code` (the stable [`crate::error::code`]).
pub const UPSTREAM_ERRORS_TOTAL: &str = "upstream_errors_total";

/// Counter: weather-forecast cache lookups on the rerank path, labeled by
/// `outcome` (`hit` served from cache, `miss` fetched upstream).
pub const WEATHER_CACHE_LOOKUPS_TOTAL: &str = "weather_cache_lookups_total";

/// Label key: HTTP method (`GET`, `POST`, …).
pub const LABEL_METHOD: &str = "method";
/// Label key: numeric HTTP status code, as a string.
pub const LABEL_STATUS: &str = "status";
/// Label key: the external engine (`otp|photon|viaggiatreno`).
pub const LABEL_UPSTREAM: &str = "upstream";
/// Label key: the stable [`crate::error::code`] on an upstream failure.
pub const LABEL_CODE: &str = "code";
/// Label key: a lookup outcome (`hit|miss`).
pub const LABEL_OUTCOME: &str = "outcome";

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Install the process-wide Prometheus recorder and stash its handle, once. Later
/// calls are no-ops that return the already-installed handle. **Fail-soft:** if the
/// global recorder was already claimed (a lost install race, or another test
/// installed one), this logs at DEBUG and returns `None` rather than aborting
/// startup — the metric macros then feed whatever recorder won the race, and the
/// gateway simply has no handle to render (its `/metrics` reports so). Called from
/// [`crate::telemetry::init`]; separated out so it is unit-testable on its own.
pub fn install_recorder() -> Option<PrometheusHandle> {
    if let Some(handle) = HANDLE.get() {
        return Some(handle.clone());
    }
    match PrometheusBuilder::new().install_recorder() {
        Ok(handle) => {
            // First installer wins the `OnceLock`; a racing caller that lost the
            // `install_recorder` race is handled by the `Err` arm below.
            let _ = HANDLE.set(handle.clone());
            Some(handle)
        }
        Err(e) => {
            // Another recorder is already installed globally (e.g. a prior test in
            // the same process). Metrics still record into it; we just have no
            // handle of our own to render from. Never abort startup for this.
            tracing::debug!(
                event = "service.metrics",
                outcome = "skip",
                cause = %e,
                "prometheus recorder already installed; continuing without a handle"
            );
            None
        }
    }
}

/// The installed Prometheus handle, if [`install_recorder`] ran and won the race.
/// The gateway's `/metrics` endpoint calls this to render the exposition; `None`
/// means no handle is available (metrics were never installed, or a race lost),
/// in which case the endpoint reports that rather than panicking.
pub fn prometheus_handle() -> Option<PrometheusHandle> {
    HANDLE.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_installs_once_and_renders() {
        // A record before any recorder exists is a harmless no-op (it must not
        // panic and, being dropped, must not appear once we install below).
        metrics::counter!(HTTP_REQUESTS_TOTAL, LABEL_METHOD => "GET", LABEL_STATUS => "200")
            .increment(1);
        let first = install_recorder();
        // In isolation this process has no other recorder, so the install wins.
        assert!(
            first.is_some(),
            "recorder should install in a fresh process"
        );
        // A record AFTER install is captured and renders as real Prometheus text.
        metrics::counter!(WEATHER_CACHE_LOOKUPS_TOTAL, LABEL_OUTCOME => "hit").increment(1);
        let handle = prometheus_handle().expect("handle available after install");
        let text = handle.render();
        assert!(
            text.contains(WEATHER_CACHE_LOOKUPS_TOTAL),
            "a counter recorded after install should render: {text}"
        );
        assert!(
            text.contains("outcome=\"hit\""),
            "the bounded outcome label should render: {text}"
        );
        // Idempotent: a second install returns Some (the same handle), never panics.
        assert!(install_recorder().is_some(), "second install is idempotent");
    }
}
