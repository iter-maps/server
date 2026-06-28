# 0020 — OTP consumes region GTFS-RT for live routing

- **Status:** Accepted
- **Date:** 2026-06-28
- **Supersedes:** —
- **Superseded by:** —

## Context

ADR 0009 built the static OTP graph in the pipeline and served it with
`--load --serve /data/graph`, but explicitly left realtime wiring as follow-on
work: journeys planned off the static graph ignore live delays, cancellations,
and alerts. OTP 2.7 consumes GTFS-RT through `router-config.json` updaters read
from the same base directory as `build-config.json`, polling each feed's `.pb`
stream on a timer. ADR 0019 moved the feed source URLs into `region.toml`, so a
feed's realtime channels now carry the very `.pb` URLs OTP needs — the data is in
the resolved region, not in code. The Rome ATAC feed currently publishes a url
only on its `trip-updates` channel; `vehicle-positions` and `service-alerts` are
declared without one.

## Decision

We will add a **ROUTER_CONFIG** pipeline step that writes
`graph/router-config.json` from the resolved region, mirroring how BUILD_CONFIG
writes `build-config.json`. One updater per enabled feed realtime channel that
declares a url, keyed by the feed id BUILD_CONFIG already pins:

- `trip-updates` → `stop-time-updater` with `fuzzyTripMatching`, `frequency` 30s;
- `vehicle-positions` → `vehicle-positions`, 30s;
- `service-alerts` → `real-time-alerts`, 60s.

A channel without a url emits no updater; a region with no realtime urls gets an
empty `updaters` array, which OTP loads cleanly. OTP's existing
`--load --serve /data/graph` reads `router-config.json` from that dir
automatically, so the served graph polls the feeds and journeys reflect live
delays — no compose flag or entrypoint change. The static graph-build path is
unchanged.

## Consequences

- Routing is live: plans pick up ATAC trip-update delays as soon as OTP serves,
  with no request-time cost beyond OTP's own poll loop.
- The on-disk contract for `/data/graph` gains `router-config.json` alongside
  `build-config.json`; the compose OTP service now depends on it.
- The updater set tracks `region.toml`: declaring a url on ATAC's
  `vehicle-positions`/`service-alerts`, or adding a realtime feed elsewhere,
  yields more updaters with no code change — but a wrong/stale `.pb` url surfaces
  only as OTP poll-failure logs at serve time, not at build.
- `fuzzyTripMatching` is on for trip-updates because the ATAC realtime trip ids
  do not always match the static feed exactly; it costs some matching precision.

## Alternatives considered

- **Leave realtime to the worker's RT-reliability job (ADR 0015) alone** — that
  job archives delay events for historical reliability; it does not feed OTP, so
  journeys would stay static. The two consume the same feed for different ends.
- **Hardcode the ATAC updater in a static `router-config.json`** — re-introduces
  the region URL in code that ADR 0019 just removed; the resolved feed already
  carries the url.
- **Pass updaters via a CLI flag / separate config path** — OTP already
  auto-reads `router-config.json` from the base dir; reusing that path matches
  BUILD_CONFIG and needs no compose change.
