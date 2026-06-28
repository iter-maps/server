//! Region/country drivers, one crate subdivided by region (ADR 0018). A tier's
//! generic algorithm dispatches through one of the four traits here to the
//! driver for the deployment's country/city. The traits and their value types
//! are tier-agnostic — they depend only on `iter-contracts` + external crates,
//! never on a tier crate — so gateway/pipeline/worker can all share them.
//!
//! [`traits`] owns the four traits + value types + generic fallbacks;
//! [`registry`] owns the four selectors; `italy/` is all of Italy. Adding a
//! region is a new folder + a registry arm; it touches no generic code and no
//! other region.

pub mod registry;
pub mod traits;

mod italy;

pub use registry::{
    DEFAULT_NETEX_PROFILE, address_normalizer, live_trains_provider, netex_profile, overlay_driver,
};
pub use traits::{
    AddressNormalizer, AgencyInfo, BoardKind, LineKind, LiveTrainsProvider, NetexProfile,
    Projection, TransitOverlayDriver,
};
