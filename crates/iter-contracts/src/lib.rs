//! The Iter Maps wire contract as Rust types. Field names and JSON shapes here
//! are load-bearing: the Android client greps literal tokens and renders by
//! them, so changing a name breaks the client. Keep these verbatim with the
//! contract under `concept/02-api-contracts/`.

pub mod geo;
pub mod health;
pub mod live_trains;
pub mod offline;
pub mod places;

pub use geo::BBox;
