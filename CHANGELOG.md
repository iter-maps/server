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
- **Pipeline** — an idempotent step runner (`FORCE_*`/`SKIP_*`, skip-if-present,
  atomic writes, strict abort) with the HEALTH step.
- **Worker** — a graceful-shutdown job scheduler with the FL-GTFS build job.
- **Containerization** — multi-stage Dockerfiles (gateway/pipeline/worker), a
  podman/docker compose stack with a dev override, `go-pmtiles` in the gateway
  image.
- **CI & governance** — a strict CI (fmt, clippy `-D warnings`, build, test,
  `cargo doc -D warnings`, cargo-deny, typos, REUSE, hadolint, coverage); 124
  tests; AGPL-3.0 + REUSE licensing; the ADR process (ADRs 0001–0008); CLAUDE.md;
  CONTRIBUTING (DCO), code of conduct, security, telemetry, and data-license
  docs; the deferred-work roadmap.

### Not yet implemented

The data-production pipeline's engine-orchestration steps (OSM clip, planetiler
tiles, OTP graph, Photon import, overlay geometry, FL NeTEx→GTFS), the external
engines operating on real data, and the planned forward-looking capabilities —
all tracked in [`docs/roadmap/`](docs/roadmap/).
