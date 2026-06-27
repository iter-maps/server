//! Pipeline steps, in dependency order.
//!
//! The full idempotent sequence is fetch → clip → build → render → import →
//! write-health. Implemented: OSM (fetch), CLIP (osmium routing clip), GTFS
//! (feed fetch), BUILD_CONFIG (OTP inputs), GRAPH (OTP graph build), CIVICI
//! (Overture house numbers), PHOTON (geocoding index import), PLACES (addressed
//! POIs for correlation), TILES (planetiler render), HEALTH. The steps that
//! shell out (CLIP, GRAPH, CIVICI, PHOTON, PLACES, TILES) run against tools in
//! the data-prep image. The remaining ones (STYLES, OVERLAY, NeTEx) land next —
//! see `docs/roadmap/`.

pub mod build_config;
pub mod civici;
pub mod graph;
pub mod gtfs;
pub mod health;
pub mod osm;
pub mod osm_clip;
pub mod photon;
pub mod places;
pub mod tiles;
