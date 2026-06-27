//! A TTL cache with per-key single-flight. Concurrent requests for the same key
//! coalesce onto one upstream fetch (one waits on the other), and the result is
//! reused until it expires — the protection that keeps the flaky, rate-limited
//! ViaggiaTreno upstream alive under load.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct TtlCache<V> {
    entries: Mutex<HashMap<String, (V, Instant)>>,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl<V: Clone> Default for TtlCache<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: Clone> TtlCache<V> {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            locks: Mutex::new(HashMap::new()),
        }
    }

    fn fresh(&self, key: &str, ttl: Duration) -> Option<V> {
        let entries = self.entries.lock().unwrap();
        entries
            .get(key)
            .and_then(|(v, at)| (at.elapsed() < ttl).then(|| v.clone()))
    }

    fn store(&self, key: &str, value: V) {
        self.entries
            .lock()
            .unwrap()
            .insert(key.to_string(), (value, Instant::now()));
    }

    fn key_lock(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .unwrap()
            .entry(key.to_string())
            .or_default()
            .clone()
    }

    /// Return the cached value if fresh; otherwise fetch under a per-key lock so
    /// only one in-flight fetch per key runs at a time.
    pub async fn get_or_fetch<F, Fut, E>(&self, key: &str, ttl: Duration, fetch: F) -> Result<V, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<V, E>>,
    {
        if let Some(v) = self.fresh(key, ttl) {
            return Ok(v);
        }
        let lock = self.key_lock(key);
        let _guard = lock.lock().await;
        // Re-check: a coalesced caller may have populated it while we waited.
        if let Some(v) = self.fresh(key, ttl) {
            return Ok(v);
        }
        let value = fetch().await?;
        self.store(key, value.clone());
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    #[tokio::test]
    async fn caches_within_ttl_and_fetches_once() {
        let cache = TtlCache::<u32>::new();
        let calls = AtomicU32::new(0);

        let fetch = || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<u32, ()>(1)
        };
        let a = cache
            .get_or_fetch("k", Duration::from_secs(60), fetch)
            .await
            .unwrap();
        let b = cache
            .get_or_fetch("k", Duration::from_secs(60), || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<u32, ()>(2)
            })
            .await
            .unwrap();

        assert_eq!(a, 1);
        assert_eq!(b, 1, "second hit returns the cached value");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "fetched exactly once");
    }

    #[tokio::test]
    async fn refetches_after_expiry() {
        let cache = TtlCache::<u32>::new();
        cache
            .get_or_fetch("k", Duration::ZERO, || async { Ok::<u32, ()>(1) })
            .await
            .unwrap();
        // TTL zero → already stale → fetches again.
        let v = cache
            .get_or_fetch("k", Duration::ZERO, || async { Ok::<u32, ()>(2) })
            .await
            .unwrap();
        assert_eq!(v, 2);
    }

    #[tokio::test]
    async fn distinct_keys_are_independent() {
        let cache = TtlCache::<u32>::new();
        let a = cache
            .get_or_fetch("a", Duration::from_secs(60), || async { Ok::<u32, ()>(1) })
            .await
            .unwrap();
        let b = cache
            .get_or_fetch("b", Duration::from_secs(60), || async { Ok::<u32, ()>(2) })
            .await
            .unwrap();
        assert_eq!(a, 1);
        assert_eq!(b, 2);
    }
}
