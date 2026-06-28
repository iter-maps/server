# Development

## Quick start

The goal is **clone + up** — no host-side tools, no manual downloads:

```sh
git clone https://github.com/iter-maps/server
cd server
cp .env.example .env
podman compose up        # Docker Compose v2 works too
```

First boot fetches and builds every artifact (graph, index, tiles, overlays) from
public sources; later boots skip steps whose output already exists. For dev host
ports on a LAN, add `-f compose.dev.yaml`.

## Building the Rust services

```sh
cargo build --workspace
cargo test --workspace
cargo run -p iter-gateway        # serves on :8090
```

CI is strict — fmt, clippy `-D warnings`, tests, `cargo doc`, `cargo deny`, typos,
REUSE, and hadolint all gate. Run fmt + clippy + test green before committing.
See [`CONTRIBUTING.md`](CONTRIBUTING.md).

## Repo layout

```
crates/
  iter-core/           shared primitives (config, errors, health, shutdown)
  iter-contracts/      wire-contract DTOs
  iter-region/         the region model (nested profiles, resolver)
  iter-region-drivers/ per-region drivers (address, live-trains, overlays, netex)
  iter-gateway/        edge/BFF service (axum)
  iter-pipeline/       build tier — idempotent data-prep orchestrator
  iter-worker/         background jobs
docs/                  overview, architecture, decisions, roadmap
regions/               region profiles (the region.toml tree)
docker/                Dockerfile + container build
```

## Configuration

Everything is env-configured (`.env` for clone + up); see
[`.env.example`](../.env.example). Key knobs: service ports (`GATEWAY_PORT`, …),
upstream URLs (`OTP_URL`, `PHOTON_URL`), the region selector (`ITER_REGION`), the
per-host extent overrides (`ROUTING_BOUNDS`, `PMTILES_BOUNDS`), and the pipeline's
`FORCE_*`/`SKIP_*` step overrides.
