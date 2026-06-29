# Roadmap

An honest map of everything **not yet built**, each item pointed at its
decision record so there are no silent gaps. Three groups: external-engine
integration, remaining Rust capabilities, and planned forward-looking features.

> **Cross-cutting:** [`region-decoupling.md`](region-decoupling.md) tracks moving
> the Italy/Rome specifics out of the generic core (ADR 0017) — config-drivable
> params into `region.toml`, custom code into `regions::<country>` drivers. The
> first driver (the Italian address normalizer) has landed; that file is the
> classified worklist for the rest.

The architecture this plugs into is in [`../ARCHITECTURE.md`](../ARCHITECTURE.md):
a stateless **gateway** (edge/BFF) fronting OTP + Photon, an idempotent
**pipeline** (build tier), and an **iter-worker** (background jobs). "Decision"
pointers below name the relevant ADRs by number (e.g. "ADR 0009").

## 1. External-engine integration (orchestrate-external)

Mature external engines and tools we **orchestrate**, not reimplement. These run
inside the pipeline/worker build images, never on the host. The static builds
are now **done and proven**; the remaining sub-parts (realtime, refresh, FL) are
flagged per item.

- **OTP graph build** ✅ done (ADR 0009) **· GTFS-RT + daily fresh graph** 🚧 —
  the static graph is built and served: CLIP (osmium) → GTFS fetch →
  BUILD_CONFIG → GRAPH (OTP `--build --save`) → ROUTER_CONFIG (GTFS-RT updaters)
  → OTP serves `POST /otp/gtfs/v1` (proven with a real Rome `plan`). The GTFS-RT
  updaters are wired (ADR 0020): a ROUTER_CONFIG step writes `router-config.json`
  from the region's realtime channels (trip-updates/vehicle-positions @30 s,
  alerts @60 s, `fuzzyTripMatching` on trip-updates), one updater per channel
  with a url — today the ATAC `trip-updates` stream. **Remaining:** the **daily**
  static-graph rebuild (mandatory for calendar sparsity + trip-id churn) with a
  keep-old-in-RAM ~30–60 s swap.
  Decision: ADR 0009, 0020.
- **Photon import + civici** ✅ done (ADR 0010) — the Photon index is built from
  the raw Italy dump (`photon.jar import`, embedded OpenSearch) enriched with
  Italian civici extracted via DuckDB from Overture addresses (bbox filter, dedup
  by street/number/city); serves `/api`, `/reverse`, `/status` (proven serving a
  real Rome civico). **Remaining:** all-Italy full-index scaling (prod host).
  Decision: ADR 0010.
- **planetiler PMTiles render** ✅ done — renders the OMT PMTiles v3 archive
  (z0–14, Hilbert-clustered, atomic-replaced) from the Geofabrik PBF + ancillaries
  (proven on real Monaco/Rome output). **Remaining:** all-Italy render (prod host)
  and the road-shield sprite.
- **osmium clips** ✅ done (the routing CLIP) — `osmium extract --bbox` for the
  region routing PBF; the rail-relation export for overlay/FL builders lands with
  the OVERLAY/FL work below.
- **FL NeTEx→GTFS** ✅ done (ADR 0016) — the worker streams the official Lazio
  NeTEx (quick-xml over gunzip) into a routable GTFS the OTP graph build consumes;
  proven on the real ~58 MB CCISS dataset (450 stops / 5 routes / 1,594 trips /
  20,617 stop_times, zero loss). Auto-downloads from the Italian NAP (CCISS)
  public endpoint each run. The `UicOperatingPeriod`/`ValidDayBits` are now
  expanded via `DayTypeAssignment` into exact `calendar_dates` (calendar_dates-
  only). `shapes.txt` is now stitched **best-effort** from OSM rail: a pure
  stitcher chains each `route=train` relation's member rail ways into one ordered
  polyline per branch (greedy endpoint joining with flips, longest-run-wins on a
  gap, per-branch split), wired through an `osmpbf` clip reader and matched to
  routes by `ref`. Fail-soft — no clip (the default) emits the feed exactly as
  before. **Remaining:** none core; a fuller per-pattern multi-segment shape is
  possible if OTP ever needs it.
  Decision: ADR 0016.

## 2. Remaining Rust capabilities

Rust-native surfaces. The gateway already serves tiles, styles, glyphs, sprite,
overlays, client health + freshness manifest, **live-trains** (ViaggiaTreno
proxy with TTL cache + single-flight), and **offline extract + bundle**
(go-pmtiles in the gateway image); it reverse-proxies routing/geocoding. The
pipeline and worker frameworks are in place. What remains:

- **Live-trains** ✅ verified — the ViaggiaTreno proxy (cache, normalization,
  `Date.toString()` date-param) is confirmed end-to-end against the real API
  (2026-06-28): station search, the regional list (with lat/lon), and the
  arrivals/departures boards return correctly-normalized real data.
- **Overlay geometry** ✅ done (pure Rust, ADR 0014) — `transit-lines` (way-union
  `MultiLineString` per line, GTFS colours) and `metro-stations` (concave-hull
  concourses + per-direction platform strips offset along the real track + named
  exits), proven on the real Rome clip (9 lines; 443 station features). The
  concourse is now smoothed into an organic footprint (Chaikin corner-cutting +
  Visvalingam-Whyatt simplification, with a per-station fall-back to the raw hull
  if smoothing would invalidate the polygon, drop a stop, or distort the area).
  `transit-lines` is now multi-region (ADR 0029, overlay Phase 0): a widened route
  set ({subway, tram, light_rail, rail, regional_rail}), `route_master`-preferred
  grouping with a `(network, mode, ref)` fallback, and additive `network`/`routable`
  identity props (`OSM:<network>:<ref>` for overlay-only lines), with Rome's nine
  lines byte-stable. The metro-stations concourse dissolves its overlapping
  platform strips into one footprint (corridor union via `geo::unary_union`, ADR
  0031). **Remaining:** the morphological buffer-close (rounding concave inlets),
  still gated on a robust pure-Rust polygon buffer; the unified-overlay later
  phases (LOD PMTiles delivery, Europe OSM widening). Decisions: ADR 0014, ADR
  0025, ADR 0029, ADR 0031.
- **Style render** ✅ done (ADR 0025) — the `STYLES` pipeline step renders the four
  whitelisted MapLibre styles (Standard / Transit × light / dark) into
  `output/styles/`, each wired to the region's tile source, glyphs, the sprite
  (Standard only), and the region's overlay sources via the literal `__BASE_URL__`
  token, byte-stable so the gateway serves exactly what the build produced.
- **Pipeline refresh triggers** — the runner framework, the `FORCE_*`/`SKIP_*`
  knobs, and the build steps are implemented; the daily (`--gtfs` + graph) /
  monthly (`--osm`) refresh **triggers** remain. Best realized as a scheduled
  pipeline run (k8s CronJob / cron with `FORCE_GTFS`+`FORCE_GRAPH`) rather than a
  job in the lean worker, which lacks the OTP build toolchain. The worker runs the
  **GTFS-RT ingestion** job (ADR 0015: poll → decode → stable-key delay events,
  proven on the live ATAC feed) and the FL NeTEx→GTFS conversion; the persistent
  reliability **rollup** tier (Tier-0/1/2 + its P7-stateless exception) has now
  landed (ADR 0022) — its gateway read endpoint is the remaining gap.
  Decision: ADR 0007.

## 3. Planned forward-looking features

Documented designs, none built. One short file each, mapping the feature to its
decision record.

| Feature | Plugs into | File |
|---|---|---|
| Personalized planning | gateway rerank | [`personalized-planning.md`](personalized-planning.md) |
| Synchronization / E2EE | gateway (scoped state) | [`synchronization.md`](synchronization.md) |
| Place discovery (**wave 1 built** — enrichment + correlation; ADRs 0011/0012) | gateway fusion | [`place-discovery.md`](place-discovery.md) |
| Traffic data | pipeline → gateway | [`traffic-data.md`](traffic-data.md) |
| Crowd telemetry | gateway ingest + worker | [`crowd-telemetry.md`](crowd-telemetry.md) |
| Historical reliability (**RT ingestion + rollup tier built**; read endpoint next — ADRs 0015/0022) | worker archive → gateway | [`historical-reliability.md`](historical-reliability.md) |
| Italy/Europe rail + catalog | pipeline acquisition | [`italy-europe-rail.md`](italy-europe-rail.md) |
| Unified overlay network | pipeline overlay model | [`unified-overlay-network.md`](unified-overlay-network.md) |
| Stations & pathways | pipeline → routing | [`stations-pathways.md`](stations-pathways.md) |
| Scoped overlay delivery | pipeline → tiles | [`scoped-overlay-delivery.md`](scoped-overlay-delivery.md) |

All section-3 items are classified **defer-roadmap** in `.build-map/digest.json`:
large, multi-layer, phased — gated on architecture review and capacity. The
privacy-first **P7 stateless** invariant holds, with three scoped, opt-in
exceptions (E2EE sync, crowd telemetry, aggregate-only reliability). No
commercial keys for routing/geocoding; every commercial place/traffic source is
opt-in, flagged, with a keyless open fallback.
