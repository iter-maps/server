# Roadmap

An honest map of everything **not yet built**, each item pointed at its design
source so there are no silent gaps. Three groups: external-engine integration,
remaining Rust capabilities, and planned forward-looking features.

The architecture this plugs into is in [`../ARCHITECTURE.md`](../ARCHITECTURE.md):
a stateless **gateway** (edge/BFF) fronting OTP + Photon, an idempotent
**pipeline** (build tier), and an **iter-worker** (background jobs). "Design" /
"Decision" pointers below name the design blueprint's documents and ADRs by
number (e.g. "concept doc 16", "ADR 0014") — the structure-agnostic source of
truth maintained alongside the project.

## 1. External-engine integration (orchestrate-external)

Mature external engines and tools we **orchestrate**, not reimplement. These run
inside the pipeline/worker build images, never on the host. Tractable but
unbuilt; tracked here so the orchestration boundary stays explicit.

- **OTP graph build + GTFS-RT + daily fresh graph** — clip OSM
  (`osmium extract --bbox=$BBOX_LAZIO`), build the OTP2 graph from the 5 GTFS
  feeds, serve `POST /otp/gtfs/v1`; attach the 3 ATAC GTFS-RT updaters
  (trip-updates/vehicle-positions @30 s, alerts @60 s) with `fuzzyTripMatching`;
  rebuild the static graph **daily** (mandatory for calendar sparsity + trip-id
  churn) with a keep-old-in-RAM, single-recreate ~30–60 s swap.
  Design: concept doc 05 — routing-engine, doc 10 — realtime-transit ·
  Decision: ADR 0013.
- **Photon import + civici** — build the Photon index from the raw Italy dump
  (`photon.jar` import), enriched with Italian house numbers (civici) extracted
  via DuckDB from Overture/ANNCSU parquet (bbox filter, title-case, dedup by
  street/number/city); serve `/api`, `/reverse`, `/status`.
  Design: concept doc 06 — geocoding-engine ·
  Decision: ADR 0002.
- **planetiler PMTiles render** — render the all-Italy z0–14 OMT PMTiles v3
  archive from the Geofabrik PBF (+ auto-fetched water-polygons, natural-earth,
  lake-centerline ancillaries), Hilbert-clustered, atomic-replaced; plus the
  Pillow road-shield sprite at 1×/2×.
  Design: concept doc 07 — basemap-and-tiles, doc 08 — map-styling ·
  Decision: ADR 0008.
- **osmium clips** — `osmium extract --bbox` for the Lazio routing PBF and
  `osmium tags-filter` → OPL export of rail relations consumed by overlay/FL
  builders.
  Design: concept doc 04 — data-pipeline.
- **FL NeTEx→GTFS** — gateway/worker job synthesizing a Trenitalia-FL GTFS feed
  from CCISS-NAP NeTEx (SAX parse, calendar re-anchor, shape stitching,
  category/geographic filters); fed into the OTP graph. NAP auto-download is
  unsolved (manual placement fallback).
  Design: concept doc 11 — gateway-and-external-providers ·
  Decision: ADR 0004.

## 2. Remaining Rust capabilities

Rust-native surfaces. The gateway already serves tiles, styles, glyphs, sprite,
overlays, client health, and reverse-proxies routing/geocoding; the pipeline and
worker frameworks are in place. What remains:

- **Live-trains** — `GET /trenitalia/*`: normalized, TTL-cached, single-flighted
  proxy over ViaggiaTreno (station search, departures/arrivals boards). Deferred
  because the exact upstream field names and the DST-aware `Date.toString()`
  date-param need validating against the real (cleartext, external) API.
  Design: concept doc 11 — gateway-and-external-providers,
  concept doc 02-api-contracts/live-trains.
- **Offline extract / bundle** — `GET /offline/{extract,bundle}`: range-read the
  clustered PMTiles and zip a bundle, with the abuse guards (6 deg² cap, z14
  clamp, 3 concurrent — already typed in `iter-contracts::offline`) that are the
  only protection on this auth-less surface. Needs the pinned `go-pmtiles` CLI
  (or a Rust PMTiles v3 reader, the planned `iter-pmtiles` crate) to do the
  range-extract.
  Design: concept doc 07 — basemap-and-tiles,
  concept doc 02-api-contracts/offline ·
  Decision: ADR 0010.
- **Freshness manifest** — `GET /manifest`: per-artifact `{builtAt, etag}` so
  cache-first clients check staleness in one request (the typed `FreshnessManifest`
  already exists; the gateway's `/health` + `/livez` + `/readyz` are done).
  Design: concept doc 18 — client-facing-contract-and-load §4.
- **Pipeline engine steps + refresh triggers** — the runner framework, the
  `FORCE_*`/`SKIP_*` knobs, and the HEALTH step are implemented; the engine-
  orchestration steps (§1) and the daily (`--gtfs`) / monthly (`--osm`) refresh
  triggers remain. The worker scheduler + FL-GTFS job skeleton are in place; the
  FL NeTEx→GTFS conversion and RT-polling jobs land per §1.
  Design: concept doc 04 — data-pipeline, doc 12 — deployment-and-operations ·
  Decision: ADR 0007.

## 3. Planned forward-looking features

Documented designs, none built. One short file each, mapping the feature to its
concept doc and ADR.

| Feature | Plugs into | File |
|---|---|---|
| Personalized planning | gateway rerank | [`personalized-planning.md`](personalized-planning.md) |
| Synchronization / E2EE | gateway (scoped state) | [`synchronization.md`](synchronization.md) |
| Place discovery | gateway fusion | [`place-discovery.md`](place-discovery.md) |
| Traffic data | pipeline → gateway | [`traffic-data.md`](traffic-data.md) |
| Crowd telemetry | gateway ingest + worker | [`crowd-telemetry.md`](crowd-telemetry.md) |
| Historical reliability | worker archive → gateway | [`historical-reliability.md`](historical-reliability.md) |
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
