//! The scheduled rollup job (ADR 0022). On each tick it folds the current and
//! previous service-date's Tier-0 events into Tier-1, then folds those into the
//! permanent Tier-2, and drops Tier-0 partitions past the retention window. The
//! folds are re-derivable and idempotent over a partition, so a missed or
//! repeated run only costs work, never correctness. Fail-soft throughout: a
//! per-date error is logged and the rest of the run proceeds.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use iter_core::config;

use crate::job::Job;
use crate::reliability::store::{Store, TIER0_RETAIN_DAYS};

pub struct ReliabilityRollup {
    pub reliability_dir: PathBuf,
}

impl ReliabilityRollup {
    pub fn new(reliability_dir: PathBuf) -> Self {
        Self { reliability_dir }
    }
}

#[async_trait]
impl Job for ReliabilityRollup {
    fn name(&self) -> &'static str {
        "reliability-rollup"
    }

    fn interval(&self) -> Duration {
        // Hourly by default — the fold re-reads the whole day's partition, so an
        // hourly cadence keeps Tier-1/2 fresh without per-event writes.
        Duration::from_secs(config::parse("RELIABILITY_ROLLUP_SECS", 3600))
    }

    async fn run(&self) -> anyhow::Result<()> {
        let store = Store::new(&self.reliability_dir);
        let today = rome_today();
        let yesterday = prev_ymd(&today).unwrap_or_else(|| today.clone());

        // Fold today and yesterday's Tier-0 into Tier-1 (yesterday catches late-
        // arriving updates and the day boundary). Each fold is independent and
        // fail-soft; folding the whole partition is idempotent.
        for date in [today.as_str(), yesterday.as_str()] {
            match store.fold_tier0_into_tier1(date) {
                Ok(n) => tracing::info!(date, aggregates = n, "rollup: tier-0 → tier-1"),
                Err(e) => tracing::warn!(date, error = %e, "rollup: tier-1 fold failed"),
            }
        }

        // Rebuild the permanent Tier-2 as a pure function of all retained Tier-1
        // partitions. Recomputing (not accumulating) keeps a repeated run from
        // double-counting a partition into the permanent tier.
        match store.rebuild_tier2() {
            Ok(n) => tracing::info!(tier2_keys = n, "rollup: rebuilt tier-2 from tier-1"),
            Err(e) => tracing::warn!(error = %e, "rollup: tier-2 rebuild failed"),
        }

        // Drop expired Tier-0 partitions (retention/GC).
        match store.expire_tier0(&today, TIER0_RETAIN_DAYS) {
            Ok(dropped) if !dropped.is_empty() => {
                tracing::info!(?dropped, "rollup: expired tier-0 partitions")
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "rollup: tier-0 expiry failed"),
        }

        // Log the permanent archive's read-side overview (keys + merged metrics).
        if let Ok((keys, r)) = store.tier2_overview() {
            tracing::info!(
                tier2_keys = keys,
                observations = r.count,
                p50_s = r.p50_s,
                on_time_rate = r.on_time_rate,
                "rollup: tier-2 overview"
            );
        }
        Ok(())
    }
}

/// Today's `YYYYMMDD` in Europe/Rome wall-clock; falls back to the UTC date if
/// the zone can't be loaded.
fn rome_today() -> String {
    match jiff::tz::TimeZone::get("Europe/Rome") {
        Ok(tz) => jiff::Timestamp::now()
            .to_zoned(tz)
            .strftime("%Y%m%d")
            .to_string(),
        Err(_) => jiff::Zoned::now().strftime("%Y%m%d").to_string(),
    }
}

/// The `YYYYMMDD` one day before `ymd`, via the civil-day helpers. `None` on a
/// malformed input.
fn prev_ymd(ymd: &str) -> Option<String> {
    use crate::reliability::rollup::{days_from_ymd, ymd_from_days};
    let days = days_from_ymd(ymd)?;
    Some(ymd_from_days(days - 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::rt_reliability::StopEvent;

    fn ev(date: &str) -> StopEvent {
        StopEvent {
            route_id: "MEA".into(),
            direction_id: 0,
            stop_id: "70001".into(),
            service_date: date.into(),
            delay_s: 30,
            feed_hour: 8,
        }
    }

    #[test]
    fn prev_ymd_crosses_month_and_year_boundaries() {
        assert_eq!(prev_ymd("20260629").as_deref(), Some("20260628"));
        assert_eq!(prev_ymd("20260601").as_deref(), Some("20260531"));
        assert_eq!(prev_ymd("20260101").as_deref(), Some("20251231"));
        assert_eq!(prev_ymd("garbage"), None);
    }

    #[tokio::test]
    async fn run_folds_present_partitions_and_is_fail_soft_on_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path());
        // Seed a partition for "today" as the rollup will compute it.
        let today = rome_today();
        store.append_tier0(&[ev(&today)]).unwrap();

        let job = ReliabilityRollup::new(tmp.path().to_path_buf());
        // A full run must not error even though yesterday's partition is absent.
        job.run().await.unwrap();

        let t2 = store.read_tier2().unwrap();
        assert_eq!(t2.len(), 1, "today's event rolled into one Tier-2 key");
    }
}
