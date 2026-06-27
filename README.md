# Iter Maps — server

A self-hosted, open-source, **zero-commercial-key** public-transport backend. It
turns open data (OSM, public GTFS, a Photon dump, Overture addresses) into the
map, search, and journey-planning surfaces consumed by the Iter Maps app — no
Mapbox, Google, or Geoapify key anywhere in the stack.

> **Status: early rebuild.** This is a ground-up Rust rebuild of a working
> proof-of-concept. The foundation (workspace, shared crates, the edge service)
> is in place; capabilities are landing incrementally. See
> [Status](#status) and [`docs/roadmap/`](docs/roadmap/) for what is built vs.
> planned. The full design lives in the [concept blueprint](#the-concept-blueprint).

License: **AGPL-3.0-or-later** (code) · **CC-BY-4.0** (docs)

---

## What it does

Five capability families, served to one consumer (the app) behind an *external*
proxy that terminates TLS — the backend ships no TLS, domain, or auth of its own:

1. **Journey routing** — multimodal A→B planning (metro/bus/tram/rail/walk),
   nearby stops, arrivals, route detail, alerts, with GTFS-RT delays merged in.
2. **Geocoding** — forward autocomplete + reverse over all-Italy OSM, enriched
   with georeferenced Italian house numbers (civici).
3. **Basemap tiles + styles** — range-served vector PMTiles and host-agnostic
   MapLibre styles (Standard/Transit × light/dark), glyphs, road-shield sprites.
4. **Transit overlays** — server-generated GeoJSON the client draws over the map
   (metro-station cutouts + line geometries).
5. **Offline + live-trains gateway** — offline map pre-download (bbox PMTiles
   extract + bundle) and live train boards normalized from ViaggiaTreno.

**Scope split (deliberate):** basemap + geocoding cover **all of Italy**;
transit routing covers **Rome / Lazio only** — there is no national GTFS. The two
extents are independent knobs and must not be conflated.

## Architecture

A Rust **coordinator** fronts a couple of best-in-class external engines rather
than reinventing them:

| Component | Role | Tech |
|---|---|---|
| `iter-gateway` | Stateless edge/BFF: serves tiles, styles, glyphs, sprite, overlays, offline, live-trains, health; reverse-proxies routing and geocoding | Rust (axum) |
| `iter-pipeline` | Idempotent data-prep orchestrator (fetch → clip → build → render → import → health), with `FORCE_*`/`SKIP_*` knobs | Rust |
| `iter-worker` | Background jobs (FL-GTFS build, future RT polling, reliability rollups) | Rust |
| OTP | Journey routing (`POST /otp/gtfs/v1` GraphQL) | OpenTripPlanner 2 (JVM) |
| Photon | Geocoding (`/api`, `/reverse`, `/status`) | Komoot Photon (JVM) |

Shared crates: `iter-core` (config, error envelope, tracing, shutdown, health),
`iter-contracts` (wire DTOs), `iter-region` (the region model — see
[`docs/adr/0008`](docs/adr/0008-region-model-nested-profiles.md)).

The split follows the build/serve asymmetry: the stateless edge is cheap to
**scale wide** (replicas), the data-heavy engines stay **narrow**, and the heavy
one-shot builds run in the pipeline tier. Services are stateless with
externalized, regenerable artifacts — so the same code runs as a single
`podman compose` stack *and* scales to Kubernetes replicas + workers.

## Status

| Area | State |
|---|---|
| Workspace · `iter-core` · `iter-contracts` · `iter-region` | ✅ done |
| Gateway surface — tiles, styles, glyphs, sprite, overlays, health, freshness manifest, live-trains, offline extract/bundle, routing/geocoding proxy, liveness/readiness | ✅ done, tested |
| Region model (nested profiles, `ITER_REGION`) | ✅ done |
| `iter-pipeline` runner (full step set) + `iter-worker` scheduler (FL-GTFS + GTFS-RT ingestion) | ✅ done |
| Containerization (multi-stage Dockerfiles, compose, `go-pmtiles`) + strict CI (167 tests) | ✅ done |
| Data pipeline — OSM fetch + planetiler tiles (region-driven) | ✅ done, proven on real output |
| Data pipeline — OSM clip + GTFS fetch + OTP graph build (region-driven) | ✅ done, proven on real output |
| Data pipeline — civici extraction + Photon geocoding index (region-driven) | ✅ done, proven on real data |
| Data pipeline — transit overlays (transit-lines + metro-stations, from OSM) | ✅ done, proven on real data |
| Worker — FL NeTEx→GTFS conversion + GTFS-RT ingestion | ✅ done, proven on real data |
| Routing engine operational (OTP serving a real graph) | ✅ done, proven on real data |
| Geocoding engine operational (Photon serving real index + civici) | ✅ done, proven on real data |
| Place enrichment — image + summary for a tapped result (keyless, proxied) | ✅ done, proven on real data |
| Place correlation — related places sharing an address + civico | ✅ done, proven on real data |
| Planned forward-looking features (16, 19–28) | 🔜 roadmap |

## Quick start

> The runtime stack (`compose.yaml` + Dockerfiles) is being assembled — until it
> lands, build and run the edge service directly with `cargo`.

The design goal is **clone + up** — no host-side tools, no manual downloads:

```sh
git clone https://github.com/iter-maps/server
cd server
cp .env.example .env
podman compose up        # (Docker Compose v2 works too)
```

First boot fetches and builds every artifact (graph, index, tiles, overlays)
from public sources; subsequent boots skip steps whose output already exists.

### Developing the Rust services

```sh
cargo build
cargo test
cargo run -p iter-gateway        # serves on :8090 by default
```

## Repo layout

```
crates/
  iter-core/        shared primitives (config, error envelope, health, shutdown)
  iter-contracts/   wire-contract DTOs (geo, health, live-trains, offline, places)
  iter-region/      the region model (nested profiles, resolver)
  iter-gateway/     edge/BFF service (axum)
  iter-pipeline/    build tier — idempotent data-prep orchestrator
  iter-worker/      background jobs (FL-GTFS, RT polling, reliability)
docs/               architecture, API contract, roadmap   (CC-BY-4.0)
```

## Configuration

Everything is configured through environment variables (`.env` for "clone +
up"); see [`.env.example`](.env.example). Key knobs: service ports
(`GATEWAY_PORT`, …), upstream URLs (`OTP_URL`, `PHOTON_URL`), the per-host
extent overrides (`ROUTING_BOUNDS`, `PMTILES_BOUNDS`), and the pipeline's
`FORCE_*`/`SKIP_*` step overrides.

## Licensing

- **Code** — AGPL-3.0-or-later. The network-copyleft trigger (§13) means a
  modified *hosted* iter-server must offer its users the source.
- **Docs** (`docs/`, the concept blueprint) — CC-BY-4.0.
- **Redistributed data** — each source's license is a legal obligation, not a
  courtesy: OSM/ODbL attribution + share-alike, GTFS/CC-BY, Noto/OFL, etc. See
  `DATA_LICENSES.md`.

The app talks to this backend over a documented, arm's-length HTTP/GraphQL
contract, so it is a separate program and is **not** pulled under AGPL — third
parties are welcome to build their own clients against the API.

## Contributing

Contributions are under the **DCO** (`git commit -s` — inbound = outbound, no
CLA). See `CONTRIBUTING.md` and `CODE_OF_CONDUCT.md`. Security issues:
`SECURITY.md`.

## Design

The implementation follows a structure-agnostic design that pins the **external
wire contracts**, **data provenance**, **algorithms**, and **invariants** while
leaving the stack free to change. The published design and API contract live
under [`docs/`](docs/) (CC-BY-4.0); see [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
for how this rebuild is put together, and [`docs/adr/`](docs/adr/README.md) for
the decisions behind it.
