# 0024 — Reliability read endpoint over the shared Tier-2 archive

- **Status:** Accepted
- **Date:** 2026-06-29
- **Supersedes:** —
- **Superseded by:** —

## Context

ADR 0022 landed the persistent reliability rollup tier in the worker and left
the **read side** — a gateway endpoint / reranker input over Tier-2 — as a
tracked gap. The metric shape (`Readout`: p50/p85/p90 delay seconds, on-time
rate, sample count) and a logged Tier-2 overview existed, but nothing served the
archive over HTTP.

The constraint that shapes this decision is the crate boundary. The gateway
(`iter-gateway`, ADR 0004) is the stateless BFF and must stay **independent of
the worker** — they are separate tiers with separate lifecycles, and the gateway
must not pull in the worker's RT decoding, job scheduler, or write path to read a
few aggregates. Yet both tiers need to agree, byte-for-byte, on the on-disk
layout: the worker writes `tier2.json`, the gateway reads it. The pure rollup
core (`Histogram`, `Tier1`/`Tier2`, `Readout`, the bucket/day-type calendar) was
already I/O-free; the path-safety discipline (`sanitize_token`) and the Tier-2
key shape lived in the worker's store adapter.

`route`/`direction`/`stop` arrive as external HTTP path params, so the read must
be fail-soft and path-safe: a request must never traverse outside the
reliability dir, never 500, never panic, and never load an unbounded file.

## Decision

We will **extract the reliability read side into `iter-core`** and serve it from
the gateway, with the on-disk layout as a shared contract:

- **Shared core.** `iter_core::reliability` holds the pure rollup algebra
  (`rollup`: `Histogram`, `Tier1`/`Tier2`, `Readout`, `TodBucket`/`DayType` and
  their token round-trips, the Italian-holiday calendar, the on-time window) plus
  the read adapter (`store_read`): `sanitize_token`, the `tier2.json` filename
  and key shape, a size-bounded fail-soft `read_tier2_map`, and the
  `read_tier2_cells` / `read_tier2_readout` lookups. Both tiers depend on
  `iter-core` already, so neither depends on the other. The worker keeps its full
  write path and re-exports the moved types unchanged (`crate::reliability::
  rollup::*`), so its callers and tests are untouched.
- **Endpoint.** `GET /reliability/{route}/{direction}/{stop}` returns `200` with
  `ReliabilityResponse` — the echoed query tuple plus a `cells` array, one
  `ReliabilityCell` per stored (tod_bucket, day_type) slice for that stop
  (`todBucket`, `dayType`, `sampleCount`, `onTimeRate`, `p50S`/`p85S`/`p90S`,
  `meanS`). This "all slices" view is self-describing: the client picks the cell
  matching the trip's time-of-day and day-type, and `sampleCount` lets it gate
  low-confidence cells.
- **Wire contract.** The DTO lives in `iter-contracts` (`reliability`),
  camelCase like every other surface; delay figures are seconds, negative is
  early.
- **Fail-soft posture.** Consistent with the overlays handler: an absent key, a
  missing store, a corrupt store, an oversized store, or a non-integer direction
  all yield `200` with an empty `cells` list — never `404`, `500`, or a panic.
  The client reads empty cells as "no history yet" and falls back to
  schedule-only ranking.
- **Path-safety + bounds.** The path params are sanitized through the same
  `sanitize_token` the writer uses, and the result is a flat **map key**, never a
  filesystem path — a `../../` param can only ever miss, never traverse. The
  Tier-2 file read is size-capped (16 MiB) so a hostile or damaged store can't
  exhaust memory.
- **Config.** The gateway gains `RELIABILITY_DIR` (default `<DATA_DIR>/
  reliability`), mirroring the worker's knob so both point at the same tree.

## Consequences

- The gateway now **reads worker-written state**. The reliability on-disk layout
  (the `tier2.json` filename, the sanitized key shape, the JSON schema) is a
  **shared contract** owned by `iter_core::reliability::store_read`; changing it
  means migrating both tiers, not one. This is the price of letting them share a
  reader without a tier dependency.
- A new public surface and a new env knob (`RELIABILITY_DIR`) the gateway must
  document and keep in sync with the worker.
- The endpoint is read-only and best-effort: it never blocks on the worker and
  degrades to empty when there is no archive, so a never-run worker or a wiped
  volume costs only history, not gateway availability.
- The reranker input is now a stable HTTP shape; a future in-process reranker can
  read the same `iter-core` functions directly instead of going over HTTP.

## Alternatives considered

- **Gateway depends on `iter-worker`** — the gateway would pull in RT decoding,
  the job scheduler, and the write path to read a JSON map. Inverts the tier
  boundary (ADR 0004) and bloats the BFF. Rejected.
- **Duplicate the reader in the gateway** — copy `sanitize_token`, the key
  shape, and the Tier-2 struct into the gateway. Two owners of one on-disk
  contract drift silently; a sanitizer or key change in one tier corrupts reads
  in the other. Rejected.
- **Move the read types into `iter-contracts`** — the contracts crate is wire
  DTOs, not filesystem logic; a path-safe disk reader doesn't belong there. The
  camelCase DTO does live there; the reader lives in `iter-core`. Rejected for
  the reader, adopted for the DTO.
- **Per-cell endpoint (`?tod=…&day=…`)** — serve one resolved slice instead of
  all of them. Pushes day-type/holiday derivation to the client and costs a round
  trip per slice; the whole-stop view is small (Tier-2 is tiny) and lets the
  client choose. Rejected; `read_tier2_readout` is kept for a future in-process
  reranker that wants a single slice.
