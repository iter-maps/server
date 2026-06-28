# Roadmap

An honest map of everything **not yet built**, each item pointed at its design
source so there are no silent gaps. Three groups: external-engine integration,
remaining Rust capabilities, and planned forward-looking features.

> **Cross-cutting:** [`region-decoupling.md`](region-decoupling.md) tracks moving
> the Italy/Rome specifics out of the generic core (ADR 0017) — config-drivable
> params into `region.toml`, custom code into `regions::<country>` drivers. The
> first driver (the Italian address normalizer) has landed; that file is the
> classified worklist for the rest.

The architecture this plugs into is in [`../ARCHITECTURE.md`](../ARCHITECTURE.md):
a stateless **gateway** (edge/BFF) fronting OTP + Photon, an idempotent
**pipeline** (build tier), and an **iter-worker** (background jobs). "Design" /
"Decision" pointers below name the design blueprint's documents and ADRs by
number (e.g. "concept doc 16", "ADR 0009") — the structure-agnostic source of
truth maintained alongside the project.

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
  Design: concept doc 05 — routing-engine, doc 10 — realtime-transit ·
  Decision: ADR 0009, 0020.
- **Photon import + civici** ✅ done (ADR 0010) — the Photon index is built from
  the raw Italy dump (`photon.jar import`, embedded OpenSearch) enriched with
  Italian civici extracted via DuckDB from Overture addresses (bbox filter, dedup
  by street/number/city); serves `/api`, `/reverse`, `/status` (proven serving a
  real Rome civico). **Remaining:** all-Italy full-index scaling (prod host).
  Design: concept doc 06 — geocoding-engine ·
  Decision: ADR 0010.
- **planetiler PMTiles render** ✅ done — renders the OMT PMTiles v3 archive
  (z0–14, Hilbert-clustered, atomic-replaced) from the Geofabrik PBF + ancillaries
  (proven on real Monaco/Rome output). **Remaining:** all-Italy render (prod host)
  and the road-shield sprite.
  Design: concept doc 07 — basemap-and-tiles, doc 08 — map-styling.
- **osmium clips** ✅ done (the routing CLIP) — `osmium extract --bbox` for the
  region routing PBF; the rail-relation export for overlay/FL builders lands with
  the OVERLAY/FL work below.
  Design: concept doc 04 — data-pipeline.
- **FL NeTEx→GTFS** ✅ done (ADR 0016) — the worker streams the official Lazio
  NeTEx (quick-xml over gunzip) into a routable GTFS the OTP graph build consumes;
  proven on the real ~58 MB CCISS dataset (450 stops / 5 routes / 1,594 trips /
  20,617 stop_times, zero loss). Auto-downloads from the Italian NAP (CCISS)
  public endpoint each run. **Remaining:** `UicOperatingPeriod` →
  `calendar_dates` exceptions and `shapes.txt` stitching from OSM rail.
  Design: concept doc 11 — gateway-and-external-providers · Decision: ADR 0016.

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
  exits), proven on the real Rome clip (9 lines; 443 station features).
  **Remaining:** the reference impl's morphological smoothing + corridor union
  (the current concourse is a concave hull) and the `STYLES` render step.
  Design: concept doc 09 — overlays-geometry · Decision: ADR 0014.
- **Pipeline refresh triggers** — the runner framework, the `FORCE_*`/`SKIP_*`
  knobs, and the build steps are implemented; the daily (`--gtfs` + graph) /
  monthly (`--osm`) refresh **triggers** remain. Best realized as a scheduled
  pipeline run (k8s CronJob / cron with `FORCE_GTFS`+`FORCE_GRAPH`) rather than a
  job in the lean worker, which lacks the OTP build toolchain. The worker runs the
  **GTFS-RT ingestion** job (ADR 0015: poll → decode → stable-key delay events,
  proven on the live ATAC feed); the FL NeTEx→GTFS conversion and the persistent
  reliability **rollup** tier (Tier-0/1/2 + its P7-stateless exception ADR) land
  next.
  Design: concept doc 04 — data-pipeline, doc 12 — deployment-and-operations ·
  Decision: ADR 0007.

## 3. Planned forward-looking features

Documented designs, none built. One short file each, mapping the feature to its
concept doc and ADR.

| Feature | Plugs into | File |
|---|---|---|
| Personalized planning | gateway rerank | [`personalized-planning.md`](personalized-planning.md) |
| Synchronization / E2EE | gateway (scoped state) | [`synchronization.md`](synchronization.md) |
| Place discovery (**wave 1 built** — enrichment + correlation; ADRs 0011/0012) | gateway fusion | [`place-discovery.md`](place-discovery.md) |
| Traffic data | pipeline → gateway | [`traffic-data.md`](traffic-data.md) |
| Crowd telemetry | gateway ingest + worker | [`crowd-telemetry.md`](crowd-telemetry.md) |
| Historical reliability (**RT ingestion built**; rollup tier next — ADR 0015) | worker archive → gateway | [`historical-reliability.md`](historical-reliability.md) |
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
