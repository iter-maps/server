# 0028 — Composite soft itinerary reranking with profiles (wave 1b)

- **Status:** Accepted
- **Date:** 2026-06-29
- **Supersedes:** —
- **Superseded by:** — (filled in if this is ever replaced)

## Context

ADR 0026 added an opt-in, default-off, fail-soft soft reranker after the OTP
routing proxy: with `?rerank=reliability` the gateway buffers the plan, scores
each itinerary by a Tier-2 on-time signal, and stably reorders the itineraries.
It explicitly anticipated later waves layering "more soft factors … into the same
post-proxy stage and the same pure core", with the score becoming a weighted
blend. ADR 0027 then fixed the id-space mismatch so reliability actually fires on
production data.

Reliability alone is a thin notion of "better". A rider also cares about how many
times they change vehicles, how far they walk, and the carbon cost of the trip —
all of which are pure functions of the itinerary OTP already returns (it carries
per-leg `mode`, `distance`, and `duration`). The wave-1b task is to generalize the
score from reliability-only to a composite of these factors, without weakening any
of ADR 0026's invariants: soft (reorder-not-prune), opt-in (default byte-for-byte
passthrough), fail-soft (never 500/panic/drop), and a pure I/O-free core.

The open design questions: which factors; how to combine factors that live on
different scales and point in different directions; how the opt-in flag should
select a weighting; and how to keep the existing `rerank=reliability` contract
intact.

## Decision

We will generalize the reranker to a **weighted composite of independent soft
factors**, selected by a **named profile** on the existing opt-in flag, all within
ADR 0026's soft/opt-in/fail-soft frame.

**Factor set.** Each factor is a total, pure function over one itinerary's legs:

1. **reliability** — the ADR 0026 mean on-time rate across transit legs with
   Tier-2 history; neutral (`0.5`) when there is none. Higher is better.
2. **transfers** — the number of transit boardings. Fewer is better. (Boardings,
   not boardings-minus-one: it is monotone in transfers, which is all the ordering
   needs.)
3. **walking effort** — total `WALK`-leg duration in seconds. Less is better,
   carried at a gentle weight.
4. **eco / carbon** — per-mode gCO2e/passenger-km intensity × leg distance,
   summed over all legs. Lower is better.

**Carbon constants + methodology.** We hard-code per-mode intensities as named
consts (`CO2_ACTIVE = 0`, electrified `RAIL`/`SUBWAY`/`TRAM` low, `BUS` higher,
`FERRY`/`CAR` highest, an unknown-motorized mid fallback). These are typical
published *operational* greenhouse-gas intensities per passenger-kilometre,
order-of-magnitude figures consistent with widely reported transport-emission
factors (national environment-agency and European passenger-transport factors).
They are deliberate documented estimates, not a regional measurement; because the
reranker only ever compares carbon **relatively within one response**, the exact
values matter far less than their ordering.

**Normalization + weighting.** Each raw factor is **min-max normalized across the
itineraries in this one response** into a benefit in `0..=1` where higher is always
better (`LowerBetter` factors are flipped). Min-max — not rank — so the *magnitude*
of a difference survives: two near-identical itineraries score near-identically,
not a full rank apart. When a factor's values are all equal (or there is a single
itinerary) its spread is zero, every benefit is the neutral `0.5`, and that factor
cannot reorder anything; the stable sort then preserves OTP's order. The composite
is the weighted sum of the four benefits using per-profile weight consts; the
itineraries are stable-sorted by descending composite, original index breaking
ties.

**Profile API.** The opt-in flag selects a named profile, `?rerank=<profile>`:

- `reliability` — the ADR 0026 contract, exactly: weights `(1,0,0,0)`, so only the
  reliability factor matters and the order matches wave 1 byte-for-byte.
- `balanced` — the full composite, reliability-led with transfers/walk/eco mixed in.
- `eco` — carbon dominates; the rest break near-ties.
- `comfort` — fewer transfers and less walking dominate.

An unknown profile parses to `None`, so the handler treats it as "no rerank" and
stays the default passthrough. Profiles (not a free-form weight vector in the
query) keep the wire contract small and the behavior testable.

**Schema.** The lone additive change stays additive: alongside the existing
`reliabilityScore` (the raw reliability factor, kept for clients already reading
it) each reranked itinerary gains an optional numeric `rerankScore` (the
composite). No leg/feasibility data changes; no itinerary is dropped.

**Structure.** The pure I/O-free core (`rerank::rerank_plan`: parsed plan + lookup
closure + profile → reordered value) still owns all scoring; each factor and the
min-max combiner are total functions unit-tested on synthetic plans. The handler
remains the thin wiring — profile parse, buffering, building the Tier-2 lookup.

## Consequences

- There are now four factors and four profiles to tune. The weight consts are a
  starting point, not a measured optimum; revisiting them is a code+ADR change, not
  a config surface (kept deliberately closed for now).
- The carbon intensities are an estimate. They order modes correctly (active <
  electrified rail/metro/tram < bus < car) but are not a regional inventory; a
  region wanting accurate absolute carbon would need real per-mode factors. The
  relative-only use bounds the blast radius of the estimate.
- Normalization is **per-response and relative**: a score means "best among these
  itineraries", not an absolute rating. The same itinerary in a different result
  set can score differently. This is intended (we only ever rank within a response)
  but is a non-obvious property worth stating.
- The `rerankScore` field joins `reliabilityScore` on reranked itineraries; both
  are documented additive/optional so schema-strict clients are unaffected.
- All of ADR 0026's invariants carry forward unchanged: default path is an
  untouched passthrough, the reranker never prunes/500s/panics, and a single
  itinerary or an all-equal set is never reordered.

## Alternatives considered

- **A single global weight vector instead of profiles** — rejected: a free-form
  `?w_eco=…&w_walk=…` surface is larger to validate, harder to document, and
  invites nonsensical combinations. Named profiles cover the real intents (green,
  comfortable, reliable, balanced) with a tiny, testable contract.
- **Absolute (fixed-scale) normalization instead of per-response min-max** —
  rejected for now: absolute thresholds (e.g. "30 min walk = 0") need defensible
  regional calibration we don't have, and the reranker only compares within one
  response anyway. Min-max needs no magic constants and degrades cleanly to "no
  reorder" when a factor doesn't vary.
- **Rank-based normalization instead of min-max** — rejected: collapsing each
  factor to its rank discards how *close* two itineraries are, over-reacting to a
  trivial difference. Min-max preserves magnitude.
- **Learned / client-tuned weights** — deferred to a later wave: the server stays
  stateless (P7), so any learning lives client-side and feeds weights in later;
  out of scope here.
- **Expose raw per-factor scores on the wire** — deferred: only the composite plus
  the existing reliability factor are surfaced; a full factor breakdown (for
  explanations) is a later wave's annotation step.
