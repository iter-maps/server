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
- **Status:** wave-1 **reliability** factor started — opt-in
  `POST /otp/gtfs/v1?rerank=reliability` stably reorders itineraries by a Tier-2
  on-time score, reorder-not-prune and fail-soft (ADR 0026). Remaining wave-1
  factors and the weighted blend are still to come.
