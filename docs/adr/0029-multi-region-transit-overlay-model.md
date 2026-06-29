# 0029 — Multi-region transit-overlay model

- **Status:** Accepted
- **Date:** 2026-06-29
- **Supersedes:** —
- **Superseded by:** —

## Context

The pipeline's `transit-lines` overlay (ADR 0014, pure-Rust geometry) was built
against Rome/ATAC: it kept only `route=subway|tram` relations owned by the
driver's operator, grouped them by `(LineKind, ref)`, and emitted one
`MultiLineString` per line with a `kind`/`route`/`line`/`color` property set. The
geometry is generic; the operator specifics sit behind `TransitOverlayDriver`
(ADR 0018). That shape is correct for Rome but doesn't generalize: other regions
run light rail, regional rail, and heavy rail as part of their displayed network,
they group direction variants with OSM `route_master` relations, and a network's
lines aren't all backed by an in-scope timetable feed.

The overlay is **display-only**: the gateway serves `transit-lines.geojson` as a
static blob and the transit MapLibre styles (ADR 0025) wire it as a GeoJSON
source without reading per-feature properties in any layer expression. So the
overlay never feeds routing, and the only contract that matters is the property
set a client could read. Widening it must stay fail-soft and additive.

## Decision

We will generalize the `transit-lines` pass into a multi-region model, keeping
Rome's output identical:

- **Widened route set.** Keep a driver-owned route relation when
  `route ∈ {subway, tram, light_rail, rail, regional_rail}`. `subway` still
  counts only when the driver's allow-set promotes the ref to a metro line;
  everything else maps by name. The mode drives the GeoJSON `kind`
  (`metro`/`tram`/`light_rail`/`rail`/`regional_rail`).
- **`route_master`-preferred grouping.** A first pass maps each member route
  relation to its `route_master`; route relations sharing a master collapse to
  one line. With no master, the line falls back to grouping by
  `(network, mode, ref)` — the previous behaviour for a single-network region.
- **Network is `network` then `operator`.** Relations scope to the driver's owned
  network, matched on either the OSM `network` or `operator` tag; the recorded
  network is `network` if present, else `operator`.
- **Identity (additive props).** Each feature gains `network` (the OSM network)
  and `routable` (bool). A line that joins a routable feed keeps its
  feed/gtfsId-style id (`<route_id_prefix><gtfs route id>`) and `routable: true`;
  an overlay-only line — geometry but no in-scope timetable row — gets an
  OSM-derived id `OSM:<network>:<ref>` and `routable: false`. The existing
  `kind`/`route`/`line`/`color` properties are unchanged.
- **Invariants kept.** Per-line `MultiLineString` geometry, way-id-level shared
  dedup (a way carried by both directions counted once), metro-first ordering
  (the rail family joins the non-metro tail by numeric ref), metro colour from
  GTFS/driver, and null colour for every non-metro mode. The build never fails on
  an odd or partial relation — it is skipped, and an unresolvable-but-valid line
  still draws as a generic line. Malformed OSM is panic-free.

This is **Phase 0** (generalize the pass). The LOD-tiled PMTiles delivery and the
Europe-wide OSM extent widening are explicitly **out of scope** here and tracked
as later phases (see the unified-overlay-network roadmap).

## Consequences

- Multi-modal regions emit **more lines** (light/regional/heavy rail now draw);
  Rome, which has only ATAC subway+tram in its clip, is byte-stable — same nine
  lines, ids, colours, order, and dedup, now carrying the two additive props.
- **Identity is best-effort and OSM-dependent.** The `OSM:<network>:<ref>` id is
  only as stable as the OSM `network`/`ref`; lines without a clean ref still draw
  but their derived id is weak. `routable` reflects only whether the line matched
  the region's current in-scope feed.
- The GeoJSON **gains additive properties** a client may read; nothing the
  styles/gateway depend on is removed or renamed.
- Grouping now does two clip passes for the `route_master` map; negligible for a
  city clip, revisited if the Europe extent (a later phase) makes it costly.

## Alternatives considered

- **Stack, not dissolve (one feature per route relation).** Rejected: doubles
  features for bidirectional lines and breaks the one-geometry-per-line model the
  client draws.
- **Per-operator separate overlay files.** Rejected: multiplies the static files
  and the style sources for no display benefit; one file per kind with a
  `network` property is lighter and already what the styles wire.
- **Fail the build on an unresolvable line.** Rejected: the overlay is
  display-only and best-effort; a malformed or unkeyable line must still draw as
  a generic line rather than abort a whole region's build.
