//! The persistent reliability rollup tier (ADR 0022). Derived GTFS-RT stop-
//! events are folded into mergeable Tier-0/1/2 aggregates keyed only on the
//! stable (route, direction, stop) tuple plus time-of-day + day-type buckets —
//! never on a trip/user/device/session id. `rollup` is the pure, I/O-free core;
//! `store` is the filesystem adapter.

pub mod rollup;
pub mod store;
