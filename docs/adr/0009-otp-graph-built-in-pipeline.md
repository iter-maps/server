# 0009 — OTP routing graph built in the pipeline

- **Status:** Accepted
- **Date:** 2026-06-27
- **Supersedes:** —
- **Superseded by:** —

## Context

Journey routing runs on OpenTripPlanner (OTP 2.7.0), a JVM engine the
coordinator fronts rather than reimplements (ADR 0002). OTP serves from a
prebuilt `graph.obj` and never builds at request time. Something has to produce
that graph from a clipped street network plus the region's GTFS feeds, and the
build is a heavyweight, one-shot, memory-hungry JVM job — exactly the build/serve
asymmetry the pipeline tier exists for (ADR 0003). OTP also needs Java 21 and a
~160 MB shaded jar, neither of which belongs in the slim gateway runtime. The
region model (ADR 0008) already resolves the routing extent and the feed list;
the graph build must consume those, not a hardcoded "Rome".

## Decision

We will build the OTP graph **inside `iter-pipeline`**, shelling out to the OTP
shaded jar bundled in the **data-prep image** (alongside planetiler/osmium), as
a sequence of idempotent steps:

- **CLIP** — `osmium extract -s complete_ways` carves the routing extent out of
  the downloaded PBF into `graph/<region>.osm.pbf`. `complete_ways` keeps
  boundary-crossing ways whole so the graph has no severed streets.
- **GTFS** — download each enabled, non-`netex` feed to
  `graph/<feedId>.gtfs.zip` (the literal `gtfs` in the name is required for OTP
  detection; optional feeds warn on failure, required feeds abort).
- **BUILD_CONFIG** — write `graph/build-config.json` pinning the OSM input and
  each feed with an explicit `feedId`, derived from what's on disk. Pinning
  disables OTP's base-dir auto-scan, giving stable, declared feed ids.
- **GRAPH** — `java -jar otp-shaded.jar --build --save graph/` → `graph/graph.obj`.

OTP then serves with `--load --serve /data/graph` (read-only). All OTP inputs
and the output share one directory, `/data/graph`. The steps no-op for a
basemap-only region (no routing extent), keeping the pipeline region-generic.

## Consequences

- Routing artifacts are regenerable from public sources by the same idempotent,
  skip-if-present runner as tiles — "clone + up" still holds, now with real
  journeys.
- The data-prep image carries a second JVM toolchain (OTP) and grows by the
  shaded jar; the OTP jar URL/version is pinned (`otp-shaded` Maven artifact) and
  must be bumped deliberately.
- The on-disk contract `/data/graph/{<region>.osm.pbf,<feedId>.gtfs.zip,
  build-config.json,graph.obj}` is now load-bearing: the compose OTP service and
  the HEALTH step both depend on it.
- Graph build RAM (`OTP_BUILD_HEAP`) can exceed a small host for a full-Lazio
  clip; `ROUTING_BOUNDS` shrinks the clip (e.g. to central Rome) for dev/CI, the
  same override pattern as `PMTILES_BOUNDS`.
- `router-config.json` (GTFS-RT updaters, fuzzy trip matching) is not written
  yet — realtime wiring is follow-on work (roadmap).

## Alternatives considered

- **Build in a separate OTP container, orchestrated by compose** — splits the
  build across two images and a startup ordering dance; the pipeline already owns
  every other build and has the region config in hand.
- **Rely on OTP base-dir auto-scan instead of `build-config.json`** — feed ids
  would derive from filenames implicitly; explicit pinning makes the
  `FEED:LOCALID` client contract stable and intentional.
- **Bundle the OTP jar into the gateway image** — bloats the stateless edge with
  a JVM it never runs; the build toolchain belongs in the data-prep image.
