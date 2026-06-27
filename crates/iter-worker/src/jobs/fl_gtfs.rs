//! FL regional-rail GTFS build. Trenitalia publishes no routable GTFS for the
//! FL lines, so the gateway/worker synthesizes one from the official NeTEx
//! (placed at `GATEWAY_NETEX_PATH`) and serves it for the OTP graph build. Runs
//! on startup and daily.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;

use crate::job::Job;

pub struct FlGtfsBuild {
    pub netex_path: PathBuf,
}

#[async_trait]
impl Job for FlGtfsBuild {
    fn name(&self) -> &'static str {
        "fl-gtfs"
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(24 * 60 * 60)
    }

    async fn run(&self) -> anyhow::Result<()> {
        if !tokio::fs::try_exists(&self.netex_path)
            .await
            .unwrap_or(false)
        {
            tracing::debug!(
                source = %self.netex_path.display(),
                "fl-gtfs: no NeTEx source present; nothing to build"
            );
            return Ok(());
        }

        // The NeTEx -> GTFS conversion (SAX parse, line/stop/journey extraction,
        // calendar re-anchor, shape stitching) lands next; see docs/roadmap.
        anyhow::bail!(
            "FL NeTEx->GTFS conversion not yet implemented (source present at {}; see docs/roadmap)",
            self.netex_path.display()
        );
    }
}
