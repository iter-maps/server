# 0035 — Mode-aware weather exposure in the rerank weather factor

- **Status:** Accepted
- **Date:** 2026-06-29
- **Supersedes:** —
- **Superseded by:** —

## Context

The weather rerank factor (ADR 0033) scores an itinerary by its weather-*exposed*
minutes times a single `weather_badness` derived from both precipitation and
temperature extremes. That model treats every kind of bad weather the same way
and treats in-vehicle time as uniformly sheltered. Two facts it cannot represent:

- **Rain and temperature behave differently.** A vehicle keeps the rain off
  completely — a bus rider is as dry as a metro rider — but it does not keep the
  *temperature* off equally. A bus with weak or no air conditioning is hot on a
  hot day; an air-conditioned metro is not. Lumping the two into one exposure
  bucket cannot tell a rainy walk apart from a hot bus.
- **In-vehicle comfort is mode-dependent.** The prior model counted no in-vehicle
  time as exposure at all, so on a sweltering day a long bus ride and an equal
  metro ride scored identically — even though the bus is the worse ride.

The factor's contract must not change: it stays opt-in, default-off, fail-soft,
pure and panic-free, neutral when no forecast is available, adds no dependency,
and feeds the same single `0.0..=1.0` weather factor the composite (ADR 0028)
already consumes. The refinement is internal to how the penalty is computed.

## Decision

We will split the weather penalty by weather *type* and make in-vehicle exposure
mode-aware, keeping the same external factor contract:

- **Two badness components.** The forecast exposes `precip_badness` and
  `temp_badness` separately (the combined `badness` remains as a single-hour
  summary). Each is the same documented threshold ramp as before, just no longer
  pre-folded.
- **Precipitation hits truly-outdoor time only.** Precip exposure is walk legs
  plus outdoor wait/transfer gaps. In-vehicle time is rain-sheltered for **every**
  mode — a bus keeps rain off as well as a train does.
- **Temperature also hits in-vehicle time, scaled per mode.** Temperature
  exposure is the same outdoor time *plus*, for each transit leg, its in-vehicle
  minutes times a per-mode climate-control coefficient: air-conditioned
  rail/metro `~0.1` (near-sheltered), bus/tram `~0.4` (frequently weak/no A/C),
  and a conservative `~0.3` default for unrecognized modes.
- **Combine into the same factor.**
  `weather_penalty = precip_badness × precip_exposure + temp_badness × temp_exposure`,
  in minutes, folded into the same min-max-normalized `0.0..=1.0` weather factor
  with the same additive-field behaviour. The penalty is `0.0` when both badness
  components are `0.0` or exposure is zero, so the factor stays neutral.

The coefficients are **named, tunable consts with a heuristic doc comment**: we
have no per-vehicle A/C data, so they encode the typical fleet reality (metro/rail
reliably climate-controlled, many buses/trams not), and — like the carbon and
badness constants — they are only ever compared relatively within one response.

## Consequences

- **Rain now favors any in-vehicle route, bus included.** A sheltered ride takes
  no rain penalty, so on a wet day a route that rides rather than walks ranks
  ahead regardless of mode.
- **Heat now favors metro/rail over an equivalent bus.** Two otherwise-equal
  routes differing only in mode separate under temperature exposure: the bus
  cabin is scored hotter than the air-conditioned metro.
- **Mild/dry weather is unchanged.** With both badness components zero the penalty
  is zero for every itinerary, exactly as before; the factor neither bites nor
  reorders.
- **The coefficients are a heuristic, not data-driven.** They are deliberate,
  unmeasured estimates and tunable; retuning them is a non-breaking change. A
  real per-vehicle A/C signal would replace them later.
- **The indoor/covered-transfer refinement stays deferred.** Treating a covered
  metro transfer wait as not-outdoor needs station-topology data the project has
  not built yet, so all wait/transfer gaps still count as outdoor. This remains
  the known gap, tracked in the personalized-planning roadmap.

## Alternatives considered

- **Keep a single undifferentiated exposure bucket (the ADR 0033 model).**
  Rejected — it cannot tell a rainy walk from a hot bus, which is the whole point
  of the refinement.
- **Use per-vehicle A/C data instead of a per-mode heuristic.** Rejected for now —
  we do not have that data; the per-mode coefficient is the best available proxy
  and is a non-breaking thing to replace later.
- **Client-supplied comfort preferences (e.g. "I run hot").** Out of scope — it
  belongs to the deferred learned/client-tuned weights (waves 2–3), not this
  factor's internal model.
</content>
</invoke>
