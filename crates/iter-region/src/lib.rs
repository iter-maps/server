//! The region model. A region is a node in a tree of declarative profiles
//! (`regions/<path>/region.toml`); a deployment targets a node (e.g.
//! `italy/lazio/rome`) and [`resolve`] merges the chain root→leaf into one
//! effective [`Resolved`] config. Adding a region is config + data, no
//! recompile (ADR 0008). Data lives at the node matching its *service area*,
//! not its operator.

pub mod profile;
pub mod resolved;

pub use profile::{
    Civici, Extents, Feed, Geocoding, LiveTrains, Overlay, Profile, RealtimeChannel,
};
pub use resolved::{Resolved, resolve};
