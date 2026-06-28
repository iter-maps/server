//! Region/country drivers, consolidated into one crate, subdivided by region
//! (ADR 0018; supersedes 0017's per-tier *placement*). When a tier surface needs
//! genuinely custom code — not just config — its generic algorithm dispatches
//! through one of the four traits here to the driver for the deployment's
//! country/city. The traits and their value types are **tier-agnostic**: they
//! depend only on `iter-contracts` + external crates, never on a tier crate, so
//! the gateway/pipeline/worker can all share them.
//!
//! Layout: [`traits`] owns the four traits + value types + the generic
//! fallbacks; [`registry`] owns the four selectors; `italy/` is *all* of Italy
//! (address, live-trains, transit-overlay, NeTEx-profile). Adding a region is a
//! new folder + a registry arm; adding a country touches no generic code and no
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
