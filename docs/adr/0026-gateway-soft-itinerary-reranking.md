# 0026 — Gateway-side soft itinerary reranking (reliability, wave 1)

- **Status:** Accepted
- **Date:** 2026-06-29
- **Supersedes:** —
- **Superseded by:** — (filled in if this is ever replaced)

## Context

The gateway reverse-proxies routing to OTP (`POST /otp/gtfs/v1`) as a streaming
passthrough. OTP ranks itineraries by generalized cost; it has no notion of how
reliably a given route/stop actually runs on time, even though the worker now
maintains that history in the Tier-2 reliability archive (ADR 0022) and the
gateway already reads it (ADR 0024). The BFF is the place context-aware
reranking belongs — it can shape OTP's inputs and reorder its outputs without
forking the engine.

The hard constraint is that routing must not regress: the default request must
stay exactly what it is today. Reranking also has to be safe — it cannot drop a
feasible itinerary, change leg/feasibility data, or break the response schema the
Android client greps.

## Decision

We will add an **opt-in, default-off, fail-soft soft reranker** that sits *after*
the routing proxy response.

- **Opt-in via `?rerank=reliability`.** Without the flag the routing handler is
  the unchanged streaming passthrough — it never buffers or parses the plan, so
  existing routing cannot regress. The flag is the only trigger.
- **Reorder, never prune.** On the opt-in path the gateway buffers the OTP plan,
  scores each itinerary, and **stably reorders** the `itineraries` array by
  descending score. No itinerary is removed, and leg/feasibility data is left
  untouched. The lone additive change is a numeric `reliabilityScore` per
  itinerary — clients that grep known keys ignore it.
- **Wave-1 score is reliability only.** Each transit leg is keyed by
  `(route gtfsId, trip directionId, boarding-stop gtfsId)` and resolved to a
  count-weighted on-time rate over that stop's Tier-2 cells. An itinerary's score
  is the **mean** of its transit legs' on-time rates; walk/wait legs contribute
  nothing. An itinerary with no resolvable history scores a **neutral** 0.5 and,
  because the sort is stable, holds its original position so OTP's ranking still
  breaks ties.
- **Stateless and fail-soft everywhere.** A transport failure, a non-`200`
  upstream, a non-JSON or non-plan body, an absent/empty/corrupt reliability dir,
  or any unkeyable leg all return the original response unchanged. The reranker
  never 500s and never panics.
- **Structure.** A pure, I/O-free core (`rerank::rerank_plan`: parsed plan value
  + a lookup closure → reordered value) is fully unit-tested with synthetic plan
  JSON. The handler is the thin wiring: flag detection, buffering, and building
  the lookup over the shared Tier-2 archive (`reliability_dir`, same as ADR
  0024). The per-stop on-time index is read once per opt-in request on a blocking
  worker.

## Consequences

- The gateway now parses the OTP plan response on the opt-in path, and buffers it
  rather than streaming — a deliberate cost paid only when the flag is present.
- Leg→reliability matching depends on the leg `gtfsId`s and the feed ids the
  worker recorded sharing the same identity; a mismatch degrades silently to
  "neutral", never to an error.
- Later waves layer more soft factors (weather, crowding, carbon, covered
  transfers) into the same post-proxy stage and the same pure core; the score
  becomes a weighted blend. The reorder-not-prune, opt-in, fail-soft, stateless
  shape established here carries forward.
- An additive `reliabilityScore` field appears on reranked itineraries; it is
  documented as additive and optional so it can't break schema-strict clients.

## Alternatives considered

- **Fork OTP / score inside the engine** — rejected: couples us to engine
  internals and forfeits the no-fork posture the BFF exists to keep.
- **Prune infeasible-looking itineraries** — rejected: soft only. Dropping a
  feasible itinerary on a heuristic is a correctness hazard; we reorder.
- **Always-on reranking** — rejected: buffering+parsing every routing response
  regresses the default path. Opt-in keeps the default a byte-for-byte
  passthrough.
- **Per-leg file reads** — rejected: re-reading `tier2.json` per leg is wasteful;
  we read a per-stop on-time index once per request instead.
