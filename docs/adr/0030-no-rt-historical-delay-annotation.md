# 0030 — No-live-RT historical delay annotation in the gateway

- **Status:** Accepted
- **Date:** 2026-06-29
- **Supersedes:** —
- **Superseded by:** — (filled in if this is ever replaced)

## Context

OTP returns live realtime delays on a plan leg only where a GTFS-RT feed
actually covers that trip — a leg carries `realTime: true` with
`arrivalDelay`/`departureDelay` when it is live, and nothing when it is not.
Across a region many transit legs have no live feed at all (off-peak windows,
operators or modes the feed does not cover, gaps between polls), so the rider
sees a schedule-only time with no sense of how late that service typically runs.

The worker already accrues exactly that history: the Tier-2 reliability archive
holds per `(route, direction, stop, tod-bucket, day-type)` delay distributions
(p50/p85/p90, on-time rate), and the gateway already reads it (ADR 0024) and
keys plan legs against it for soft reranking (ADR 0026/0028, with the OTP
`FEED:LOCAL`→bare-local id normalization of ADR 0027). The same archive can fill
the no-live-feed gap with a *historical* expected delay.

The constraints mirror the reranker's: routing must not regress (the default
request stays a byte-for-byte passthrough), the transform must be additive and
panic-free, and it must never override or contradict live data — live realtime is
authoritative and history only fills gaps.

## Decision

We will add an **opt-in, default-off, fail-soft historical delay annotator** that
sits in the same post-proxy stage as the reranker.

- **Opt-in via `?predict=historical`.** Without the flag the routing handler is
  the unchanged streaming passthrough. The flag composes with `?rerank=<profile>`
  on the **same single buffered plan** — when either (or both) is present the
  gateway buffers once, reusing the bounded-buffer predicate (`rerankable`): only
  a `200` advertising a `Content-Length` within the 16 MiB cap is buffered, so
  the memory bound is unchanged and shared.
- **Annotate RT-less transit legs only.** For each transit leg whose OTP
  `realTime` is not `true`, the gateway looks up its
  `(route, direction, stop)` typical delay in the Tier-2 archive (the same leg
  keying and feed-prefix strip the reranker uses, ADR 0027) and attaches an
  additive `predictedDelay` object: `seconds`, `p50Seconds`, `sampleCount`, and
  `source: "historical"`. An itinerary with at least one annotated leg also gains
  a compact additive `predictedDelaySummary`. Existing clients that grep known
  keys ignore the additive fields.
- **Surface the p85, carry the p50.** The headline `seconds` is the **85th
  percentile** delay, not the median. A rider planning around a missing live feed
  is better served by a conservative "budget at least this much" tail than by a
  median that is beaten half the time; p85 is also the conservative percentile the
  read endpoint already exposes (ADR 0024). The median rides alongside as
  `p50Seconds` for the typical case, and `sampleCount` lets a client gate out
  low-confidence cells. Per-stop figures are count-weighted across the stop's
  Tier-2 cells, so a busy slice dominates a quiet one.
- **Authoritative is the floor.** A leg with live realtime (`realTime: true`) is
  **never** annotated — history neither overrides nor contradicts live data. Leg
  times, modes, feasibility, and itinerary order are never changed; the annotation
  is purely additive.
- **Stateless and fail-soft everywhere.** A transport failure, a non-`200`
  upstream, a non-JSON or non-plan body, an absent/empty/corrupt reliability dir,
  an unkeyable leg, or a key with no history all leave the response (or that leg)
  unchanged. The annotator never 500s and never panics.
- **Structure.** A pure, I/O-free core (`annotate::annotate_plan`: parsed plan +
  a typical-delay lookup closure → annotated plan) is fully unit-tested with
  synthetic plans. Leg keying is shared with the reranker via `legkey` rather than
  re-derived. The handler is the thin wiring: flag detection, the shared buffer,
  and a per-stop typical-delay index read once per opt-in request on a blocking
  worker (`store_read::read_tier2_typical_delay_index`).

## Consequences

- On the `predict=historical` opt-in path the plan response gains additive
  `predictedDelay` (per RT-less transit leg) and `predictedDelaySummary` (per
  affected itinerary) fields; the default path is untouched.
- The estimate is **historical, not live**, and is labelled `source: "historical"`
  so a client never mistakes it for measured realtime data. Where a live feed
  exists, the live leg is left as-is and carries no prediction.
- The quality of the estimate depends on the recorder having accrued history for
  that `(route, direction, stop)`; until the archive fills, a leg simply gains no
  annotation (the same "no history yet" degradation as the read endpoint).
- The annotator reads a second Tier-2 index (typical delay) when `predict` is on,
  alongside the reranker's on-time index when `rerank` is on — two bounded reads
  on the same buffered request when both compose.

## Alternatives considered

- **Predict for all legs, including those with live RT** — rejected: live data is
  authoritative; overlaying a historical guess on a leg that already has a measured
  delay would contradict it. History fills gaps only.
- **A separate `/predict` endpoint** — rejected: the prediction is most useful
  in-context, attached to the very legs of the plan the rider is looking at; a
  detached endpoint forces the client to re-key and re-join legs itself.
- **Always-on annotation** — rejected: buffering and rewriting every routing
  response regresses the default path. Opt-in keeps the default a byte-for-byte
  passthrough, consistent with the reranker (ADR 0026).
- **Surface the median (p50) as the headline** — rejected as the headline: a
  median delay is beaten half the time, which under-prepares the rider. We lead
  with the conservative p85 and carry the p50 alongside.
