//! In-process memo of the parsed Tier-2 reliability map, validated by the
//! `tier2.json` file mtime (ADR 0032). The three reliability read paths — the
//! `/reliability` endpoint, the reranker, and the no-RT delay annotator — all
//! used to re-read and re-parse the same file per request. This caches the
//! parsed map and hands each path a cheap `Arc` clone, re-reading only when the
//! worker's hourly rollup rewrites the file (a changed mtime).
//!
//! This is **derived, disposable soft-state**: it is rebuilt from the
//! regenerable `tier2.json` on restart and holds no user data — only operator
//! route/stop aggregates. The gateway stays stateless-on-restart; this is not a
//! new persistent-state exception.
//!
//! Concurrency: the cache is a `Mutex` around `(mtime, Arc<map>)`. The lock is
//! held only for the cheap stat-compare and swap — never across an `.await` and
//! never across the file read+parse (which happens with the lock released). A
//! poisoned lock (a panic while held) is recovered rather than unwrapped, so one
//! unlucky request can never wedge the gateway; the worst case is a redundant
//! re-read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use iter_core::reliability::rollup::Tier2;
use iter_core::reliability::store_read::{TIER2_FILE, read_tier2_map};

/// The parsed Tier-2 map, shared by `Arc` across every reader.
pub type Tier2Map = Arc<BTreeMap<String, Tier2>>;

/// mtime-validated cache of the parsed Tier-2 map for one reliability dir.
pub struct Tier2Cache {
    root: PathBuf,
    // (mtime the cached map was parsed at, the parsed map). `None` mtime means
    // "nothing cached yet" or "the file was absent last time" — either way the
    // next read re-stats and reloads.
    inner: Mutex<(Option<SystemTime>, Tier2Map)>,
}

impl Tier2Cache {
    /// A cold cache for the Tier-2 file under `reliability_dir`. Nothing is read
    /// until the first [`Tier2Cache::map`] call.
    pub fn new(reliability_dir: PathBuf) -> Self {
        Self {
            root: reliability_dir,
            inner: Mutex::new((None, Arc::new(BTreeMap::new()))),
        }
    }

    /// The current parsed Tier-2 map, as a cheap `Arc` clone. Returns the cached
    /// map when its mtime matches the file's; otherwise re-reads+parses, updates
    /// the cache, and returns the fresh map. Fail-soft: a missing/corrupt/
    /// oversized/unreadable file yields an empty map (same as a direct
    /// `read_tier2_map`), never an error or panic. Synchronous and lock-safe —
    /// no `.await` is held across the lock, and a poisoned lock is recovered.
    pub fn map(&self) -> Tier2Map {
        let path = self.root.join(TIER2_FILE);
        let current = file_mtime(&path);

        // Fast path: under the lock, if the file's mtime equals what we parsed
        // last time, the cached map is still valid — clone the Arc and return.
        // `None == None` (file absent now and last time) is a hit on the empty
        // map, so a never-written store doesn't re-read every request. Equality
        // (not a monotonic `>`) is deliberate: a non-monotonic rewrite — a
        // backup-restore or clock-skew that lowers the mtime — still differs and
        // reloads, which a `>` check would miss.
        {
            let guard = lock(&self.inner);
            if guard.0 == current {
                return guard.1.clone();
            }
        }

        // Slow path: the mtime moved (or this is the first read). Parse with the
        // lock RELEASED so a slow read never serializes other readers, then take
        // the lock again to publish. A racing reader may parse concurrently; the
        // last writer wins and both observe a correct map for the file they saw.
        let fresh: Tier2Map = Arc::new(read_tier2_map(&self.root));
        let mut guard = lock(&self.inner);
        *guard = (current, fresh.clone());
        fresh
    }
}

/// The file's mtime, or `None` if it can't be stat'd (absent/unreadable) — the
/// fail-soft "treat as empty" signal, paired with `read_tier2_map` returning an
/// empty map for the same conditions.
fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Lock the cache, recovering from a poisoned lock instead of unwrapping. A
/// panic while the lock was held poisons it, but the guarded data is a plain
/// `(mtime, Arc<map>)` with no broken invariant, so recovering and reusing it is
/// always safe — and it keeps one panic from wedging every later request.
fn lock(
    m: &Mutex<(Option<SystemTime>, Tier2Map)>,
) -> std::sync::MutexGuard<'_, (Option<SystemTime>, Tier2Map)> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iter_core::reliability::rollup::{DayType, TodBucket};
    use iter_core::reliability::store_read::{read_tier2_map, tier2_key};
    use std::collections::BTreeMap;

    fn seed(root: &Path, late_delay: i32) {
        std::fs::create_dir_all(root).unwrap();
        let mut agg = Tier2::default();
        agg.observe(0);
        agg.observe(late_delay);
        let mut map = BTreeMap::new();
        map.insert(
            tier2_key("MEA", 0, "70001", TodBucket::AmPeak, DayType::Weekday),
            agg,
        );
        std::fs::write(root.join(TIER2_FILE), serde_json::to_vec(&map).unwrap()).unwrap();
    }

    #[test]
    fn cached_map_equals_a_fresh_direct_read() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), 600);
        let cache = Tier2Cache::new(tmp.path().to_path_buf());
        // The cached map must be byte-for-byte the direct read of the same file.
        assert_eq!(*cache.map(), read_tier2_map(tmp.path()));
        // A second read serves the cache and is still identical.
        assert_eq!(*cache.map(), read_tier2_map(tmp.path()));
    }

    #[test]
    fn second_read_returns_the_same_arc() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), 600);
        let cache = Tier2Cache::new(tmp.path().to_path_buf());
        let a = cache.map();
        let b = cache.map();
        // Same mtime → the cache hands back the same Arc, no re-parse.
        assert!(Arc::ptr_eq(&a, &b), "cache hit should reuse the Arc");
    }

    #[test]
    fn a_new_mtime_reloads_the_map() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), 600);
        let cache = Tier2Cache::new(tmp.path().to_path_buf());
        let first = cache.map();

        // Rewrite with a different aggregate and bump the mtime; the next read
        // must reflect the new data (a worker rollup is picked up).
        seed(tmp.path(), 1800);
        bump_mtime(&tmp.path().join(TIER2_FILE));

        let second = cache.map();
        assert!(
            !Arc::ptr_eq(&first, &second),
            "a changed mtime must reload, not reuse"
        );
        assert_eq!(*second, read_tier2_map(tmp.path()));
    }

    #[test]
    fn missing_file_is_empty_and_fail_soft() {
        let tmp = tempfile::tempdir().unwrap();
        // No tier2.json written at all → empty map, never a panic.
        let cache = Tier2Cache::new(tmp.path().to_path_buf());
        assert!(cache.map().is_empty());
        // Repeated reads of an absent file stay empty and don't error.
        assert!(cache.map().is_empty());
    }

    #[test]
    fn corrupt_file_is_empty_and_fail_soft() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(TIER2_FILE), b"{ not json").unwrap();
        let cache = Tier2Cache::new(tmp.path().to_path_buf());
        assert!(cache.map().is_empty());
    }

    #[test]
    fn appearing_file_is_picked_up() {
        // Cache starts cold against an absent file (empty), then the worker
        // writes the store — the next read must reflect it.
        let tmp = tempfile::tempdir().unwrap();
        let cache = Tier2Cache::new(tmp.path().to_path_buf());
        assert!(cache.map().is_empty());
        seed(tmp.path(), 600);
        assert_eq!(*cache.map(), read_tier2_map(tmp.path()));
        assert!(!cache.map().is_empty());
    }

    #[test]
    fn concurrent_reads_are_safe() {
        // Many threads hammering one cache must never panic or deadlock and must
        // all observe the seeded map.
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), 600);
        let cache = Arc::new(Tier2Cache::new(tmp.path().to_path_buf()));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let c = cache.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..50 {
                    assert_eq!(c.map().len(), 1);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn poisoned_lock_is_recovered_not_unwrapped() {
        // Poison the inner lock by panicking while it is held, then prove the
        // cache still serves reads (the recover-on-poison path).
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), 600);
        let cache = Arc::new(Tier2Cache::new(tmp.path().to_path_buf()));
        // Prime the cache so the map is populated before we poison.
        assert_eq!(cache.map().len(), 1);

        let c = cache.clone();
        let poisoned = std::thread::spawn(move || {
            let _g = c.inner.lock().unwrap();
            panic!("poison the cache lock while held");
        })
        .join();
        assert!(poisoned.is_err(), "the spawned thread should have panicked");
        assert!(cache.inner.is_poisoned(), "the lock should be poisoned");

        // Despite the poison, reads still succeed (recovered, not unwrapped).
        assert_eq!(cache.map().len(), 1);
    }

    /// Force a file's mtime to differ from its current value, simulating a worker
    /// rewrite without depending on a set-mtime crate. std has no public set-mtime
    /// and a same-tick rewrite can land on the same coarse timestamp, so re-touch
    /// in a loop until the OS stamps a new mtime — all the cache needs is a
    /// *different* value.
    fn bump_mtime(path: &Path) {
        let before = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        loop {
            let bytes = std::fs::read(path).unwrap();
            std::fs::write(path, &bytes).unwrap();
            let now = std::fs::metadata(path).and_then(|m| m.modified()).ok();
            if now != before {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
}
