# Unified transit-overlay network model (PLANNED)

Generalize the Rome metro/tram overlay algorithms into one clean, multi-region,
LOD-tiered network model — one geometry per line, with modes, identity, and
routable flags.

- **Plugs into:** the pipeline overlay-generation step, extending the shipped
  Rome pass: widen the route-type filter to
  {metro, tram, light_rail, rail, regional_rail}, prefer `route_master` grouping,
  add per-country entity-resolution heuristics, emit an extended property model.
  Output is GeoJSON, consumed by scoped delivery.
- **Data deps:** OSM route relations at the overlay extent; per-region feeds to
  color/identify routable lines. Overlay-only lines (no routing feed) carry
  OSM-derived ids (`OSM:<network>:<ref>`).
- **Build order:** Phase 0 generalize the Rome pass ✅ (ADR 0029); Phase 1 ingest
  per-region feeds; Phase 2 LOD-tiled PMTiles delivery + widen the OSM extent to
  Europe. Cross-boundary line resolution is best-effort, never-fail.
- **Landed so far:** the metro-stations concourse is smoothed into an organic
  footprint (Chaikin corner-cutting + Visvalingam-Whyatt simplification, ADR
  0014); the `STYLES` render step landed (ADR 0025). **Phase 0 done (ADR 0029):**
  `transit-lines` is multi-region — the widened route set, `route_master`-preferred
  grouping with the `(network, mode, ref)` fallback, and additive `network` +
  `routable` identity props (`OSM:<network>:<ref>` for overlay-only lines), with
  Rome's nine-line output byte-stable. Still remaining: the LOD-tiled PMTiles
  delivery, the Europe-wide OSM widening, and corridor union (gated on a robust
  polygon buffer).

Decision: ADR 0018, ADR 0029
