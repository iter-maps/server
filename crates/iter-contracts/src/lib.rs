//! The Iter Maps wire contract as Rust types. Field names and JSON shapes here
//! are load-bearing: the Android client greps literal tokens and renders by
//! them, so changing a name breaks the client.

pub mod geo;
pub mod health;
pub mod live_trains;
pub mod offline;
pub mod places;

pub use geo::BBox;
