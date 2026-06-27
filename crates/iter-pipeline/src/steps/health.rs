//! HEALTH — write `output/health.json` from artifact mtimes. Not idempotent by
//! design: it always re-runs (unless `SKIP_HEALTH`) so the document reflects
//! the freshest build.

use std::path::Path;

use async_trait::async_trait;
use iter_contracts::health::StaticHealth;
use iter_core::Status;

use crate::context::Context;
use crate::fsx;
use crate::step::Step;

pub struct WriteHealth;

#[async_trait]
impl Step for WriteHealth {
    fn name(&self) -> &'static str {
        "HEALTH"
    }

    async fn satisfied(&self, _ctx: &Context) -> bool {
        false
    }

    async fn run(&self, ctx: &Context) -> anyhow::Result<()> {
        let tiles_built_at = mtime_iso(&ctx.output("output/tiles/roma.pmtiles")).await;
        let gtfs_loaded =
            mtime_iso(&ctx.output("graph.obj")).await.unwrap_or_else(|| "unknown".to_string());

        let status = if tiles_built_at.is_some() && gtfs_loaded != "unknown" {
            Status::Ok
        } else {
            Status::Degraded
        };

        let health = StaticHealth {
            status,
            version: ctx.version.clone(),
            gtfs_loaded,
            tiles_built_at,
            bootstrapped_at: Some(jiff::Timestamp::now().to_string()),
        };

        let bytes = serde_json::to_vec_pretty(&health)?;
        fsx::write_atomic(&ctx.output("output/health.json"), &bytes).await?;
        Ok(())
    }
}

async fn mtime_iso(path: &Path) -> Option<String> {
    let modified = tokio::fs::metadata(path).await.ok()?.modified().ok()?;
    jiff::Timestamp::try_from(modified).ok().map(|t| t.to_string())
}
