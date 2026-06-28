//! Background jobs, derived from the resolved region's feeds (ADR 0019).
//!
//! `fl_gtfs` (FL NeTEx→GTFS) and `rt_reliability` (GTFS-RT ingestion) are wired;
//! the reliability rollup tier and daily graph-refresh trigger are tracked in
//! `docs/roadmap/`.

pub mod fl_gtfs;
pub mod rt_reliability;

use std::path::{Path, PathBuf};

use iter_core::config;
use iter_region::Resolved;

use crate::job::Job;

/// Derive the worker's job set from a resolved region: one NeTEx→GTFS job per
/// `source="netex"` feed (using its `url`), one RT-reliability job per feed
/// declaring a `trip-updates` channel (using that channel's URL). Env overrides
/// still win inside each job.
pub fn from_region(
    region: &Resolved,
    data_dir: &Path,
    http: &reqwest::Client,
) -> Vec<Box<dyn Job>> {
    let mut jobs: Vec<Box<dyn Job>> = Vec::new();

    for feed in region.enabled_feeds() {
        if feed.source.as_deref() == Some("netex") {
            // GTFS lands next to the other graph inputs; steps/gtfs.rs skips
            // netex feeds so they don't collide here.
            let netex_path = config::opt("GATEWAY_NETEX_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    data_dir.join(format!("netex/{}.netex.xml.gz", feed.id.to_lowercase()))
                });
            jobs.push(Box::new(fl_gtfs::FlGtfsBuild {
                netex_path,
                out_path: data_dir.join(format!("graph/{}.gtfs.zip", feed.id)),
                // `NETEX_URL=` (empty) forces using a file at GATEWAY_NETEX_PATH.
                netex_url: feed
                    .url
                    .clone()
                    .map(|u| config::or("NETEX_URL", &u))
                    .filter(|u| !u.is_empty()),
                // The FL feed is the Italian NeTEx-IT profile (ADR 0017).
                netex_profile: iter_region_drivers::DEFAULT_NETEX_PROFILE.to_string(),
                http: http.clone(),
            }));
        }

        if let Some(url) = feed.realtime_url("trip-updates") {
            jobs.push(Box::new(rt_reliability::RtReliability::new(
                url.to_string(),
                http.clone(),
            )));
        }
    }

    jobs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn region(regions_dir: &Path) -> Resolved {
        iter_region::resolve(regions_dir, "italy/lazio/rome").unwrap()
    }

    #[test]
    fn derives_one_fl_and_one_rt_job_from_the_committed_region() {
        let regions = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../regions");
        let r = region(&regions);
        let jobs = from_region(&r, Path::new("/data"), &reqwest::Client::new());

        let names: Vec<&str> = jobs.iter().map(|j| j.name()).collect();
        // TRENITALIA-FL is the lone netex feed; ATAC the lone trip-updates feed.
        assert_eq!(names.iter().filter(|n| **n == "fl-gtfs").count(), 1);
        assert_eq!(names.iter().filter(|n| **n == "rt-reliability").count(), 1);
    }
}
