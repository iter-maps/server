# Unified transit-overlay network model (PLANNED)

Generalize the Rome metro/tram overlay algorithms into one clean, multi-region,
LOD-tiered network model — one geometry per line, with modes, identity, and
routable flags.

- **Plugs into:** the pipeline overlay-generation step, extending the shipped
  Rome pass (doc 09): widen the route-type filter to
  {metro, tram, light_rail, rail, regional_rail}, prefer `route_master` grouping,
  add per-country entity-resolution heuristics, emit an extended property model.
  Output is GeoJSON, consumed by scoped delivery (doc 28).
- **Data deps:** OSM route relations at the overlay extent; per-region feeds to
  color/identify routable lines. Overlay-only lines (no routing feed) carry
  OSM-derived ids (`OSM:<network>:<ref>`).
- **Build order:** Phase 0 generalize the Rome pass; Phase 1 ingest per-region
  feeds; Phase 2 widen OSM extent to Europe. Cross-boundary line resolution is
  best-effort, never-fail.

Design: concept doc 26 — transit-overlay-network-model ·
Decision: ADR 0018
