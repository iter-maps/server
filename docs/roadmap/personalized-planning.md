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
  external runtime data dependency: a keyless Open-Meteo forecast for the journey's
  coarse (~1 km) origin, opt-in and default-off (`WEATHER_API_URL`), short-timeout,
  TTL-cached, and fail-soft/neutral-on-failure — it scores an itinerary's
  weather-exposed minutes (walking + outdoor waiting) against precipitation and
  temperature extremes so bad-weather + high-exposure journeys rank lower.
  **Remaining wave-1 factors:** crowding and covered-transfer scoring, plus
  per-factor explanations. The carbon and weather constants are estimates and the
  weights are unmeasured; learned/client-tuned weights are deferred to waves 2–3.
