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
            // The FL NeTEx auto-downloads from the Italian NAP (CCISS) public
            // endpoint — RAP Lazio L2 (the L1 superset). Override or set empty
            // (NETEX_URL=) to use a file placed at GATEWAY_NETEX_PATH instead.
            netex_url: Some(config::or(
                "NETEX_URL",
                "https://www.cciss.it/nap/mmtis/public/api/v1/download/blob/Asset/663391/checkedResource",
            ))
            .filter(|u| !u.is_empty()),
            // The FL feed is the Italian NeTEx-IT profile (ADR 0017).
            netex_profile: iter_region_drivers::DEFAULT_NETEX_PROFILE.to_string(),
            http: http.clone(),
        }),
        Box::new(jobs::rt_reliability::RtReliability::from_env(http)),
    ];

    tracing::info!(jobs = jobs.len(), "iter-worker starting");
    scheduler::run(jobs).await
}
