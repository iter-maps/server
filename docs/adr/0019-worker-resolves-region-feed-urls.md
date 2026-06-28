# 0019 — The worker resolves its region; feeds carry their source URLs

- **Status:** Accepted
- **Date:** 2026-06-28
- **Supersedes:** —
- **Superseded by:** —

## Context

ADR 0008 said the pipeline, gateway, **and worker** stay region-generic and
consume the resolved config; ADR 0013 made the gateway resolve `iter-region` at
startup, and the pipeline already does. The worker is the gap: it builds a static
job set in `main.rs` from env defaults, with two region URLs hardcoded in generic
code — the ATAC GTFS-RT feed (`rt_reliability.rs`) and the CCISS NeTEx asset
(`main.rs`). The feed schema carries `id`/`source`/`realtime` but not the source
URL, so those URLs had nowhere to live but code.

## Decision

The worker **resolves `iter-region` at startup** (`REGIONS_DIR` + `ITER_REGION`,
like the gateway and pipeline) and **derives its jobs from the resolved feeds**:

- the feed schema gains the source URLs it was missing — a `url` for a
  `source="netex"` feed, and a URL per realtime channel for a feed that declares
  realtime;
- one NeTEx→GTFS job per `netex` feed (using its `url`), one RT-reliability job
  per feed declaring a `trip-updates` channel (using that channel's URL);
- env overrides still win where they exist.

The hardcoded `romamobilita`/`cciss` URLs leave the worker; they become
`region.toml` feed data.

## Consequences

- The worker joins the other two tiers as region-driven (fulfils ADR 0008); the
  last region URLs leave generic code.
- Adding a region's feeds — including their URLs — is pure `region.toml` data; a
  multi-feed region just yields more jobs, no code change.
- The worker gains the `iter-region` dependency and the `regions/` tree at runtime
  (the same small cost ADR 0013 accepted for the gateway).
- This evolves only the **URL sourcing** of ADR 0015 (RT ingestion) and ADR 0016
  (FL converter); their job internals — the stable-tuple delay key, the streaming
  NeTEx parse — are unchanged.

## Alternatives considered

- **Keep env-only defaults** — leaves region URLs hardcoded in generic code, the
  thing this fixes.
- **A separate worker config file** — `region.toml` is already the one source of
  truth the other tiers resolve; reuse it.
