//! Operator-local observability only: structured logs to stdout. This never
//! phones home (telemetry policy concern (b): operator observability stays
//! local). `ITER_LOG` sets the filter, `ITER_LOG_FORMAT=json` switches to JSON.

use tracing_subscriber::EnvFilter;

pub fn init(service: &str) {
    let filter = EnvFilter::try_from_env("ITER_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let json = std::env::var("ITER_LOG_FORMAT").as_deref() == Ok("json");
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true);

    if json {
        builder.json().init();
    } else {
        builder.init();
    }

    tracing::info!(service, "logging initialized");
}
