# Historical reliability archive (RT ingestion + rollup tier built)

An efficient archive of past delays/cancellations (and past road traffic) that
feeds reliability ranking and prediction when no live signal exists. Third scoped
exception to stateless P7 (aggregate-only).

- **Built (ADR 0015 + 0022):** the worker recorder tees the GTFS-RT poll into
  derived stop-events (stable route/direction/stop/date key) and the **persistent
  rollup tier** lands — Tier-0 (hot, ~10-day partition files) → Tier-1 (warm,
  bounded per-day aggregates with a fixed-bin mergeable delay histogram) → Tier-2
  (cold, tiny, permanent, keyed on route/direction/stop/tod-bucket/day-type). The
  `reliability-rollup` job folds the tiers hourly and expires old Tier-0; metrics
  (p50/p85/p90, on-time rate over [-60 s, +300 s]) read back from the histogram.
- **Remaining:** the gateway **read endpoint** / reranker input over Tier-2
  (the `Readout` shape exists and is logged, but no HTTP surface yet); past road
  traffic; movable-holiday calendar; a possible DuckDB/Parquet scale-up if the
  per-host volume ever warrants it.
- **Note:** the recorder is critical-path — history is unrecoverable — and now
  persists from the first poll, ahead of the read-side.

Decision: ADR 0015, 0022
