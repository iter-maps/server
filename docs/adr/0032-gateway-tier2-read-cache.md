# 0032 — Gateway Tier-2 reliability read cache, mtime-validated

- **Status:** Accepted
- **Date:** 2026-06-29
- **Supersedes:** —
- **Superseded by:** —

## Context

ADR 0024 gave the gateway a path-safe, fail-soft reader over the worker-written
Tier-2 reliability archive (`tier2.json`). Three gateway paths now read it, and
every one re-reads and re-parses the whole file **per request**:

- the `GET /reliability/{route}/{direction}/{stop}` endpoint (ADR 0024),
- the opt-in reranker (`?rerank=<profile>`, ADR 0026/0028), which builds an
  on-time index,
- the opt-in no-RT delay annotator (`?predict=historical`, ADR 0030), which
  builds a typical-delay index.

`read_tier2_map` stats, reads, and `serde_json`-parses the file each time. The
reranker and annotator do this on the routing hot path, inside `spawn_blocking`,
once or twice per request that opts in — disk I/O and a JSON parse on every
routing call, for a file the worker rewrites only hourly (the `reliability-rollup`
job, ADR 0022).

The file is small and changes rarely, so almost every read re-derives an
identical map. The data is **operator aggregates** keyed on (route, direction,
stop, tod-bucket, day-type) — no trip, user, device, or session id (ADR 0022) —
and is fully regenerable from the worker's Tier-1 partitions. The constraint that
shapes the design: the reranker/annotator reads run **synchronously** inside
`spawn_blocking`, and the read endpoint runs in an async handler, so any shared
memo must be lock-safe without ever holding a lock across an `.await`.

## Decision

We will **memoize the parsed Tier-2 map in the gateway process**, validated by
the `tier2.json` file mtime, and share it across all three read paths:

- **Cache shape.** `Tier2Cache` (in `iter-gateway`) holds a
  `Mutex<(Option<SystemTime>, Arc<BTreeMap<String, Tier2>>)>` — the mtime the
  cached map was parsed at, plus the parsed map. It lives in `AppState` as an
  `Arc<Tier2Cache>`, shared across every cheaply-cloned handler.
- **Read protocol.** On a read, stat the file's mtime. If it equals the cached
  mtime (including "absent now and absent last time"), return a cheap `Arc` clone
  of the cached map. Otherwise re-read+parse via `read_tier2_map`, publish the new
  `(mtime, Arc<map>)`, and return the fresh map. The parse happens with the lock
  **released**; the lock is taken only for the stat-compare and the swap, so it is
  never held across the file read and never across an `.await`.
- **Shared derivation.** `iter_core::reliability::store_read` gains
  `tier2_cells_from`, `on_time_index_from`, and `typical_delay_index_from`, which
  operate on an already-parsed map; the file-reading `read_tier2_*` functions
  delegate to them. The read endpoint, reranker, and annotator each derive what
  they need from the one cached map instead of re-reading the file.
- **Fail-soft, panic-free.** A missing, corrupt, oversized, or unreadable
  `tier2.json` yields an empty map — exactly as a direct `read_tier2_map` does
  today. A poisoned lock (a panic while it was held) is **recovered**, not
  unwrapped: the guarded data is a plain `(mtime, Arc<map>)` with no broken
  invariant, so one unlucky request can never wedge the gateway.

This is **derived, disposable soft-state (P7-clean).** The cache is rebuilt from
the regenerable `tier2.json` on restart and holds no user data — only the
operator aggregates the worker already persists. The gateway stays
stateless-on-restart; this is **not** a new persistent-state exception (the
archive itself remains the third scoped exception, ADR 0022/0024).

## Consequences

- A **concurrency surface** is added to the gateway: a shared `Mutex` around the
  cached map. The discipline it imposes — never lock across an `.await`, recover
  from a poisoned lock rather than unwrap — is encoded in `reliability_cache.rs`
  and covered by a concurrent-stress test and a poison-recovery test.
- **Staleness is bounded by the mtime check**, and exactly: a read sees the
  worker's last rewrite as soon as the new mtime lands, so a rollup is never
  masked. The worst case under a same-tick rewrite is a redundant re-read, never a
  stale answer to a *different* mtime.
- The gateway now holds **derived soft-state in memory**, but it is disposable: a
  restart drops the cache and the first read rebuilds it from `tier2.json`. No new
  durability or persistence obligation.
- The hot routing path drops a per-request disk read + JSON parse to a cheap
  `Arc` clone on the common (unchanged-file) case.

## Alternatives considered

- **No cache / per-request read (today).** Re-stat, re-read, re-parse on every
  request. Simple, but pays disk I/O and a JSON parse on the routing hot path for
  a file that changes hourly. Rejected.
- **A TTL cache** (like the upstream `TtlCache`). Would re-read on a fixed timer
  regardless of whether the file changed, trading exactness for a knob. The mtime
  check is both exact and free (one `stat`), so a TTL adds staleness and a tuning
  parameter for nothing. Rejected.
- **`tokio::sync::RwLock`.** The reranker/annotator reads run synchronously inside
  `spawn_blocking`; an async lock there would mean blocking on an async primitive
  off the runtime. A `std::sync::Mutex` held for a stat-and-swap, never across an
  `.await`, is the right primitive. Rejected.
- **Precompute the map once in `AppState::new`.** Cheapest reads, but it would
  never pick up the worker's hourly rewrites — the gateway would serve a snapshot
  frozen at boot. Rejected; the mtime check is what makes a rollup visible.
