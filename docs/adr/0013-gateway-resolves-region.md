# 0013 — The gateway resolves the region at startup

- **Status:** Accepted
- **Date:** 2026-06-27
- **Supersedes:** —
- **Superseded by:** —

## Context

ADR 0008 made the pipeline region-driven: artifacts are named from the resolved
region id (`<region.id>.pmtiles`, e.g. `rome.pmtiles`). The gateway, however,
still carried region-specific constants from the pre-region era — a literal
`roma.pmtiles` in the offline source, the manifest freshness stat, and the
offline style rewrite; a hardcoded `["metro-stations","transit-lines"]` overlay
allowlist; and `TRENITALIA_REGION` read straight from env. Because the pipeline
now writes `rome.pmtiles`, the gateway's `roma.pmtiles` literals point at files
that never exist: `/offline/extract`/`bundle` break, the manifest reports tiles
as permanently missing, and the offline style rewrite is a silent no-op so
bundled styles keep pointing at the server. This is the `roma.pmtiles` →
`<region>.pmtiles` rename ADR 0008 flagged as a coordination point.

## Decision

We will **resolve `iter-region` in the gateway at startup**, the same way the
pipeline does (`REGIONS_DIR` + `ITER_REGION`), and derive region-specific config
from the resolved profile instead of hardcoding it:

- `tiles_basename = "{region.id}.pmtiles"` — used by the offline source default,
  the manifest tiles freshness, and the offline style rewrite, so the basemap
  name can never drift from what the pipeline writes.
- `overlay_kinds = region.overlays[].kind` — drives the `/overlays/{kind}`
  allowlist, so a region with different overlays needs no code change.
- `trenitalia_region` seeds from `region.live_trains.region_code` (env still
  overrides).

The gateway image gains the `regions/` tree (`COPY regions /regions`,
`REGIONS_DIR=/regions`) since it now needs the region config at runtime.
Resolution failure is fatal (`from_env` returns `Result`) — a gateway that can't
resolve its region is misconfigured.

## Consequences

- The `roma.pmtiles` → `rome.pmtiles` rename ships: offline extract/bundle, the
  manifest, and bundled styles all work against real pipeline output. Clients
  that re-fetch the served style follow the new `/tiles/rome.pmtiles` URL
  transparently; only a client with a *hardcoded* tile URL would break — the
  ADR 0008 coordination point (no released client today).
- The gateway and pipeline read **one source of truth** for the region; adding a
  region is config + data on both tiers with no gateway code change.
- The gateway gains a startup dependency on the `regions/` tree (in the image,
  and at the repo root for `cargo run`); a missing tree fails fast at boot.
- The offline-bundle code threads the basename through `BundleDirs`, a small
  plumbing cost.

## Alternatives considered

- **A single `TILES_BASENAME` env var** — fixes only the rename, leaves the
  overlay allowlist and trenitalia region hardcoded, and adds a value that can
  drift from the region tree. Resolving the region fixes all three at the source.
- **Make resolution non-fatal with a fallback** — hides misconfiguration; the
  region tree is tiny and always shipped, so hard-failing at boot is clearer.
- **Leave the gateway env-driven, keep `roma.pmtiles`** — a live bug against the
  region-named artifacts the pipeline already writes.
