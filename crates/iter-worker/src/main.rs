mod gtfs_rt;
mod job;
mod jobs;
mod netex;
mod reliability;
mod scheduler;

use std::path::PathBuf;

use anyhow::Context as _;
use iter_core::config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    iter_core::telemetry::init("iter-worker");

    let data_dir = PathBuf::from(config::or("DATA_DIR", "/data"));

    // Resolve the region like the gateway and pipeline do, so the worker's jobs
    // and their source URLs come from one source of truth (ADR 0008 / 0019).
    let regions_dir = PathBuf::from(config::or("REGIONS_DIR", "regions"));
    let target = config::or("ITER_REGION", "italy/lazio/rome");
    let region = iter_region::resolve(&regions_dir, &target)
        .with_context(|| format!("resolving region '{target}'"))?;

    let http = reqwest::Client::builder()
        .user_agent(concat!("iter-worker/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    let jobs = jobs::from_region(&region, &data_dir, &http);

    tracing::info!(jobs = jobs.len(), region = %target, "iter-worker starting");
    scheduler::run(jobs).await
}
