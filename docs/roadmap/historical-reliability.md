# Historical reliability archive (RT ingestion + rollup tier + read side built)

An efficient archive of past delays/cancellations (and past road traffic) that
feeds reliability ranking and prediction when no live signal exists. Third scoped
exception to stateless P7 (aggregate-only).

- **Built (ADR 0015 + 0022 + 0023):** the worker recorder tees the GTFS-RT poll
  into derived stop-events (stable route/direction/stop/date key) and the
  **persistent rollup tier** lands — Tier-0 (hot, ~10-day partition files) →
  Tier-1 (warm, bounded per-day aggregates with a fixed-bin mergeable delay
  histogram) → Tier-2 (cold, tiny, permanent, keyed on route/direction/stop/
  tod-bucket/day-type). The `reliability-rollup` job rebuilds the tiers hourly and
  expires old Tier-0; metrics (p50/p85/p90, on-time rate over [-60 s, +300 s])
  read back from the histogram; the day-type calendar covers the fixed Italian
  holidays plus Easter Monday.
- **Built (ADR 0024):** the **read side** — the pure rollup core and a path-safe,
  size-bounded Tier-2 reader moved into `iter-core`, and the gateway serves
  `GET /reliability/{route}/{direction}/{stop}` over it (all tod-bucket/day-type
  cells for a stop), fail-soft, with no gateway→worker dependency.
- **Remaining:** an in-process reranker that consumes Tier-2 for itinerary
  ranking; past road traffic; a possible DuckDB/Parquet scale-up if the per-host
  volume ever warrants it.
- **Note:** the recorder is critical-path — history is unrecoverable — and
  persists from the first poll; the read side degrades to "no history yet" until
  the archive fills.

Decision: ADR 0015, 0022, 0023, 0024
