//! Filesystem store for the reliability rollups. A thin adapter over the pure
//! core in `reliability::rollup`: Tier-0 is an append-only per-service-date
//! partition of derived events; Tier-1/Tier-2 are read-modify-write JSON maps of
//! mergeable aggregates. Writes are atomic (temp file + rename) so a crash mid-
//! write never leaves a half-record.
//!
//! SECURITY: `route_id` and `stop_id` arrive verbatim from an external GTFS-RT
//! feed. Every path component is sanitized to a safe token (`sanitize_token`)
//! before it touches a path, so a feed value like `../../etc` can never escape
//! the reliability dir. The store never joins an un-sanitized external string.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::jobs::rt_reliability::StopEvent;
use crate::reliability::rollup::{
    DayType, Readout, Tier1, Tier2, TodBucket, day_type_of, tod_bucket_from_hour,
};
// The read side (key shape, Tier-2 filename, sanitizer) is shared with the
// gateway via iter-core so the on-disk layout has a single owner (ADR 0024).
use iter_core::reliability::store_read::{TIER2_FILE, sanitize_token, tier2_key};

/// On-disk subdirectories under the reliability root.
const TIER0_DIR: &str = "tier0";
const TIER1_DIR: &str = "tier1";

/// Tier-0 retention window in days; partitions older than this are dropped
/// wholesale (file unlink), never edited row-by-row.
pub const TIER0_RETAIN_DAYS: i64 = 10;

/// The filesystem store. All public methods are fail-soft at the call site: they
/// return `anyhow::Result` and the caller logs-and-continues, since losing
/// history is acceptable but crashing a poll is not.
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Tier-0 partition file for a service-date (already a digit string from the
    /// feed; sanitized defensively).
    fn tier0_path(&self, service_date: &str) -> PathBuf {
        self.root
            .join(TIER0_DIR)
            .join(format!("{}.ndjson", sanitize_token(service_date)))
    }

    /// Tier-1 file for a service-date.
    fn tier1_path(&self, service_date: &str) -> PathBuf {
        self.root
            .join(TIER1_DIR)
            .join(format!("{}.json", sanitize_token(service_date)))
    }

    fn tier2_path(&self) -> PathBuf {
        self.root.join(TIER2_FILE)
    }

    /// Append derived events to their service-date Tier-0 partition as NDJSON,
    /// one line per event. Append is open-append so concurrent ticks don't clobber
    /// each other; a partial line on crash is dropped by the reader.
    pub fn append_tier0(&self, events: &[StopEvent]) -> anyhow::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        // Events in a single feed all share a service-date in practice, but group
        // defensively so a mixed batch still lands in the right partitions.
        let mut by_date: BTreeMap<&str, Vec<&StopEvent>> = BTreeMap::new();
        for e in events {
            by_date.entry(&e.service_date).or_default().push(e);
        }
        for (date, evs) in by_date {
            let path = self.tier0_path(date);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut buf = String::new();
            for e in evs {
                let rec = Tier0Record {
                    route_id: &e.route_id,
                    direction_id: e.direction_id,
                    stop_id: &e.stop_id,
                    service_date: &e.service_date,
                    delay_s: e.delay_s,
                    feed_hour: e.feed_hour,
                };
                buf.push_str(&serde_json::to_string(&rec)?);
                buf.push('\n');
            }
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            f.write_all(buf.as_bytes())?;
        }
        Ok(())
    }

    /// Read a Tier-0 partition's events back. Malformed lines are skipped (fail-
    /// soft over a torn write or a future format). Missing file → empty vec.
    pub fn read_tier0(&self, service_date: &str) -> anyhow::Result<Vec<StopEvent>> {
        let path = self.tier0_path(service_date);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(r) = serde_json::from_str::<Tier0RecordOwned>(line) {
                out.push(StopEvent {
                    route_id: r.route_id,
                    direction_id: r.direction_id,
                    stop_id: r.stop_id,
                    service_date: r.service_date,
                    delay_s: r.delay_s,
                    feed_hour: r.feed_hour,
                });
            }
        }
        Ok(out)
    }

    /// Fold a service-date's Tier-0 events into its Tier-1 map (read-modify-write,
    /// merging into any existing aggregates so a re-run is idempotent-ish: it
    /// re-folds the whole partition, so it must be called on the full partition,
    /// not deltas). Returns the number of (key) aggregates written.
    pub fn fold_tier0_into_tier1(&self, service_date: &str) -> anyhow::Result<usize> {
        let events = self.read_tier0(service_date)?;
        if events.is_empty() {
            return Ok(0);
        }
        let mut map: BTreeMap<String, Tier1> = BTreeMap::new();
        for e in &events {
            let bucket = tod_bucket_from_hour(e.feed_hour);
            let key = tier1_key(
                &e.route_id,
                e.direction_id,
                &e.stop_id,
                &e.service_date,
                bucket,
            );
            map.entry(key).or_default().observe(e.delay_s);
        }
        let path = self.tier1_path(service_date);
        self.write_json_atomic(&path, &map)?;
        Ok(map.len())
    }

    /// Read a service-date's Tier-1 map (missing → empty).
    pub fn read_tier1(&self, service_date: &str) -> anyhow::Result<BTreeMap<String, Tier1>> {
        read_json_map(&self.tier1_path(service_date))
    }

    /// Read the permanent Tier-2 map (missing → empty).
    pub fn read_tier2(&self) -> anyhow::Result<BTreeMap<String, Tier2>> {
        read_json_map(&self.tier2_path())
    }

    /// A compact read-side rollup of the whole Tier-2 archive: number of keys and
    /// the total-observation `Readout` merged across them. Used for operator
    /// logging today and shaped like what a future read-endpoint serves per key.
    pub fn tier2_overview(&self) -> anyhow::Result<(usize, Readout)> {
        let map = self.read_tier2()?;
        let mut all = Tier2::default();
        for agg in map.values() {
            all.merge(agg);
        }
        Ok((map.len(), Readout::of(&all)))
    }

    /// Rebuild the permanent Tier-2 map as a pure function of every retained
    /// Tier-1 partition: read each Tier-1 file, re-key its aggregates on
    /// (route, direction, stop, tod_bucket, day_type), merge them, and atomically
    /// write the whole Tier-2 file. Recomputing from scratch (rather than
    /// accumulating in place) makes the roll idempotent: re-running it any number
    /// of times for the same set of partitions yields identical Tier-2 counts, so
    /// the hourly job's repeated today/yesterday folds can never double-count.
    /// Returns the number of Tier-2 keys written.
    pub fn rebuild_tier2(&self) -> anyhow::Result<usize> {
        let mut tier2: BTreeMap<String, Tier2> = BTreeMap::new();
        for service_date in self.tier1_dates()? {
            let day_type = day_type_of(&service_date).unwrap_or(DayType::Weekday);
            let tier1 = self.read_tier1(&service_date)?;
            for (k1, agg) in &tier1 {
                let Some(parts) = Tier1Key::parse(k1) else {
                    continue;
                };
                let k2 = tier2_key(
                    &parts.route_id,
                    parts.direction_id,
                    &parts.stop_id,
                    parts.tod_bucket,
                    day_type,
                );
                tier2.entry(k2).or_default().merge(agg);
            }
        }
        self.write_json_atomic(&self.tier2_path(), &tier2)?;
        Ok(tier2.len())
    }

    /// The service-dates of every present Tier-1 partition (file stem of each
    /// `<date>.json`). Missing dir → empty. Non-date stems are skipped.
    fn tier1_dates(&self) -> anyhow::Result<Vec<String>> {
        let dir = self.root.join(TIER1_DIR);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut dates = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            if crate::reliability::rollup::days_from_ymd(stem).is_some() {
                dates.push(stem.to_string());
            }
        }
        Ok(dates)
    }

    /// Drop Tier-0 partitions whose service-date is older than `keep_days` before
    /// `today` (a `YYYYMMDD` reference). Returns the dropped partition names.
    /// Files whose name isn't a parseable date are left untouched.
    pub fn expire_tier0(&self, today: &str, keep_days: i64) -> anyhow::Result<Vec<String>> {
        use crate::reliability::rollup::days_from_ymd;
        let dir = self.root.join(TIER0_DIR);
        let Some(today_days) = days_from_ymd(today) else {
            return Ok(Vec::new());
        };
        let cutoff = today_days - keep_days;
        let mut dropped = Vec::new();
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".ndjson") else {
                continue;
            };
            let Some(days) = days_from_ymd(stem) else {
                continue;
            };
            if days < cutoff && std::fs::remove_file(entry.path()).is_ok() {
                dropped.push(name);
            }
        }
        Ok(dropped)
    }

    /// Atomic JSON write: serialize to a temp file in the same dir, then rename
    /// over the target (rename is atomic on the same filesystem).
    fn write_json_atomic<T: serde::Serialize>(&self, path: &Path, value: &T) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec(value)?;
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&json)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Read a JSON map of aggregates, fail-soft: a missing file is an empty map, a
/// corrupt file is also an empty map (rather than poisoning the fold).
fn read_json_map<T: serde::de::DeserializeOwned>(
    path: &Path,
) -> anyhow::Result<BTreeMap<String, T>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e.into()),
    };
    Ok(serde_json::from_str(&text).unwrap_or_default())
}

/// Build a Tier-1 record key from its components. Each external component is
/// sanitized so the key is a flat, traversal-free token joined by `/` field
/// separators (the key is a map key, not a path — but we sanitize anyway in case
/// it is ever used to derive one).
fn tier1_key(
    route_id: &str,
    direction_id: i32,
    stop_id: &str,
    service_date: &str,
    bucket: TodBucket,
) -> String {
    format!(
        "{}/{}/{}/{}/{}",
        sanitize_token(route_id),
        direction_id,
        sanitize_token(stop_id),
        sanitize_token(service_date),
        bucket.token(),
    )
}

/// The parsed parts of a Tier-1 key needed to re-key into Tier-2.
struct Tier1Key {
    route_id: String,
    direction_id: i32,
    stop_id: String,
    tod_bucket: TodBucket,
}

impl Tier1Key {
    /// Parse a Tier-1 key (`route/dir/stop/date/bucket`) back into its parts.
    /// Returns `None` if the shape doesn't match.
    fn parse(key: &str) -> Option<Self> {
        let parts: Vec<&str> = key.split('/').collect();
        if parts.len() != 5 {
            return None;
        }
        Some(Self {
            route_id: parts[0].to_string(),
            direction_id: parts[1].parse().ok()?,
            stop_id: parts[2].to_string(),
            tod_bucket: TodBucket::from_token(parts[4])?,
        })
    }
}

/// The serialized Tier-0 line (borrowed for the write side).
#[derive(serde::Serialize)]
struct Tier0Record<'a> {
    route_id: &'a str,
    direction_id: i32,
    stop_id: &'a str,
    service_date: &'a str,
    delay_s: i32,
    feed_hour: i32,
}

/// The deserialized Tier-0 line (owned for the read side).
#[derive(serde::Deserialize)]
struct Tier0RecordOwned {
    route_id: String,
    direction_id: i32,
    stop_id: String,
    service_date: String,
    delay_s: i32,
    #[serde(default)]
    feed_hour: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(route: &str, stop: &str, date: &str, delay: i32, hour: i32) -> StopEvent {
        StopEvent {
            route_id: route.to_string(),
            direction_id: 0,
            stop_id: stop.to_string(),
            service_date: date.to_string(),
            delay_s: delay,
            feed_hour: hour,
        }
    }

    #[test]
    fn sanitize_neutralizes_traversal_and_separators() {
        // Alphanumerics, '_' and '-' pass through verbatim.
        assert_eq!(sanitize_token("MEA"), "MEA");
        assert_eq!(sanitize_token("70001"), "70001");
        assert_eq!(sanitize_token("ok_chars-only"), "ok_chars-only");
        // Path separators and every "../" segment are escaped — no "/", no "..".
        let cleaned = sanitize_token("../../etc/passwd");
        assert!(!cleaned.contains('/'));
        assert!(!cleaned.contains(".."));
        assert!(!sanitize_token("a/b").contains('/'));
        // Control chars and NUL are escaped, not present literally.
        let escaped = sanitize_token("x\n\t\0y");
        assert!(escaped.starts_with('x') && escaped.ends_with('y'));
        assert!(!escaped.contains('\n') && !escaped.contains('\0'));
        // Dots are always escaped, so "." and ".." can never appear literally.
        assert!(!sanitize_token(".").contains('.'));
        assert!(!sanitize_token("..").contains(".."));
        assert!(!sanitize_token("...").contains(".."));
        // Empty input is a placeholder.
        assert_eq!(sanitize_token(""), "_");
    }

    #[test]
    fn malicious_key_cannot_escape_the_reliability_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path());
        // A feed value crafted to traverse upward.
        let evil = ev("../../../../tmp/evil", "../../escape", "20260629", 30, 8);
        store.append_tier0(&[evil]).unwrap();

        // Nothing was written outside the reliability root.
        let tier0 = tmp.path().join(TIER0_DIR);
        let mut all_files = Vec::new();
        for entry in std::fs::read_dir(&tier0).unwrap().flatten() {
            all_files.push(entry.path());
        }
        for p in &all_files {
            assert!(p.starts_with(&tier0), "file {p:?} escaped the tier0 dir");
        }
        // And the partition file name carries the sanitized service-date.
        store.fold_tier0_into_tier1("20260629").unwrap();
        let map = store.read_tier1("20260629").unwrap();
        for key in map.keys() {
            assert!(!key.contains(".."), "key {key} still contains traversal");
        }
    }

    #[test]
    fn tier0_round_trip_in_a_tempdir() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path());
        let events = vec![
            ev("MEA", "70001", "20260629", 60, 8),
            ev("MEA", "70002", "20260629", 120, 8),
        ];
        store.append_tier0(&events).unwrap();
        // A second append accumulates (open-append, not truncate).
        store
            .append_tier0(&[ev("MEA", "70001", "20260629", -30, 8)])
            .unwrap();

        let back = store.read_tier0("20260629").unwrap();
        assert_eq!(back.len(), 3);
        assert_eq!(back[0].route_id, "MEA");
        assert_eq!(back[2].delay_s, -30);
    }

    #[test]
    fn full_fold_tier0_to_tier1_to_tier2() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path());
        // Two stops, same AM-peak hour, on 2026-06-29 (a Monday → weekday).
        store
            .append_tier0(&[
                ev("MEA", "70001", "20260629", 0, 8),
                ev("MEA", "70001", "20260629", 120, 8),
                ev("MEA", "70002", "20260629", 600, 8),
            ])
            .unwrap();

        let n1 = store.fold_tier0_into_tier1("20260629").unwrap();
        assert_eq!(n1, 2, "two distinct (route,dir,stop,date,bucket) keys");
        let t1 = store.read_tier1("20260629").unwrap();
        let stop1 = t1
            .iter()
            .find(|(k, _)| k.contains("70001"))
            .map(|(_, v)| v)
            .unwrap();
        assert_eq!(stop1.count, 2);

        let n2 = store.rebuild_tier2().unwrap();
        assert_eq!(n2, 2);
        let t2 = store.read_tier2().unwrap();
        // Every Tier-2 key carries the weekday day-type.
        assert!(t2.keys().all(|k| k.ends_with("weekday")));

        // Folding a second weekday merges into the same Tier-2 keys (no growth).
        store
            .append_tier0(&[ev("MEA", "70001", "20260630", 60, 8)])
            .unwrap();
        store.fold_tier0_into_tier1("20260630").unwrap();
        store.rebuild_tier2().unwrap();
        let t2b = store.read_tier2().unwrap();
        assert_eq!(
            t2b.len(),
            2,
            "Tier-2 is bounded — no new keys for a new date"
        );
        let stop1_t2 = t2b
            .iter()
            .find(|(k, _)| k.contains("70001"))
            .map(|(_, v)| v)
            .unwrap();
        assert_eq!(stop1_t2.count, 3, "merged across both service-dates");
        let r = Readout::of(stop1_t2);
        assert!(r.on_time_rate.is_some());
    }

    #[test]
    fn retention_drops_only_expired_partitions() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path());
        // Old partition (well outside the 10-day window) and a fresh one.
        store
            .append_tier0(&[ev("MEA", "S", "20260601", 0, 8)])
            .unwrap();
        store
            .append_tier0(&[ev("MEA", "S", "20260628", 0, 8)])
            .unwrap();
        // A non-date file that must be left alone.
        std::fs::write(tmp.path().join(TIER0_DIR).join("README.txt"), b"keep").unwrap();

        let dropped = store.expire_tier0("20260629", TIER0_RETAIN_DAYS).unwrap();
        assert_eq!(dropped, vec!["20260601.ndjson".to_string()]);
        // The fresh partition survives.
        assert!(!store.read_tier0("20260628").unwrap().is_empty());
        // The non-date file survives.
        assert!(tmp.path().join(TIER0_DIR).join("README.txt").exists());
    }

    #[test]
    fn retention_is_fail_soft_on_missing_dir_and_bad_today() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path());
        // No tier0 dir yet.
        assert!(store.expire_tier0("20260629", 10).unwrap().is_empty());
        // Garbage `today` doesn't panic.
        store
            .append_tier0(&[ev("MEA", "S", "20260601", 0, 8)])
            .unwrap();
        assert!(store.expire_tier0("notadate", 10).unwrap().is_empty());
    }

    #[test]
    fn corrupt_files_are_fail_soft() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path());
        // A torn Tier-0 line plus a good one.
        std::fs::create_dir_all(tmp.path().join(TIER0_DIR)).unwrap();
        std::fs::write(
            store.tier0_path("20260629"),
            b"{not json}\n{\"route_id\":\"MEA\",\"direction_id\":0,\"stop_id\":\"S\",\"service_date\":\"20260629\",\"delay_s\":42,\"feed_hour\":8}\n",
        )
        .unwrap();
        let back = store.read_tier0("20260629").unwrap();
        assert_eq!(back.len(), 1, "the torn line is skipped");
        assert_eq!(back[0].delay_s, 42);

        // A corrupt Tier-1 file reads as empty rather than erroring.
        std::fs::create_dir_all(tmp.path().join(TIER1_DIR)).unwrap();
        std::fs::write(store.tier1_path("20260629"), b"{ corrupt").unwrap();
        assert!(store.read_tier1("20260629").unwrap().is_empty());
    }

    #[test]
    fn rebuild_tier2_is_idempotent_across_repeated_runs() {
        // The hourly job folds today and yesterday on every tick; rebuilding
        // Tier-2 from the Tier-1 partitions must not double-count a re-folded day.
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path());
        store
            .append_tier0(&[
                ev("MEA", "70001", "20260629", 0, 8),
                ev("MEA", "70001", "20260629", 120, 8),
            ])
            .unwrap();
        store.fold_tier0_into_tier1("20260629").unwrap();

        store.rebuild_tier2().unwrap();
        let first = store.read_tier2().unwrap();
        let first_count: u64 = first.values().map(|a| a.count).sum();
        assert_eq!(first_count, 2);

        // Re-fold the SAME partition and rebuild many times: counts stay put.
        for _ in 0..5 {
            store.fold_tier0_into_tier1("20260629").unwrap();
            store.rebuild_tier2().unwrap();
        }
        let again = store.read_tier2().unwrap();
        assert_eq!(again, first, "repeated rebuilds must be idempotent");
        let again_count: u64 = again.values().map(|a| a.count).sum();
        assert_eq!(again_count, 2, "Tier-2 must not inflate on repeated runs");
    }

    #[test]
    fn persisted_tier0_line_holds_no_user_data() {
        // P7 invariant: a persisted row never keys on or contains a trip-id,
        // user, device, or session. Assert it over the raw on-disk NDJSON.
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path());
        store
            .append_tier0(&[ev("MEA", "70001", "20260629", 60, 8)])
            .unwrap();
        let raw = std::fs::read_to_string(store.tier0_path("20260629")).unwrap();
        for forbidden in ["trip_id", "user", "device", "session"] {
            assert!(
                !raw.contains(forbidden),
                "persisted Tier-0 line leaked `{forbidden}`: {raw}"
            );
        }
    }

    #[test]
    fn tier1_and_tier2_keys_hold_no_trip_id() {
        // The derived keys carry only the stable (route, dir, stop, date/bucket,
        // day-type) tuple — never a trip-id-derived token.
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path());
        store
            .append_tier0(&[ev("MEA", "70001", "20260629", 60, 8)])
            .unwrap();
        store.fold_tier0_into_tier1("20260629").unwrap();
        store.rebuild_tier2().unwrap();
        for key in store.read_tier1("20260629").unwrap().keys() {
            assert!(!key.contains("renumbered"), "tier1 key carried a trip id");
        }
        for key in store.read_tier2().unwrap().keys() {
            assert!(!key.contains("renumbered"), "tier2 key carried a trip id");
        }
    }

    #[test]
    fn extra_persisted_field_is_ignored_on_read() {
        // A future/foreign field (e.g. a leaked trip_id) round-trips into nothing:
        // the reader keeps only the known columns, so it can't enter an aggregate.
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path());
        std::fs::create_dir_all(tmp.path().join(TIER0_DIR)).unwrap();
        std::fs::write(
            store.tier0_path("20260629"),
            b"{\"route_id\":\"MEA\",\"direction_id\":0,\"stop_id\":\"S\",\"service_date\":\"20260629\",\"delay_s\":42,\"feed_hour\":8,\"trip_id\":\"0#renumbered\"}\n",
        )
        .unwrap();
        let back = store.read_tier0("20260629").unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].delay_s, 42);
    }

    #[test]
    fn tod_bucket_round_trips_through_the_fold() {
        // Two events for the same route/stop/date but different hours land in
        // different tod buckets, key distinctly through Tier-1, and survive as two
        // distinct Tier-2 keys with the right bucket tokens.
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path());
        store
            .append_tier0(&[
                ev("MEA", "70001", "20260629", 60, 8),  // AM peak
                ev("MEA", "70001", "20260629", 90, 12), // midday
            ])
            .unwrap();
        store.fold_tier0_into_tier1("20260629").unwrap();
        let t1 = store.read_tier1("20260629").unwrap();
        assert_eq!(t1.len(), 2, "two buckets → two Tier-1 keys");
        assert!(t1.keys().any(|k| k.ends_with("am-peak")));
        assert!(t1.keys().any(|k| k.ends_with("midday")));

        store.rebuild_tier2().unwrap();
        let t2 = store.read_tier2().unwrap();
        assert_eq!(t2.len(), 2, "two buckets → two Tier-2 keys");
        assert!(t2.keys().any(|k| k.contains("am-peak")));
        assert!(t2.keys().any(|k| k.contains("midday")));
    }

    #[test]
    fn sanitize_is_injective_over_disallowed_bytes() {
        // Distinct external ids that previously collapsed to the same token must
        // now map to distinct tokens, so unrelated aggregates never merge.
        assert_ne!(sanitize_token("a/b"), sanitize_token("a_b"));
        assert_ne!(sanitize_token("a.b"), sanitize_token("a/b"));
        // Traversal is still neutralized: no separators, no "..".
        let cleaned = sanitize_token("../../etc/passwd");
        assert!(!cleaned.contains('/'));
        assert!(!cleaned.contains(".."));
    }
}
