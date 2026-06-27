# Architecture

How this Rust rebuild is put together, and why. It realizes a structure-agnostic
design: the **wire contracts, data provenance, algorithms, and invariants** are
fixed; the stack here is one valid way to honor them.

## Shape

The backend is a Rust **coordinator** in front of two external engines. Routing
and geocoding are mature, memory-heavy JVM engines (OpenTripPlanner, Komoot
Photon) — we orchestrate them, we don't reimplement them. Everything else (tile
serving, styles, overlays, offline, live-trains, health, and the data pipeline)
is Rust.

```
                 external proxy (TLS, domain, prod CORS, rate-limit)   ── out of scope (P3)
                                     │
        ┌────────────────────────────┼─────────────────────────────┐
        │                 internal container network                │
        │                                                           │
        │   ┌─────────────┐   reverse-proxy    ┌──────────────┐     │
client ─┼──▶│ iter-gateway │──────────────────▶│ OTP  :8080   │     │  routing
        │   │   (edge/BFF) │──────────────────▶│ Photon :2322 │     │  geocoding
        │   │    :8090     │                    └──────────────┘     │
        │   │  serves directly:                                      │
        │   │   tiles · styles · glyphs · sprite · overlays          │
        │   │   offline · live-trains · health · manifest            │
        │   └─────────────┘                                          │
        │          ▲ reads                                           │
        │   ┌──────┴───────────────┐         ┌──────────────┐        │
        │   │  read-only artifacts │◀────────│ iter-pipeline│ (build)│
        │   │  (graph, index,      │  writes │ iter-worker  │ (jobs) │
        │   │   tiles, overlays)   │         └──────────────┘        │
        │   └──────────────────────┘                                 │
        └───────────────────────────────────────────────────────────┘
```

## Components

### `iter-gateway` (edge / BFF) — stateless, scale-wide

The single ingress for everything the Rust side owns, plus a reverse proxy to the
engines. It is **stateless across requests** (no per-client state), so replicas
scale horizontally with no coordination. Surfaces:

- **Tiles** — `GET /tiles/*.pmtiles` via HTTP byte-range (gzip off; the archive
  is internally compressed and must be clustered).
- **Styles / glyphs / sprite** — the four MapLibre styles with per-request
  `__BASE_URL__` substitution; glyph stacks fall back to `NotoSans-Regular`
  (never 404).
- **Overlays** — `GET /overlays/{metro-stations,transit-lines}.geojson`,
  fail-soft (missing file → client draws nothing, no error).
- **Offline** — `GET /offline/{extract,bundle}`; range-reads the clustered
  PMTiles (via the pinned `go-pmtiles` CLI) and zips a bundle. Abuse guards
  (6 deg² area cap, z14 clamp, 3 concurrent) are the only protection on this
  public, auth-less surface.
- **Live-trains** — `GET /trenitalia/*`; a normalized, TTL-cached,
  single-flighted proxy over ViaggiaTreno.
- **Health** — client-facing freshness `health.json`, the `GET /manifest`
  per-artifact freshness document, plus the orchestration probes (`/livez`,
  `/readyz`).
- **Routing / geocoding** — reverse-proxied to OTP / Photon; the BFF is where
  future itinerary re-ranking and place-enrichment will sit.

### `iter-pipeline` (build tier) — one-shot / scheduled

The idempotent data-prep orchestrator: fetch → clip → build → render → import →
write-health, every step **skipped when its output exists** and **forceable**
individually (`FORCE_<step>` / `SKIP_<step>`). It coordinates external tools
(osmium, planetiler, the OTP graph build, the Photon import) — which live inside
the build image, never on the host — alongside Rust-native steps (glyph fetch,
style render, build-config generation, overlay generation, health write).

Implemented steps: **OSM** (fetch the regional PBF) → **CLIP** (osmium carves the
routing extent) → **GTFS** (fetch the region's feeds) → **BUILD_CONFIG** (pin
OTP's inputs with stable feedIds) → **GRAPH** (OTP `--build --save`) → **TILES**
(planetiler render) → **HEALTH**. OTP's inputs and its `graph.obj` share one
directory, `/data/graph`, which the OTP service then loads read-only (ADR 0009).
The routing steps no-op for a basemap-only region. STYLES, OVERLAY, CIVICI, and
the Photon import land next (roadmap).

### `iter-worker` (background tier)

Long-running scheduled jobs: the FL NeTEx→GTFS build (on startup + every 24 h),
and the planned RT polling / reliability rollups. Modelled as a job abstraction
so it scales independently of the request path.

### Shared crates

- **`iter-core`** — config helpers, the `{error:{code,message,details?}}`
  envelope, operator-local tracing, SIGINT/SIGTERM graceful shutdown, the
  liveness/readiness model.
- **`iter-contracts`** — the wire DTOs (camelCase field names the client greps
  for): `geo::BBox`, health documents, live-trains board/station, offline
  manifest + caps + error codes.
- **`iter-region`** — the region model: profile schema + the root→leaf resolver
  (see [Region model](#region-model)).

(Tile range-extract is done by the pinned `go-pmtiles` CLI rather than a Rust
PMTiles reader; see ADR 0007.)

## Region model

Region is a first-class, region-generic abstraction — not hardcoded constants
(ADR 0008). A region is a node in a tree of declarative profiles
(`regions/<path>/region.toml`); a deployment targets a node
(`ITER_REGION=italy/lazio/rome`) and `iter-region` resolves the chain root→leaf
into one effective config: the decoupled extents, geocoding, live-trains
provider, feeds, and overlays. Data is placed by **service area, not operator** —
the all-Italy basemap + geocoding + ViaggiaTreno boards at the `italy` root,
COTRAL/COTRAL-FERRO/FL at `lazio`, ATAC + overlays at `rome`. The pipeline,
gateway, and worker consume the resolved config and stay region-generic, so
adding a region (Milan, Paris, all of Europe) is config + data, no recompile.

## Scaling model

The design follows the **build/serve asymmetry** and **scaling asymmetry** of a
data-heavy backend:

- **Build ≠ serve.** Data-prep is a multi-GB, CPU-saturating, minutes-long peak
  that exits; serving is light and runs forever. They never run together, so
  builds live in `iter-pipeline` (a one-shot/cron tier) and ship **read-only
  artifacts** to the serving tier.
- **Stateless wide, stateful narrow.** The edge holds no dataset → scale it
  wide for throughput. OTP/Photon load their whole dataset into each instance →
  keep them **narrow** (a replica is for HA, not throughput, because it
  duplicates the dataset).
- **Artifacts are externalized and regenerable.** Nothing the backend stores is
  irreplaceable (P7); "delete and rebuild" is always valid. On one host the
  artifact tree is a shared mount; on a fleet it is baked into versioned images.

This is why the same code runs as a single `podman compose` stack **and** scales
to Kubernetes replicas + a worker tier: statelessness + externalized state +
graceful drain + readiness gating are designed in, not bolted on.

> **A deliberate evolution.** The proof-of-concept's design fixed "single host,
> no Kubernetes" (its P2). This rebuild keeps single-host "clone + up" as the
> default *and* makes the code K8s-ready (stateless replicas, worker tier,
> blue/green-friendly artifact delivery). The advanced orchestration manifests
> themselves live in a separate `iter-maps/deploy` repo, so single-host stays
> the default and orchestration is opt-in.

## Configuration

Entirely environment-driven (`.env` for "clone + up"); no host-side state. The
active region (`ITER_REGION`) selects a node in the region tree, whose resolved
profile supplies the decoupled extents (basemap vs routing vs overlay), feeds,
geocoding, and overlays — so the map can go nationwide while routing stays
scoped. The pipeline's per-step `FORCE_*`/`SKIP_*` knobs make refresh granular,
and env knobs (e.g. `PMTILES_BOUNDS`) can override a profile value for a small
host.

## Invariants honored

- **No commercial keys** — every tile/geocode/route comes from open,
  self-hostable data and runtimes.
- **No proxy/TLS/host-ports in the deployment** — an external proxy owns those;
  the stack exposes no host ports in production (a dev override publishes them).
- **Zero-touch idempotent setup** — `clone + up`, every step skip-if-present.
- **No persistent user state** — the server is stateless; personalization
  arrives as request params. (A future opt-in, end-to-end-encrypted sync blob
  store is the one scoped exception — it holds only opaque ciphertext.)
- **Host-agnostic artifacts** — styles/bundles carry the literal `__BASE_URL__`
  placeholder, rewritten per-request online and to `file://` offline.

## Roadmap

The external-engine integration (OTP graph build, Photon import, planetiler
render, overlay geometry, FL NeTEx→GTFS) and the planned capabilities
(personalized planning, place discovery, traffic, crowd telemetry, reliability
archive, Italy/Europe scaling) are tracked in [`roadmap/`](roadmap/), each linked
to its design source.
