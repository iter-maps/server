# Historical reliability archive (PLANNED)

An efficient archive of past delays/cancellations (and past road traffic) that
feeds reliability ranking and prediction when no live signal exists. Third scoped
exception to stateless P7 (aggregate-only).

- **Plugs into:** a worker recorder that tees the GTFS-RT poll into derived
  stop-events, tiered rollups (Tier-0 raw → Tier-1 hourly → Tier-2 permanent
  aggregates, t-digest quantile sketches); read by the gateway reranker and
  no-RT prediction.
- **Data deps:** the daily-fresh GTFS-RT stream (+ road profiles). Storage:
  DuckDB/Parquet (lightest, matches project ethos). Tier-2 is tiny and permanent;
  Tier-0 is short and cost-dominant.
- **Note:** the recorder is critical-path — history is unrecoverable, so it
  should start before the read-side is built.

Design: concept doc 23 — historical-reliability ·
Decision: ADR 0017
