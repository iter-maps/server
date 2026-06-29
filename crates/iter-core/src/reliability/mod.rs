//! Shared reliability primitives: the pure rollup algebra (`rollup`) and the
//! read side of the on-disk archive (`store_read`). The worker owns the write
//! path on top of these; the gateway reads the cold Tier-2 tier through
//! `store_read`. Keeping these types here lets both tiers share the on-disk
//! layout and metric definitions without the gateway depending on the worker.

pub mod rollup;
pub mod store_read;
