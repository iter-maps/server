//! The pure rollup core now lives in `iter_core::reliability::rollup` so the
//! gateway can read the cold tier without depending on the worker (ADR 0024).
//! The worker re-exports the module here unchanged, so the write path and its
//! callers keep referring to `crate::reliability::rollup::*`.

pub use iter_core::reliability::rollup::*;
