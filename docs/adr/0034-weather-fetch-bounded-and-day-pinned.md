# 0034 — Weather fetch: bounded response and journey-day pinning

- **Status:** Accepted
- **Date:** 2026-06-29
- **Supersedes:** —
- **Superseded by:** —

## Context

The weather rerank factor (ADR 0033) added the gateway's first outbound runtime
data dependency: a keyless Open-Meteo forecast for the journey origin, on the
opt-in rerank path, fail-soft and short-timeout. Two gaps surfaced once it was in
place.

First, the fetch read the response body with only the 2 s timeout bounding it —
no byte ceiling. The rerank path that buffers the OTP plan already refuses an
unbounded body (it requires a within-cap advertised `Content-Length`, ADR 0032),
but the weather fetch did not mirror that discipline. A fast localhost/LAN or
compromised upstream could deliver a large per-request allocation inside the
timeout window. The factor stayed fail-soft, so this is a DoS-surface gap, not a
correctness or hang bug.

Second, the fetch sent no `&timezone`/`&start_date`, so Open-Meteo returned a
multi-day hourly array anchored at today's UTC midnight, and the factor indexed it
by hour-of-day (`0..=23`). That addresses the correct row only for a journey
departing today (UTC): a departure on a later day collapsed to the same hour of
*today*, reading the wrong forecast. The value stayed finite and bounded, so this
was a forecast-accuracy gap, not an invariant break.

## Decision

We will harden the weather fetch on the same fail-soft, neutral-on-failure
contract as ADR 0033:

- **Bound the buffered body.** Before reading, the fetch requires an advertised
  `Content-Length` within a small cap (256 KiB — a real forecast is a few KB). A
  body with no advertised length, or one over the cap, degrades to a neutral
  factor, mirroring the rerank path's bounded-buffer discipline. The buffered
  forecast body is therefore always bounded, not merely time-bounded.
- **Pin the forecast window to the journey's UTC day.** The request now carries
  `&timezone=UTC&start_date=<YYYY-MM-DD>&end_date=<YYYY-MM-DD>` derived from the
  journey's first-leg start, so the hourly array starts at that day's 00:00 UTC
  and the existing hour-of-day index selects the journey's own row — correct for
  multi-day-out and past-midnight departures, not just today. The date is derived
  by a pure civil-from-days conversion (no new crate) and is part of the cache key
  so same-hour journeys on different days do not collide. A plan without a usable
  start time sends no date (the default today-anchored window) and still resolves
  a coarse forecast.

## Consequences

- The weather fetch is now bounded in both time and bytes, closing the
  unbounded-response surface for the first outbound dependency. An oversized or
  length-less body is one more case that degrades to the neutral factor.
- Multi-day and past-midnight departures get the right forecast row. The
  hour-of-day index and the UTC window are now self-consistent by construction, so
  the earlier latent timezone imprecision is resolved for the pinned-day case.
- The cache key grows a date component; cardinality rises modestly (a cell-hour is
  now also per-day), still bounded by the wholesale-eviction cap.
- The request shape changes (extra query parameters). The endpoint is operator
  config and the parameters are standard Open-Meteo; no contract for external
  consumers is affected.

## Alternatives considered

- **Stream-limit the body with a bounded reader instead of a `Content-Length`
  check.** Equivalent safety, more code; the advertised-length check matches the
  rerank path's existing discipline and is enough for a few-KB forecast.
- **Index by absolute hours-since-`hourly.time[0]` instead of pinning the day.**
  Works, but parses the response's time array and keeps the today-anchored
  multi-day window; pinning the day keeps the hour-of-day index unchanged and the
  response smaller.
- **Accept the day-0 collapse as wave-1 imprecision.** Rejected — the fix is small
  and the wrong-day forecast is user-visible on the most common non-today case.
