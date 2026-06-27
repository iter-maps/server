//! Background jobs.
//!
//! `fl_gtfs` (FL NeTEx→GTFS) and `rt_reliability` (GTFS-RT ingestion) are wired.
//! The reliability rollup tier (persistent Tier-0/1/2 archives) and the daily
//! graph-refresh trigger land next — see `docs/roadmap/`.

pub mod fl_gtfs;
pub mod rt_reliability;
