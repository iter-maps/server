mod gtfs_rt;
mod job;
mod jobs;
mod scheduler;

use std::path::PathBuf;

use iter_core::config;
use job::Job;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    iter_core::telemetry::init("iter-worker");

    let data_dir = PathBuf::from(config::or("DATA_DIR", "/data"));
    let netex_path = config::opt("GATEWAY_NETEX_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join("netex/trenitalia-fl.netex.xml.gz"));

    let http = reqwest::Client::builder()
        .user_agent(concat!("iter-worker/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let jobs: Vec<Box<dyn Job>> = vec![
        Box::new(jobs::fl_gtfs::FlGtfsBuild { netex_path }),
        Box::new(jobs::rt_reliability::RtReliability::from_env(http)),
    ];

    tracing::info!(jobs = jobs.len(), "iter-worker starting");
    scheduler::run(jobs).await
}
