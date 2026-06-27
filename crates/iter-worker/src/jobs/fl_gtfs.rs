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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn skips_cleanly_when_no_source() {
        let dir = tempfile::tempdir().unwrap();
        let job = FlGtfsBuild {
            netex_path: dir.path().join("absent.netex.xml.gz"),
        };
        assert!(job.run().await.is_ok());
    }

    #[tokio::test]
    async fn errors_when_source_present_until_converter_lands() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("trenitalia-fl.netex.xml.gz");
        std::fs::write(&src, b"<PublicationDelivery/>").unwrap();
        let job = FlGtfsBuild { netex_path: src };
        assert!(job.run().await.is_err());
    }

    #[test]
    fn schedule_metadata() {
        let job = FlGtfsBuild {
            netex_path: PathBuf::from("/x"),
        };
        assert_eq!(job.name(), "fl-gtfs");
        assert_eq!(job.interval().as_secs(), 24 * 60 * 60);
        assert!(job.run_on_start());
    }
}
