# 0033 — Server-side Open-Meteo weather factor for the composite reranker

- **Status:** Accepted
- **Date:** 2026-06-29
- **Supersedes:** —
- **Superseded by:** —

## Context

The composite reranker (ADR 0028) scores OTP itineraries by a weighted blend of
pure, I/O-free soft factors — reliability, transfers, walking effort, eco/carbon
— each derived from the buffered plan alone. Every factor so far is computable
from data already inside the gateway: the plan body and the worker-written Tier-2
archive. The reranker has therefore never made an outbound runtime call.

Weather is the next planned wave-1 factor: a rainy or freezing journey with long
exposed (walking / outdoor-waiting) stretches is worse than a sheltered one of
the same duration, and travelers reasonably want that reflected. Unlike the
existing factors, weather is **not** derivable from local data — it requires a
forecast for the journey's place and time. Open-Meteo publishes a keyless,
no-commercial-key forecast API that fits the project's no-paid-keys posture.

Adding it makes the gateway depend on an external service at request time for the
first time. That crosses several invariants the project holds: routing must never
regress or stall on the default path; the opt-in post-processing path must stay
fail-soft and panic-free; the server is stateless; and we must not leak precise
user journey coordinates to third parties.

## Decision

We will add a **weather factor** to the composite reranker, fed by a server-side,
keyless **Open-Meteo** forecast client, with the following shape:

- **Opt-in, default-off, config-gated.** The base URL comes from
  `WEATHER_API_URL`. Unset or empty disables weather entirely: no client is
  built, no call is made, and the factor is neutral — so existing behaviour is
  byte-for-byte unchanged unless an operator configures it.
- **Pure factor.** A leg-and-forecast function computes the itinerary's
  weather-*exposed* minutes (walk-leg durations plus outdoor wait/transfer gaps
  between legs) times a `weather_badness` in `0.0..=1.0` derived from
  precipitation and apparent-temperature extremes via documented thresholds. The
  result is a penalty (higher is worse) min-max-normalized across the response
  like every other factor, so bad weather + high exposure ranks lower; good
  weather or zero exposure contributes nothing.
- **Weighted into the relevant profiles.** `balanced` and `comfort` carry a
  weather weight (comfort the heavier, since it optimizes felt experience); `eco`
  a small one; `reliability` keeps weight `0` so the wave-1 contract is intact.
  Because the factor is neutral whenever no forecast is available, a configured
  weight changes ordering only when weather is enabled *and* resolved.
- **Short-timeout, fail-soft, TTL-cached.** The client reuses the shared pooled
  reqwest client with a ~2 s timeout. Any transport error, timeout, non-success
  status, or unparsable body yields a neutral factor; the rerank still completes
  and the routing response is never blocked, failed, or stalled. Forecasts are
  memoized in a bounded in-memory TTL cache (~1 h) keyed by coarse
  `(lat, lon, hour)`, mirroring the Tier-2 read cache's lock discipline (the lock
  is never held across the fetch await).
- **Coarse coordinates.** The journey's origin is rounded to two decimals
  (~1 km) before it is sent to Open-Meteo or used as a cache key. Weather is the
  same across such a cell, so this costs nothing in quality and bounds what
  leaves the server.

## Consequences

- The gateway gains its **first external runtime data dependency**, on the opt-in
  rerank path only. That adds latency and a new failure surface — both bounded by
  the short timeout, the TTL cache, the fail-soft/neutral-on-failure contract, and
  the default-off gate. The default routing path is untouched and makes no call.
- A **privacy consideration**: coarse journey-origin coordinates leave to a
  third party on the opt-in path. Coarsening to ~1 km is the mitigation; the
  precise coordinates never leave the server.
- This sets the **posture and pattern** for future external runtime factors
  (traffic, crowding): config-gated, short-timeout, cached, fail-soft, coarse
  inputs. New factors should follow it rather than inventing their own.
- The badness thresholds and the per-profile weather weights are deliberate,
  unmeasured estimates; like the carbon constants they are only ever compared
  relatively within one response, and tuning them later is a non-breaking change.

## Alternatives considered

- **Client fetches the weather and passes it as a request parameter.** Strictly
  more private (no coordinates leave to the server at all) and keeps the server
  callless. Noted as a viable future option, but rejected for wave 1: it pushes
  the dependency and caching into every client and makes the rerank no longer
  self-contained in the gateway. We keep the rerank gateway-centric and
  consistently cached now, and may add a client-supplied-forecast path later.
- **A paid/keyed weather API.** Rejected — it violates the no-commercial-keys
  invariant, for no quality the project needs over the keyless Open-Meteo data.
- **No weather factor.** Rejected — it is the chosen wave-1 feature.
