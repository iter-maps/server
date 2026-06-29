//! Pipeline steps, in dependency order.
//!
//! The full idempotent sequence is fetch → clip → build → render → import →
//! write-health. Implemented: OSM (fetch), CLIP (osmium routing clip), GTFS
//! (feed fetch), BUILD_CONFIG (OTP inputs), GRAPH (OTP graph build),
//! ROUTER_CONFIG (OTP GTFS-RT updaters), OVERLAY (pure-Rust transit overlays),
//! STYLES (MapLibre style render), CIVICI (Overture house numbers), PHOTON
//! (geocoding index import), PLACES (addressed POIs for correlation), TILES
//! (planetiler render), HEALTH. The steps that
//! shell out (CLIP, GRAPH, CIVICI, PHOTON, PLACES, TILES) run against tools in
//! the data-prep image. The remaining build-tier work (NeTEx) lands next — see
//! `docs/roadmap/`.

/// The overlay kinds the OVERLAY step actually builds. STYLES wires GeoJSON
/// sources only for these, so it never references a `.geojson` the pipeline
/// doesn't emit (which would 404 at the gateway). Shared so the two steps agree.
pub const IMPLEMENTED_OVERLAY_KINDS: &[&str] = &["transit-lines", "metro-stations"];

pub mod build_config;
pub mod civici;
pub mod graph;
pub mod gtfs;
pub mod health;
pub mod osm;
pub mod osm_clip;
pub mod overlay;
pub mod photon;
pub mod places;
pub mod router_config;
pub mod styles;
pub mod tiles;
