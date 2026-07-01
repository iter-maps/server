//! Operator-local observability: structured logs to stdout, never phoning
//! home (ADR 0037; posture in `docs/TELEMETRY.md`, ADR 0024). `ITER_LOG` sets
//! the filter, `ITER_LOG_FORMAT=json` switches to JSON.
//!
//! # Log field/label schema (ADR 0037)
//!
//! Logs are structured for Loki: a small set of **low-cardinality labels** you
//! can index and group by, plus higher-cardinality **context fields** you read
//! but don't index. Keep new logs on this vocabulary so a query is the same
//! shape everywhere.
//!
//! ## Labels (low cardinality — safe to index/group by)
//!
//! - `service` — the binary: `iter-gateway` | `iter-pipeline` | `iter-worker`.
//!   Set once here in the event formatter, so every line carries it regardless
//!   of the emitting thread.
//! - `event` — the log category, dotted: `gateway.request`, `proxy.upstream`,
//!   `worker.job`, `pipeline.step`. Replaces free-text prose prefixes.
//! - `outcome` — `ok` | `fail` | `hit` | `miss`.
//! - `error.code` — the stable [`crate::error::code`] value (`UPSTREAM_ERROR`,
//!   `TIMEOUT`, …) on a failure line.
//! - `upstream` — the external engine: `otp` | `photon` | `viaggiatreno`.
//!
//! ## Context fields (higher cardinality — read, don't index)
//!
//! `latency_ms`, `route`, `status`, `request_id`, `job`, `step`, `feed`,
//! `count`.
//!
//! Correlation: the gateway mints/accepts an `x-request-id` per request
//! (accepting a W3C `traceparent` trace-id), records it as `request_id` on the
//! request span so every line during that request carries it, echoes it on the
//! response, and propagates it to the engines (ADR 0037).
//!
//! Metrics ([`crate::metrics`], ADR 0037 phase 2) are the sibling concern: [`init`]
//! installs the process-wide Prometheus recorder so the gateway can serve an
//! **internal** `/metrics` endpoint. Same posture as the logs — operator-local,
//! never phone-home — and fail-soft: a lost install race logs and continues.

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::{Format, Json, JsonFields, Writer};
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

/// Wraps the built-in pretty event formatter and prefixes each line with the
/// process `service` label from one choke-point, so every pretty line carries
/// `service=<name>` without each call site repeating it (ADR 0037). The JSON
/// path uses [`WithServiceJson`] for the same guarantee.
struct WithService<F> {
    service: &'static str,
    inner: F,
}

impl<S, N, F> FormatEvent<S, N> for WithService<F>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
    F: FormatEvent<S, N>,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        write!(writer, "service={} ", self.service)?;
        self.inner.format_event(ctx, writer, event)
    }
}

/// Wraps the built-in JSON event formatter and splices a constant `service`
/// field into every emitted object, so JSON lines carry `service` regardless of
/// which thread emitted them (ADR 0037).
///
/// The old approach — enter a process-lifetime `service` root span and rely on
/// its `service` field — only works on the thread that entered it: `entered()`
/// pushes onto that thread's local dispatch stack. The gateway runs on a
/// multi-threaded tokio runtime and request-scoped events fire on worker
/// threads, which never inherit the main thread's span, so those lines carried
/// no `service`. Injecting the field in the formatter is thread-independent.
struct WithServiceJson {
    /// The pre-rendered, JSON-escaped `"service":"<name>"` field, spliced verbatim.
    field: &'static str,
    inner: Format<Json>,
}

impl WithServiceJson {
    fn new(service: &str, inner: Format<Json>) -> Self {
        // Escape once via serde so an odd service name can't produce invalid JSON,
        // then leak the tiny string for the `'static` formatter (init runs once).
        let name = serde_json::to_string(service).unwrap_or_else(|_| "\"\"".to_owned());
        let field = format!("\"service\":{name}");
        Self {
            field: Box::leak(field.into_boxed_str()),
            inner,
        }
    }
}

impl<S, N> FormatEvent<S, N> for WithServiceJson
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        // Render the inner JSON object to a scratch buffer, then insert the
        // `service` field right after the opening `{`. `Format<Json>` always emits
        // a single object beginning with `{`; the trailing newline stays on the
        // spliced line. Fail-soft: if the shape is ever unexpected, emit the object
        // unchanged rather than break logging.
        let mut buf = String::new();
        self.inner.format_event(ctx, Writer::new(&mut buf), event)?;
        match buf.strip_prefix('{') {
            Some(rest) => {
                let sep = if rest.trim_start().starts_with('}') {
                    ""
                } else {
                    ","
                };
                write!(writer, "{{{}{sep}{rest}", self.field)
            }
            None => writer.write_str(&buf),
        }
    }
}

pub fn init(service: &str) {
    // Leak the name once so it can live in the `'static` formatter/span; a binary
    // calls `init` exactly once at startup, so this is a fixed, tiny cost.
    let service: &'static str = Box::leak(service.to_owned().into_boxed_str());

    let filter = EnvFilter::try_from_env("ITER_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let json = std::env::var("ITER_LOG_FORMAT").as_deref() == Ok("json");

    if json {
        // JSON: splice a constant `service` field into every object from one
        // formatter, so every line carries it independent of the emitting thread.
        let fmt = WithServiceJson::new(service, Format::default().json());
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(true)
            .fmt_fields(JsonFields::new())
            .event_format(fmt)
            .init();
    } else {
        // Pretty: prefix every line with the service label from one formatter.
        let fmt = WithService {
            service,
            inner: Format::default(),
        };
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(true)
            .event_format(fmt)
            .init();
    }

    tracing::info!(event = "service.start", service, "logging initialized");

    // Install the Prometheus recorder so the gateway can serve an internal
    // `/metrics` endpoint (ADR 0037 phase 2). Idempotent + fail-soft: a lost race
    // logs and continues, and the metric macros are no-ops until a recorder exists.
    crate::metrics::install_recorder();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    /// A `MakeWriter` collecting emitted bytes into a shared buffer.
    #[derive(Clone)]
    struct BufWriter(Arc<Mutex<Vec<u8>>>);

    impl<'a> MakeWriter<'a> for BufWriter {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    impl Write for BufWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn pretty_formatter_prefixes_every_line_with_the_service_label() {
        // The pretty path prefixes each rendered line via `WithService`, so any
        // emitted event carries `service=<name>` from one choke-point.
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sub = tracing_subscriber::fmt()
            .event_format(WithService {
                service: "iter-gateway",
                inner: Format::default(),
            })
            .with_writer(BufWriter(buf.clone()))
            .finish();
        tracing::subscriber::with_default(sub, || {
            tracing::info!(event = "unit.test", "hello");
        });
        let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            out.contains("service=iter-gateway"),
            "line should carry the service label: {out}"
        );
    }

    /// Build a subscriber wiring the `WithServiceJson` formatter to a buffer, the
    /// way [`init`] does for the JSON path (so the test covers the real mechanism).
    fn json_service_subscriber(
        service: &'static str,
        buf: Arc<Mutex<Vec<u8>>>,
    ) -> impl tracing::Subscriber + Send + Sync {
        tracing_subscriber::fmt()
            .fmt_fields(JsonFields::new())
            .event_format(WithServiceJson::new(service, Format::default().json()))
            .with_writer(BufWriter(buf))
            .finish()
    }

    #[test]
    fn json_lines_carry_service_as_a_field() {
        // The JSON path splices `service` into the object from the formatter, so
        // the field is present without an entered span and the output stays valid
        // JSON.
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sub = json_service_subscriber("iter-worker", buf.clone());
        tracing::subscriber::with_default(sub, || {
            tracing::info!(event = "unit.test", "hello");
        });
        let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        let line = out.lines().next().expect("one json line");
        let v: serde_json::Value = serde_json::from_str(line).expect("valid JSON line");
        assert_eq!(v["service"], "iter-worker", "service field: {line}");
    }

    #[test]
    fn json_service_is_present_on_a_tokio_worker_thread() {
        // The real deployment topology: a multi-thread runtime emitting from a
        // spawned task on a worker thread. The old entered-root-span approach lost
        // `service` here (the span lives on another thread); the formatter-spliced
        // field survives the thread hop.
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sub = std::sync::Arc::new(json_service_subscriber("iter-gateway", buf.clone()));
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .build()
            .unwrap();
        rt.block_on(async move {
            // Install the subscriber inside the spawned task, i.e. on a worker
            // thread, so the emit happens off the main thread. A thread-local
            // entered span would be lost here; the formatter-spliced field is not.
            tokio::spawn(async move {
                tracing::subscriber::with_default(sub, || {
                    tracing::info!(event = "unit.test", "from worker");
                });
            })
            .await
            .unwrap();
        });
        let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        let line = out
            .lines()
            .find(|l| l.contains("from worker"))
            .unwrap_or_else(|| panic!("expected the worker-thread line: {out}"));
        let v: serde_json::Value = serde_json::from_str(line).expect("valid JSON line");
        assert_eq!(
            v["service"], "iter-gateway",
            "service on worker line: {line}"
        );
    }
}
