mod context;
mod fsx;
mod regions;
mod runner;
mod step;
mod steps;

use context::Context;
use step::Step;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    iter_core::telemetry::init("iter-pipeline");

    let ctx = Context::from_env()?;
    tracing::info!(
        data_dir = %ctx.data_dir.display(),
        region = %ctx.region.target,
        "data-prep pipeline starting"
    );

    let steps: Vec<Box<dyn Step>> = vec![
        Box::new(steps::osm::FetchOsm),
        Box::new(steps::osm_clip::ClipOsm),
        Box::new(steps::gtfs::FetchGtfs),
        Box::new(steps::build_config::WriteBuildConfig),
        Box::new(steps::graph::BuildGraph),
        Box::new(steps::overlay::BuildOverlays),
        Box::new(steps::civici::ExtractCivici),
        Box::new(steps::photon::BuildPhotonIndex),
        Box::new(steps::places::ExtractPlaces),
        Box::new(steps::tiles::BuildTiles),
        Box::new(steps::health::WriteHealth),
    ];

    runner::run_all(&ctx, &steps).await?;
    tracing::info!("pipeline complete");
    Ok(())
}
