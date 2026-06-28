# Overview

The backend powering **Iter Maps** — a public-transport journey-planning and maps
platform for Europe. It turns open data (OpenStreetMap, public GTFS/NeTEx, address
data) into the map, search, and journey-planning surfaces the Iter Maps app
consumes. It is open-source, and free of commercial routing/geocoding keys.

## What it does

Five capability families, served to the app behind an external proxy that
terminates TLS — the backend ships no TLS, domain, or auth of its own:

1. **Journey routing** — multimodal A→B planning (metro / bus / tram / rail /
   walk), nearby stops, arrivals, route detail, and alerts, with live GTFS-RT
   delays merged in.
2. **Geocoding** — forward autocomplete + reverse search, enriched with
   georeferenced house numbers (civici).
3. **Basemap tiles + styles** — range-served vector PMTiles and host-agnostic
   MapLibre styles (Standard / Transit × light / dark), glyphs, road-shield
   sprites.
4. **Transit overlays** — server-generated GeoJSON the client draws over the map
   (metro-station cutouts + line geometries).
5. **Offline + live-trains** — offline map pre-download (bbox PMTiles extract +
   bundle) and live train boards.

**Scope today:** basemap + geocoding cover all of Italy; transit routing covers
Rome / Lazio. The two extents are independent and expand toward Europe.

## Architecture

A Rust coordinator fronts a couple of mature external engines rather than
reinventing them. Full picture in [`ARCHITECTURE.md`](ARCHITECTURE.md); the
decisions behind it in [`adr/`](adr/README.md).

| Component | Role |
|---|---|
| `iter-gateway` | Stateless edge/BFF: serves tiles, styles, glyphs, sprite, overlays, offline, live-trains, health; reverse-proxies routing + geocoding |
| `iter-pipeline` | Idempotent data-prep orchestrator (fetch → clip → build → render → import), with `FORCE_*`/`SKIP_*` knobs |
| `iter-worker` | Background jobs (NeTEx→GTFS build, GTFS-RT ingestion, reliability) |
| routing engine | Journey routing — OpenTripPlanner |
| geocoding engine | Forward + reverse geocoding — Photon |

Shared crates: `iter-core` (config, error envelope, tracing, health),
`iter-contracts` (wire DTOs), `iter-region` (the region model), and
`iter-region-drivers` (per-region drivers — address, live-trains, overlays,
NeTEx; one folder per country).

The split follows the build/serve asymmetry: the stateless edge scales **wide**
(replicas), the data-heavy engines stay **narrow**, and the heavy one-shot builds
run in the pipeline tier. Services are stateless with externalized, regenerable
artifacts — so the same code runs as a single compose stack (`docker/compose.yaml`) and scales
to Kubernetes replicas + workers.

## Status

| Area | State |
|---|---|
| Workspace · core / contracts / region / region-drivers crates | done |
| Gateway surface — tiles, styles, glyphs, sprite, overlays, health, manifest, live-trains, offline extract/bundle, routing/geocoding proxy | done, tested |
| Region model (nested profiles, `ITER_REGION`) + per-country drivers | done |
| Pipeline runner (full step set) + worker scheduler | done |
| Containerization (multi-stage Dockerfiles, compose, `go-pmtiles`) + strict CI | done |
| Basemap tiles (planetiler) · OSM clip + GTFS + OTP graph build | done, proven on real output |
| Civici extraction + Photon geocoding index | done, proven on real data |
| Transit overlays (lines + metro-stations, from OSM) | done, proven on real data |
| FL NeTEx→GTFS conversion + GTFS-RT ingestion + live routing | done, proven on real data |
| Place enrichment + correlation (keyless, proxied) | done, proven on real data |
| Forward-looking features | see the [roadmap](../ROADMAP.md) |
