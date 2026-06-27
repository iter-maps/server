//! Pipeline steps, in dependency order.
//!
//! The full idempotent sequence is fetch → clip → build → render → import →
//! write-health. Implemented: OSM (fetch), TILES (planetiler render), HEALTH.
//! The remaining engine-orchestration steps (GTFS fetch, OSM clip via osmium,
//! STYLES, OVERLAY, CIVICI, PHOTON import, BUILD_CONFIG, GRAPH via OTP) shell
//! out to tools in the data-prep image and land next — see `docs/roadmap/`.

pub mod health;
pub mod osm;
pub mod tiles;
