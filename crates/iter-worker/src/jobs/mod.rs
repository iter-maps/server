//! Background jobs.
//!
//! `fl_gtfs` is wired today. Planned jobs (see `docs/roadmap/`): GTFS-RT polling
//! into the historical-reliability rollups, and the daily transit-graph refresh
//! trigger.

pub mod fl_gtfs;
