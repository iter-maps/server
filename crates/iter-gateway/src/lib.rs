//! Iter Maps edge / BFF library: the router and its capability modules. The
//! binary (`main.rs`) is a thin wrapper over `router::build`, so the full
//! router is integration-testable via `tower`'s `oneshot`.

pub mod config;
pub mod glyphs;
pub mod health;
pub mod http;
pub mod manifest;
pub mod overlays;
pub mod proxy;
pub mod router;
pub mod sprite;
pub mod state;
pub mod styles;
pub mod tiles;
