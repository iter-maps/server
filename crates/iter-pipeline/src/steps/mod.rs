//! Pipeline steps.
//!
//! The full idempotent sequence is fetch → clip → build → render → import →
//! write-health. The engine-orchestration steps (GLYPHS, GTFS, OSM clip, TILES
//! via planetiler, STYLES, OVERLAY, CIVICI, PHOTON import, BUILD_CONFIG, GRAPH
//! via OTP) shell out to tools that live in the build image; they land next —
//! see `docs/roadmap/`. HEALTH is implemented and always runs last so the
//! gateway's `/health` reflects the freshest artifact mtimes.

pub mod health;
