# 0036 — Per-segment local weather from multi-point trip sampling

- **Status:** Accepted
- **Date:** 2026-06-29
- **Supersedes:** —
- **Superseded by:** —

## Context

The weather rerank factor (ADR 0033, refined by ADR 0034 and 0035) samples
**one** forecast — at the journey origin, at the origin's hour — and applies it to
every segment of every itinerary. That single-point model cannot represent two
real facts about a multi-leg trip:

- **Weather is local.** A cross-town transfer hub can be in a different ~1 km cell
  than the origin, and a destination further still. A heatwave baking the transfer
  point, or rain at the arrival, is invisible to a forecast read only at the
  origin — yet that is exactly where the exposed waiting and the final walk happen.
- **Weather is time-varying.** A leg an hour or two after departure sees a
  different hour's forecast; a multi-day trip a different day's. Reading only the
  origin's hour mis-times every later segment.

The per-mode exposure split (ADR 0035) already attributes exposure to each
segment — each walk leg, each outdoor wait/transfer, each in-vehicle ride. What it
lacked was a *local* forecast for each of those segments. The factor's contract
must not change: it stays opt-in, default-off, fail-soft, pure/panic-free in its
scoring core, neutral when no forecast is available, adds **no** dependency, sends
only coarse (~1 km) coordinates, and feeds the same single `0.0..=1.0` weather
factor the composite (ADR 0028) consumes. The Open-Meteo client stays in the
gateway — a per-region weather driver was considered and rejected (it was reverted
before this change). The fetch must stay bounded: a trip must not fan out into an
unbounded number of forecast requests.

## Decision

We will sample each exposed segment against the forecast **local to its own place
and time**, resolved in **one** bounded multi-location request:

- **A sample point per segment.** Each exposure segment is keyed by a `SampleKey`
  of `(quantized coarse-lat, coarse-lon, hour-of-day, UTC-date)` derived from that
  segment's `from` coordinates and its own absolute time — so a later transfer keys
  a later hour, a destination keys the arrival cell, and a multi-day trip keys the
  right day. The same walk over the legs drives both the penalty and the set of
  points to fetch, so they never disagree on where/when a segment is sampled.
- **One multi-location fetch over the distinct points.** The journey's distinct
  sample points (the deduplicated union across all itineraries, capped at a small
  `MAX_SAMPLE_POINTS`) are fetched in a single Open-Meteo request with
  comma-separated `latitude`/`longitude` lists, bounded to `[min, max]` of the
  points' dates with `timezone=UTC`. Open-Meteo returns a parallel array, one
  object per point; each point's own hour indexes its row.
- **Per-point cache keyed by `(cell, hour, date)`.** The TTL cache shares the
  `SampleKey` space, so a fully-cached journey makes **zero** calls and a partially
  cached one fetches **only** the cache-miss points. The lock is never held across
  the fetch `.await`.
- **Apparent temperature drives felt comfort, folding UV and wind.** The forecast
  now carries `apparent_temperature` (preferred over raw air temperature for the
  thermal badness, falling back to it when absent) plus `uv_index` and
  `wind_speed_10m`. UV and wind are secondary nudges into the felt-comfort
  (temperature) badness bucket — a long sunny outdoor wait burns, wind drives
  windchill / sideways rain — capped so they never overrule a real cold/heat
  extreme. Each is optional: an older/partial body without them still scores.
- **Same factor, same fail-soft.** The penalty is the per-segment sum
  `Σ (precip_badness_local × outdoor-minutes + temp_badness_local × temp-minutes)`,
  folded into the same min-max-normalized `0.0..=1.0` weather factor. A segment
  whose local forecast is missing from the resolved map — a point that failed to
  resolve, or a short/partial response — is simply neutral; the rest still score.

The single-origin form (`weather_penalty` over one `Forecast`) is kept for the
continuity contract: a single-leg trip sampled at its origin scores the same under
the per-segment model as under the prior single-point one.

## Consequences

- **Local weather now reorders correctly.** A hot transfer point penalizes that
  transfer's segment, not the origin; rain at the destination penalizes the final
  walk. Two itineraries that differ only in where their final walk ends separate
  under the destination-local forecast.
- **Per-leg timing is honored.** A later leg reads a later hour, and a multi-day
  trip the right day, instead of every segment sharing the origin's hour.
- **Call volume stays low.** The request is bounded by the point cap and the
  journey's date range, and transfer hubs are hot cache cells — many journeys share
  the same coarse transfer cell and hour — so the per-point cache absorbs most of
  the volume after warm-up.
- **The fetch is one request, not N.** A multi-leg, multi-cell trip still makes a
  single outbound call (or none, when fully cached), so the per-trip outbound cost
  is unchanged in shape from the single-point model.
- **The covered/indoor-transfer refinement still needs station topology and stays
  deferred.** Sampling a transfer's *own* cell sharpens the locality, but it still
  treats every wait/transfer gap as outdoor; knowing a metro transfer wait is under
  cover needs station-topology data the project has not built yet. This remains the
  known gap, tracked in the personalized-planning roadmap.
- **UV/wind are heuristic comfort nudges.** Like the temperature coefficients (ADR
  0035) they are deliberate, unmeasured pedestrian-comfort thresholds, only ever
  compared relatively within one response; retuning them is a non-breaking change.

## Alternatives considered

- **Keep a single origin sample point (the ADR 0033/0034/0035 model).** Rejected —
  it cannot see a hot transfer or a rainy destination, and mis-times every later
  leg, which is the whole point of the refinement.
- **One request per sample point.** Rejected — N round-trips per trip is the
  unbounded fan-out we explicitly avoid; Open-Meteo's multi-location request gives
  the same data in one call, and the per-point cache de-duplicates across trips.
- **A per-region weather driver instead of the gateway client.** Rejected (and
  reverted) — it duplicates the keyless, opt-in, fail-soft client per region for no
  gain; the weather egress posture is a single gateway concern.
- **Raw air temperature instead of feels-like.** Rejected — apparent temperature is
  what a pedestrian actually feels (it already folds humidity/wind/sun), so it is
  the right driver for a comfort penalty; raw temp remains only as the fallback when
  the upstream omits the apparent value.
