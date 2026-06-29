# Scoped / LOD overlay delivery (PLANNED)

The delivery layer for the unified overlay network: scope overlay data by
viewport+zoom (blue lines at continent zoom → full per-line colour + station
cutouts at city zoom) without downloading a nationwide blob.

- **Plugs into:** one new pipeline build step — run tippecanoe/planetiler on the
  per-tier GeoJSON the overlay model already produces → PMTiles vector
  tiles with three zoom-gated LOD layers (network z3–7, lines z8–14,
  stations z14+). Build-only; no runtime Rust change.
- **Data deps:** the overlay-model GeoJSON features. LOD is baked at build time
  (source-layer minzoom/maxzoom), not a style toggle. Offline reuses the
  existing PMTiles `extract` path (same as basemap).
- **Build order:** Phase 0 Rome (prove the path, keep whole-file GeoJSON as
  fallback); Phase 1 widen to all-Italy (tiles scale gracefully, same tooling).

Decision: ADR 0020
