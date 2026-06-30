# Personalized & context-aware trip planning (PLANNED)

Rank itineraries by weather, comfort, cost, eco-impact and accessibility
**without forking OTP** — the gateway shapes OTP *inputs* and reranks its
*outputs*.

- **Plugs into:** gateway / BFF, as post-processing over the OTP `planTrip`
  response (stages 0–7: profile resolve → param shaping → enrichment → scoring →
  annotation → cache). No engine fork.
- **Data deps:** Open-Meteo (weather), GTFS-RT occupancy + crowd telemetry
  (crowding), per-mode carbon, the reliability archive (Tier-2), DATEX II
  (traffic). Wave-2+ pipeline gates: DEM (slope/stairs), Fares V2, GBFS, OTP
  `DataOverlay` NetCDF grids.
- **Build order:** wave 1 is pure-gateway (reliability + weather + crowding +
  carbon + covered-transfer + explanations); waves 2–3 layer data-gated factors
  and learned client-side weights (server stays stateless, P7).
- **Status:** wave-1 **composite scoring** built — opt-in
  `POST /otp/gtfs/v1?rerank=<profile>` stably reorders itineraries by a weighted
  blend of pure soft factors (reliability + transfers + walking effort +
  eco/carbon + weather), reorder-not-prune and fail-soft (ADR 0026, 0027, 0028,
  0033). Named profiles select the weighting: `reliability` (the original
  Tier-2-only contract), `balanced`, `eco`, `comfort`; an unknown profile stays a
  passthrough. Each factor is min-max normalized per response and combined into an
  additive `rerankScore` (the raw reliability factor is still surfaced as
  `reliabilityScore`). The **weather factor** (ADR 0033) is the gateway's first
  external runtime data dependency: keyless Open-Meteo forecasts, opt-in and
  default-off (`WEATHER_API_URL`), short-timeout, TTL-cached, and
  fail-soft/neutral-on-failure, sent only coarse (~1 km) coordinates. It scores
  weather exposure **by type** (ADR 0035): precipitation hits truly-outdoor minutes
  only (walking + outdoor waiting — a vehicle keeps rain off for every mode), while
  temperature extremes also hit in-vehicle minutes scaled by a per-mode
  climate-control coefficient (air-conditioned rail/metro near-sheltered, bus/tram
  partially exposed). So a rainy day favors any in-vehicle route incl. a bus, and a
  hot day favors metro/rail over an equivalent bus. It now samples **per segment**
  (ADR 0036): each exposed segment — origin, transfer/boarding waits, destination,
  walk legs — reads the forecast **local to its own coarse cell and its own
  per-leg hour/day**, resolved in **one** bounded multi-location request over the
  journey's distinct points (capped, per-point cached by `(cell, hour, date)`) so a
  hot transfer or a rainy destination is scored where it actually happens.
  Apparent/feels-like temperature drives the thermal badness (folding UV and wind
  as secondary nudges). **Remaining wave-1 factors:** crowding and covered-transfer
  scoring, plus per-factor explanations. The covered/underground-transfer
  refinement (not counting a sheltered metro transfer wait as outdoor) stays
  deferred — it needs station-topology data not yet built, so every wait/transfer
  gap currently counts as outdoor even though it is now sampled in its own cell. The
  carbon and weather constants (the per-mode temperature coefficients and the
  UV/wind thresholds) are heuristic estimates and the weights are unmeasured;
  learned/client-tuned weights are deferred to waves 2–3.
