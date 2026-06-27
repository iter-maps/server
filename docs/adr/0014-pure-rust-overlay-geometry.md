# 0014 — Pure-Rust overlay geometry (no Python/shapely)

- **Status:** Accepted
- **Date:** 2026-06-27
- **Supersedes:** —
- **Superseded by:** —

## Context

The transit overlays (concept doc 09) — `transit-lines.geojson` (network line
geometry) and `metro-stations.geojson` (platform/concourse/exit cutouts) — are
generated from the region's OSM clip. The design blueprint's reference
implementation uses Python + shapely (for the concave-hull concourse and the
buffer/offset platform geometry). Pulling Python + shapely into the data-prep
image would add a second language runtime and a heavy native stack purely for
these two files, when every other pipeline step is Rust shelling to JVM/Go tools.
The rest of the data-prep image is already large (planetiler, OTP, Photon,
DuckDB); the overlays are by far the lightest step.

## Decision

We will **generate the overlays in pure Rust**, no Python/shapely:

- Read the clip with the `osmpbf` crate (way/node ids are intrinsic, so the
  osmium-export `--add-unique-id` id-survival gotcha is avoided).
- **`transit-lines`** needs no geometry library: it is an id-level union of OSM
  track ways. For each ATAC `route=subway|tram` relation with a `ref`, union the
  track-way members across every direction/variant relation of a line (shared
  track deduped by way id) and emit one `MultiLineString` feature per line, with
  the GTFS `route_id`/colour joined from `routes.txt`. Output via `serde_json`.
- **`metro-stations`** (next) uses the `geo` crate for the geometry primitives
  the blueprint did in shapely — `ConcaveHull` for the concourse, buffering for
  platforms/close — in local-planar metres, back to WGS84. `geo` provides a
  native concave hull, so no shapely is needed.

The step is region-driven (`region.overlays[].kind`) and builds only the kinds
it implements, warning-skipping the rest.

## Consequences

- No Python runtime enters the data-prep image; the overlay step is a fast,
  in-process Rust step (transit-lines over the Rome clip is seconds).
- New pipeline deps: `osmpbf`, `geo`, `geojson`. `geo` pulls a `thiserror` 1.x
  in its tree (a duplicate of the workspace's 2.x) — accepted by cargo-deny.
- **Concave-hull parity is not byte-exact:** `geo::ConcaveHull`'s concavity
  parameter is not shapely's `ratio`, so the metro-stations hulls will be tuned
  to visual equivalence, and the blueprint's exact feature/vertex counts are
  approximate sanity targets, not regression assertions.
- Polygon buffering in pure Rust is less battle-tested than GEOS; the
  metro-stations step may need geometry-validity hardening (documented when it
  lands).

## Alternatives considered

- **Python + shapely (the blueprint reference)** — adds a language runtime + a
  heavy native stack to the image for two small files; rejected on weight.
- **osmium-export → parse OPL in Rust** — hits the `--add-unique-id` id-survival
  gotcha and needs a separate export pass; reading the PBF directly is simpler.
- **GEOS (C library) for buffering** — more robust than pure-Rust buffering but
  adds a system dependency; try pure-Rust `geo` first, revisit only if needed.
