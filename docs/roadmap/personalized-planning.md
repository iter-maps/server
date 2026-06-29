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
- **Build order:** wave 1 is pure-gateway (weather + crowding + carbon +
  covered-transfer + explanations); waves 2–3 layer data-gated factors and
  learned client-side weights (server stays stateless, P7).
