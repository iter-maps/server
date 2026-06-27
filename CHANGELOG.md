# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/) and the project follows
[Semantic Versioning](https://semver.org/). Until the first tagged release,
everything lives under **Unreleased**. Entries are distilled from the Conventional
Commit history.

## [Unreleased]

### Added

- **Workspace** — a six-crate Cargo workspace: `iter-core` (config, error
  envelope, tracing, graceful shutdown, health model), `iter-contracts` (wire
  DTOs), `iter-region` (the region model), `iter-gateway` (edge/BFF),
  `iter-pipeline` (build tier), `iter-worker` (background jobs).
- **Gateway surface** — the full client-facing contract, served and tested:
  - basemap tiles over HTTP byte-range (gzip off, immutable cache);
  - the four MapLibre styles with per-request `__BASE_URL__` substitution;
  - glyphs with `NotoSans-Regular` fallback; road-shield sprite;
  - transit overlays (fail-soft GeoJSON);
  - client health (`/health`, `/health.json`) and the `/manifest` freshness
    document; `/livez` + `/readyz` orchestration probes;
  - live-trains (`/trenitalia/*`) — a normalized, TTL-cached, single-flighted
    proxy over ViaggiaTreno;
  - offline extract + bundle (`/offline/{extract,bundle}`) with bbox/zoom/area
    caps, a concurrency gate, and `go-pmtiles` range-extraction;
  - reverse proxy for routing (OTP) and geocoding (Photon).
- **Region model** — nested declarative profiles (`regions/<path>/region.toml`)
  resolved root→leaf into one effective config; `ITER_REGION` selects the target.
  Data is placed by service area, not operator. Italy → Lazio → Rome profiles
  included.
- **Place enrichment** — a keyless BFF surface above geocoding: `/places/enrich`
  fuses Wikipedia (summary + thumbnail + the Wikidata QID), Wikidata (`P18`
  image), and Wikimedia Commons (license + author) into the normalized `Place`
  DTO with per-field provenance; `/places/image` proxies the Commons image
  through the gateway. TTL-cached + single-flighted (ADR 0011). Proven live:
  enriching the Colosseo returned its Italian summary + a CC-BY-SA image served
  through the BFF.
- **Place correlation** — `/places/related` surfaces the places at a searched
  civico (the restaurant at "Via Cavour 1") as `related[]`, attaching not
  deduping. A **PLACES** pipeline step extracts addressed POIs from Overture; the
  gateway loads them into an in-memory bucket index keyed by an Italian address
  normalizer (street-type/DUG expansion, accent fold, esponenti, the
  Firenze/Genova red-vs-black flag, `snc` sentinel). ADR 0012.
- **Pipeline** — an idempotent step runner (`FORCE_*`/`SKIP_*`, skip-if-present,
  atomic writes, strict abort), region-driven (`ITER_REGION`), with steps: OSM
  source fetch, CLIP (osmium routing-extent clip), GTFS feed fetch, BUILD_CONFIG
  (OTP input pinning with stable feedIds), GRAPH (OTP `--build --save`), CIVICI
  (Italian house numbers from Overture addresses via DuckDB-by-bbox → Photon
  house docs), PHOTON (geocoding index import, civici appended, embedded
  OpenSearch, `-extra-tags` for the enrichment back-links), OVERLAY (the transit
  overlays — `transit-lines` + `metro-stations` — from the OSM clip, pure-Rust,
  ADR 0014), basemap tiles via planetiler
  (clustered PMTiles v3, z0-14), and HEALTH. Proven end-to-end on real data:
  planetiler tiles served + go-pmtiles offline extract; a real OTP graph built
  from a region-clipped OSM + ATAC GTFS, served and reachable as a real `plan`
  through the gateway (ADR 0009); a real Photon index (9,237 Overture civici over
  central Rome + a country dump) served and queried for a Rome civico through the
  gateway (ADR 0010); and the transit overlays generated from the real Rome clip
  (`transit-lines` → 9 line features; `metro-stations` → 443 features: concourse
  concave-hulls + per-direction platform strips + named exits).
- **Worker** — a graceful-shutdown job scheduler. **FL NeTEx→GTFS** (`fl-gtfs`):
  a streaming `quick-xml` parser (over a `flate2` gunzip) converts the official
  Lazio NeTEx into a routable GTFS the OTP graph build consumes — proven on the
  real ~58 MB CCISS dataset (450 stops, 5 routes, 1,594 trips, 20,617 stop_times,
  zero loss; ADR 0016). Plus **GTFS-RT ingestion** (`rt-reliability`): polls
  ATAC's trip-updates feed,
  decodes it with a vendored `prost` GTFS-RT subset (no `protoc`), and derives
  validated stop-delay events keyed on the stable (route, direction, stop,
  service-date) tuple — not the renumbered `trip_id` (ADR 0015). Proven on the
  live feed (721 trip updates → 13,909 events). The persistent reliability rollup
  tier lands next.
- **Containerization** — multi-stage Dockerfiles, a podman/docker compose stack
  with a dev override, `go-pmtiles` in the gateway image, a **data-prep image**
  (`eclipse-temurin:21-jre` + planetiler 0.10.2 + osmium + OTP 2.7.0 shaded jar +
  Photon 1.1.0 jar + DuckDB CLI with spatial/httpfs baked in + go-pmtiles)
  carrying the pipeline's build toolchain, and a slim **Photon serve image**. The
  OTP service loads the graph from `/data/graph`; Photon serves the index from
  `/data/photon` (read-write — embedded OpenSearch).
- **CI & governance** — a strict CI (fmt, clippy `-D warnings`, build, test,
  `cargo doc -D warnings`, cargo-deny, typos, REUSE, hadolint, coverage); 167
  tests; AGPL-3.0 + REUSE licensing; the ADR process (ADRs 0001–0012); CLAUDE.md;
  CONTRIBUTING (DCO), code of conduct, security, telemetry, and data-license
  docs; the deferred-work roadmap.

### Not yet implemented

The remaining data-production steps (overlay geometry, FL NeTEx→GTFS), the
deferred place-discovery waves (Wikivoyage editorial, commercial place/traffic
overlays — wave 1 keyless enrichment + correlation is built), the worker jobs
(RT-reliability rollups, daily graph refresh), and the planned forward-looking
capabilities — all tracked in [`docs/roadmap/`](docs/roadmap/).
