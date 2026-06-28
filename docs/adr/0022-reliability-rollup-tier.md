# 0022 — Persistent reliability rollup tier in the worker

- **Status:** Accepted
- **Date:** 2026-06-29
- **Supersedes:** —
- **Superseded by:** — (refined by 0023: Tier-2 rebuild + Easter Monday)

## Context

ADR 0015 landed GTFS-RT ingestion but deferred the **persistent rollup tier**
(Tier-0/1/2 archives and the percentile machinery) and "its P7-stateless
exception" to a follow-on decision. The `rt-reliability` job already polls the
region's trip-updates feed every ~30 s, decodes it, and derives one validated
delay observation per stop on the **stable** (route, direction, stop,
service-date) key — never the raw `trip_id` (renumbered near-daily). Today those
observations are only counted and dropped; history can't be back-filled, so the
recorder must start persisting now even though the value accrues over weeks
(concept doc 23).

Historical reliability needs an archive that a future reranker (and a no-live-
signal predictor) can read, without keeping raw events forever (~1 TiB/yr) and
without standing up a database engine in the lean, pure-Rust, cargo-deny-clean
worker. The privacy invariant **P7 (stateless)** holds across the backend, with
a small set of scoped, opt-in exceptions; this archive must fit that frame
precisely rather than quietly becoming general server state.

## Decision

We will persist GTFS-RT-derived stop-event aggregates as **mergeable Tier-0/1/2
rollups** in the worker, built only from the feed we already poll:

- **Observation key.** Every row is keyed only on the stable
  (route_id, direction_id, stop_id, service_date) tuple plus the derived
  `tod_bucket` and `day_type`. We will **never** key a row on `trip_id`, nor on
  any user, device, or session id — there is none; these are vehicle/road
  aggregates.
- **Three tiers, all derivable and disposable.**
  - *Tier-0 (hot, ~10 days):* append derived events into a per-service-date
    NDJSON partition; expire by **dropping whole old partition files**, never
    editing rows.
  - *Tier-1 (warm, bounded):* one record per
    (route, direction, stop, service_date, tod_bucket) holding mergeable
    aggregate state — count, sum, sum-of-squares, min, max, on-time count, and a
    fixed-bin mergeable delay histogram for percentile estimation.
  - *Tier-2 (cold, permanent, tiny, bounded):* one record per
    (route, direction, stop, tod_bucket, day_type), the associative merge over
    all history — what a future reranker reads. It does **not** grow with time.
- **Bucketing.** Six `tod_bucket`s (early / am-peak / midday / pm-peak /
  evening / night) from the Europe/Rome wall-clock hour of the observation.
  `day_type` is weekday / Saturday / Sunday-&-holiday, calendar-aware over a
  static set of **fixed** Italian public holidays (Jan 1, Jan 6, Apr 25, May 1,
  Jun 2, Aug 15, Nov 1, Dec 8, Dec 25, Dec 26); movable feasts are out of scope.
- **Metrics.** On-time window is [-60 s, +300 s]; p50/p85/p90 and on-time rate
  are read back from the histogram + moments.
- **Engineering.** A pure, I/O-free core (`reliability::rollup`) carries the
  merge algebra and is fully unit-tested; a thin filesystem adapter
  (`reliability::store`) does atomic writes (temp file + rename) and **sanitizes
  every external key component** to `[A-Za-z0-9._-]`, neutralizing `/`, `..`, and
  control chars so a feed value can never escape the reliability dir. The ingest
  job tees events into Tier-0 fail-soft (a store error is logged, the poll
  continues); a scheduled `reliability-rollup` job folds Tier-0 → Tier-1 →
  Tier-2 and drops expired Tier-0.

This is the **third** persistent-state exception to P7. It is explicitly **not a
user-data exception**: it holds operational aggregates about vehicles and roads,
built from public GTFS-RT we already poll. The standing invariant is *never key
any row on a user, device, or session*, and everything stays
derivable/disposable — Tier-0 short, Tier-1 bounded, Tier-2 tiny + permanent +
bounded.

## Consequences

- The worker now keeps its **first persistent state**. A crash mid-write can't
  corrupt a record (atomic rename), but the worker volume now carries data worth
  a few KiB→MiB that is regenerated-by-accrual, not by rebuild.
- **Retention/GC becomes a discipline**: Tier-0 must be expired or it grows
  without bound; the rollup job owns that and the window is a documented knob.
- **Backups are still not needed.** Tier-0/1 are short-lived and re-derivable
  from the live feed; Tier-2 is the only permanent artifact and is small, but
  losing it costs only history, never correctness or availability — consistent
  with the regenerable-artifact posture (P7).
- A new env knob (`RELIABILITY_DIR`, `RELIABILITY_ROLLUP_SECS`) and an on-disk
  layout we must keep stable or migrate.
- The read side (a gateway endpoint / reranker input) is **not** built here; the
  `Readout` shape and a logged Tier-2 overview prove the metrics, and the
  endpoint is tracked as a gap.

## Alternatives considered

- **Raw long-term archive** — keep every derived event forever; ~1 TiB/yr and
  no read benefit over the merged aggregate. Rejected.
- **DuckDB / Parquet** — the lightest "real" analytics option and a plausible
  scale-up, but it adds a native dependency to a worker we keep pure-Rust and
  cargo-deny-clean. Noted as a future scale-up; rejected here.
- **External TSDB (Prometheus/Influx/…)** — operational weight and another
  service to run for a few KiB of permanent aggregates. Rejected.
- **Persist nothing / compute on read** — impossible: history can't be
  back-filled, so the recorder must persist as it polls.
