//! Pipeline steps, in dependency order.
//!
//! The full idempotent sequence is fetch → clip → build → render → import →
//! write-health. Implemented: OSM (fetch), CLIP (osmium routing clip), GTFS
//! (feed fetch), BUILD_CONFIG (OTP inputs), GRAPH (OTP graph build), TILES
//! (planetiler render), HEALTH. The steps that shell out (CLIP, GRAPH, TILES)
//! run against tools in the data-prep image. The remaining ones (STYLES,
//! OVERLAY, CIVICI, PHOTON import, NeTEx) land next — see `docs/roadmap/`.

pub mod build_config;
pub mod graph;
pub mod gtfs;
pub mod health;
pub mod osm;
pub mod osm_clip;
pub mod tiles;
