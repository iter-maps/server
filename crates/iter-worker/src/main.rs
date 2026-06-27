mod gtfs_rt;
mod job;
mod jobs;
mod netex;
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
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    let jobs: Vec<Box<dyn Job>> = vec![
        Box::new(jobs::fl_gtfs::FlGtfsBuild {
            netex_path,
            // The FL GTFS lands next to the other graph inputs (steps/gtfs.rs
            // skips netex feeds, leaving this slot for the worker).
            out_path: data_dir.join("graph/TRENITALIA-FL.gtfs.zip"),
            netex_url: config::opt("NETEX_URL"),
            http: http.clone(),
        }),
        Box::new(jobs::rt_reliability::RtReliability::from_env(http)),
    ];

    tracing::info!(jobs = jobs.len(), "iter-worker starting");
    scheduler::run(jobs).await
}
