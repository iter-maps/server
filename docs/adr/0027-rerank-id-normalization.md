# 0027 — Reranker normalizes OTP feed-prefixed ids to the reliability key space

- **Status:** Accepted
- **Date:** 2026-06-29
- **Supersedes:** —
- **Superseded by:** — (filled in if this is ever replaced)

## Context

The soft itinerary reranker (ADR 0026) keys each transit leg by its
`(route gtfsId, trip directionId, boarding-stop gtfsId)` and resolves that key
against the Tier-2 on-time index. ADR 0026 listed "leg gtfsIds and the worker's
recorded feed ids share the same identity" as an assumption — and that assumption
does not hold:

- OTP namespaces every `gtfsId` as `FEED:LOCALID` (e.g. `ATAC:MEA`,
  `ATAC:70001`); the `feedId` prefix is assigned when the routing graph is built.
- The reliability index is keyed by the bare local ids the worker records
  straight from GTFS-RT (`MEA`, `70001`) — no feed prefix.

So every leg lookup missed, every itinerary scored the neutral 0.5, and the
stable sort preserved OTP's order: with the flag set the reranker reordered
nothing on real data. The wave-1 unit and integration tests passed only because
their fixtures used matching synthetic ids on both sides, never exercising the
`FEED:` prefix.

## Decision

We will **strip OTP's `FEED:` namespace prefix** off the route and stop `gtfsId`
before the reliability lookup, normalizing both to the bare local id space the
worker keys by. An id with no colon is already local and passes through
unchanged; only the first colon is split on (local ids that themselves contain a
colon keep their remainder).

We also pin the **supported OTP response shape**: the reranker operates on the
legacy `data.plan.itineraries` GraphQL `plan` query — the shape the Android
client sends. A response in any other shape (e.g. the newer
`planConnection`/`edges[].node`) is not a recognized plan, so the reranker
declines and the original response streams through unchanged (fail-soft, per ADR
0026). Extending to `planConnection` is a follow-on if/when a client adopts it.

## Consequences

- The reranker now actually reorders against production id shapes; the id-identity
  assumption in ADR 0026 is resolved rather than merely noted.
- A regression test exercises `FEED:LOCAL`-prefixed plan ids against an
  unprefixed index and asserts the reorder fires, so a future divergence between
  the two id spaces fails the suite instead of silently no-op'ing.
- The normalization is intentionally minimal (first-colon split). If a region's
  feed ever uses a different namespace scheme, this is the single place to revisit.
- Pinning the legacy `plan` shape means a client that switches to
  `planConnection` silently gets passthrough (no rerank) until the navigation is
  extended — surfaced here so the gap is a known, documented one.

## Alternatives considered

- **Normalize on the worker's write side** (record `FEED:LOCAL`) — rejected: the
  worker ingests GTFS-RT, which has no feed-id namespace, and would have to learn
  the graph's `feedId`; the gateway already knows OTP's id shape and is the natural
  place to bridge.
- **Match both prefixed and unprefixed keys in the index** — rejected: doubles the
  key space and the lookup cost for no gain over a one-line prefix split.
- **Support `planConnection` now** — deferred: no client sends it yet; adding an
  unused navigation path would ship untested-against-real-traffic code.
