# Road-traffic data layer (PLANNED)

Open-first road-traffic awareness: ingest DATEX II situations, normalize to an
internal model, and feed the personalized-planning road/car factor.

- **Plugs into:** a pipeline/worker DATEX II ingest+normalize step → cached and
  read by the gateway reranker (soft scoring of road/car legs). A `DataOverlay`
  hook into OTP route discovery is documented but explicitly **not** first build.
- **Data deps:** open DATEX II situations from CCISS (keyless default; provisioning
  is a B2B agreement, not instant); Rome-specific Luceverde pending a
  machine-access investigation; opt-in commercial flow (TomTom/HERE) layered
  on top.
- **Cache key:** (grid-cell × situation-id × validity-window); multi-layer
  geographic stacking.

Decision: ADR 0015
