# 0023 — Tier-2 is a pure rebuild from Tier-1, plus Easter Monday in the day-type calendar

- **Status:** Accepted
- **Date:** 2026-06-29
- **Supersedes:** — (refines 0022)
- **Superseded by:** —

## Context

ADR 0022 landed the persistent reliability rollup tier. As specified there, the
scheduled `reliability-rollup` job folded each service-date's Tier-1 aggregates
*into* the permanent Tier-2 by reading Tier-2, merging the day's contribution in
place, and writing it back. The job folds both today and yesterday on every tick
(hourly by default) for the whole window a Tier-0 partition is live (10 days), so
the same unchanged Tier-1 partition was merged into the permanent Tier-2 dozens
of times. Merge is additive, so Tier-2 `count`, `sum_delay`, `on_time_count`, and
the histogram bins inflated by the number of ticks a partition stayed live. Every
absolute readout (observation totals, the mean denominator, percentile mass) was
corrupted; only ratios such as on-time-rate survived. Tier-2 is the *permanent*
tier, so the corruption never cleared. This broke the module's own stated
invariant that the fold is idempotent over a partition.

ADR 0022 also scoped day-type calendar-awareness to the **fixed** Italian public
holidays and declared movable feasts out of scope. Easter Monday (Lunedì
dell'Angelo) is a national reduced-service day; classifying it as a plain weekday
mis-buckets one high-divergence day each year against the very day-type the
rollup exists to separate.

## Decision

We will make Tier-2 a **pure function of the retained Tier-1 partitions**: the
rollup job folds Tier-0 → Tier-1 per date (idempotent, full-partition rewrite),
then **rebuilds Tier-2 from scratch** by merging every present Tier-1 partition,
re-keyed on (route, direction, stop, tod_bucket, day_type), and atomically
writing the whole Tier-2 file. Recomputing rather than accumulating-in-place makes
the roll idempotent by construction: repeated runs over the same set of Tier-1
partitions yield byte-identical Tier-2, so the hourly today/yesterday cadence can
never double-count.

We will also derive **Easter Monday** per year via the Anonymous Gregorian
computus and include it in the `SundayOrHoliday` day-type, alongside the fixed
holidays. Other movable feasts remain out of scope.

## Consequences

- Tier-2 is now genuinely "the merge over all history" and bounded — its values
  are correct regardless of how often the job runs. The earlier in-place
  `fold_tier1_into_tier2` is replaced by `rebuild_tier2`.
- The rebuild reads *all* Tier-1 partitions each tick, which is O(retained days)
  I/O instead of O(2). Tier-1 is bounded per day and never expired, so this grows
  with calendar history; if that I/O ever bites, a provenance-ledger incremental
  fold can be revisited. For now correctness beats the constant-factor saving.
- Day-type bucketing needs a per-year computation, not just a static table, so a
  malformed year now also has to fail soft — `days_from_ymd` already rejects
  malformed dates, and it now additionally rejects an out-of-range day-of-month
  (e.g. Feb 31) so a bad external `start_date` can't silently roll into the wrong
  civil date and mis-bucket the day-type.

## Alternatives considered

- *Keep the in-place fold, add a per-date provenance marker / Tier-1 hash and skip
  an already-applied partition* — works, but carries a separate ledger that must
  itself stay consistent with Tier-2; the pure rebuild needs no extra state.
- *Subtract the prior contribution before re-adding* — requires persisting each
  date's last-applied snapshot and exact reversibility of the merge; fragile.
- *Leave movable feasts out of scope (as 0022 had it)* — rejected: Easter Monday
  is cheap to derive and is a real reduced-service day the rollup is meant to
  separate.
