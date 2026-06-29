//! Read side of the on-disk reliability archive — the half shared between the
//! worker (which also writes it) and the gateway (which only reads it). It owns
//! the **on-disk layout contract**: the Tier-2 filename, the sanitized key
//! shape, and a bounded, fail-soft read that resolves a
//! (route, direction, stop, tod_bucket, day_type) lookup to a [`Readout`].
//!
//! SECURITY: `route`/`direction`/`stop` reach this code verbatim from an
//! external caller (a feed on the write side, an HTTP path param on the read
//! side). Every component is run through [`sanitize_token`] before it is used to
//! build a key, and the key is a flat map key — never a filesystem path — so a
//! `../../` component can neither traverse the key space nor escape the
//! reliability dir. The Tier-2 file read is size-bounded so a corrupt or
//! oversized store can't exhaust memory.

use std::path::Path;

use super::rollup::{DayType, Readout, Tier2, TodBucket};

/// The permanent Tier-2 file, relative to the reliability root. The worker
/// writes it; the gateway reads it. This name is part of the on-disk contract.
pub const TIER2_FILE: &str = "tier2.json";

/// Upper bound on the Tier-2 file we will load into memory (16 MiB). Tier-2 is
/// tiny and bounded by design (one small record per route×stop×bucket×day-type),
/// so a file past this cap is treated as corrupt and read as empty rather than
/// risking an unbounded allocation from a hostile or damaged store.
pub const TIER2_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Sanitize an external key component to a safe path token, *injectively*: keeps
/// `[A-Za-z0-9_-]` verbatim and escapes every other byte — including `.`, path
/// separators, control chars, and NUL — as `+HH` (uppercase hex of each UTF-8
/// byte). The `+` escape marker is itself escaped, so the mapping is reversible
/// and distinct inputs never collide on the same token. Because `.` is always
/// escaped, a `..` traversal segment can never appear; the result also contains
/// no path separator, so it can never escape its parent dir. Empty input
/// collapses to a placeholder.
pub fn sanitize_token(raw: &str) -> String {
    if raw.is_empty() {
        return "_".to_string();
    }
    let mut out = String::with_capacity(raw.len());
    for &b in raw.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-') {
            out.push(b as char);
        } else {
            out.push('+');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

/// Build a Tier-2 record key from its components (no service-date; carries the
/// day-type instead). Each external component is sanitized so the key is a flat,
/// traversal-free token joined by `/` field separators.
pub fn tier2_key(
    route_id: &str,
    direction_id: i32,
    stop_id: &str,
    bucket: TodBucket,
    day_type: DayType,
) -> String {
    format!(
        "{}/{}/{}/{}/{}",
        sanitize_token(route_id),
        direction_id,
        sanitize_token(stop_id),
        bucket.token(),
        day_type.token(),
    )
}

/// Read the whole Tier-2 map from `root/tier2.json`, fail-soft and bounded: a
/// missing file is an empty map, an oversized or corrupt file is also an empty
/// map (never an error, never an unbounded read). Keys are the sanitized
/// `tier2_key` shape.
pub fn read_tier2_map(root: &Path) -> std::collections::BTreeMap<String, Tier2> {
    use std::collections::BTreeMap;
    let path = root.join(TIER2_FILE);
    // Bound the read: stat first, skip anything implausibly large.
    match std::fs::metadata(&path) {
        Ok(m) if m.len() > TIER2_MAX_BYTES => return BTreeMap::new(),
        Ok(_) => {}
        Err(_) => return BTreeMap::new(),
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return BTreeMap::new(),
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Resolve a single (route, direction, stop, tod_bucket, day_type) lookup to its
/// [`Readout`] from the Tier-2 archive under `root`. Fail-soft: an absent key, a
/// missing store, or a corrupt store all return `None` — never an error, never a
/// panic. The lookup sanitizes its components the same way the writer does, so a
/// hostile path param resolves to a normal (most likely absent) key.
pub fn read_tier2_readout(
    root: &Path,
    route_id: &str,
    direction_id: i32,
    stop_id: &str,
    bucket: TodBucket,
    day_type: DayType,
) -> Option<Readout> {
    let map = read_tier2_map(root);
    let key = tier2_key(route_id, direction_id, stop_id, bucket, day_type);
    map.get(&key).map(Readout::of)
}

/// One resolved Tier-2 cell: the (tod_bucket, day_type) slice tokens and the
/// [`Readout`] derived from that slice's aggregate.
pub struct Tier2Cell {
    pub tod_bucket: TodBucket,
    pub day_type: DayType,
    pub readout: Readout,
}

/// Read every Tier-2 cell stored for a (route, direction, stop), one per
/// (tod_bucket, day_type) slice that has history. Fail-soft and bounded: a
/// missing or corrupt store yields an empty vec. Components are sanitized the
/// same way the writer keys them, so the match is exact and a hostile path
/// param can only ever miss. Cells are returned in a stable (bucket, day-type)
/// order.
pub fn read_tier2_cells(
    root: &Path,
    route_id: &str,
    direction_id: i32,
    stop_id: &str,
) -> Vec<Tier2Cell> {
    // The key shape is `route/dir/stop/bucket/daytype`; we match the first three
    // sanitized components exactly and decode the trailing slice tokens.
    let prefix = format!(
        "{}/{}/{}/",
        sanitize_token(route_id),
        direction_id,
        sanitize_token(stop_id),
    );
    let mut cells = Vec::new();
    for (key, agg) in read_tier2_map(root) {
        let Some(rest) = key.strip_prefix(&prefix) else {
            continue;
        };
        let mut parts = rest.split('/');
        let (Some(bucket_tok), Some(day_tok), None) = (parts.next(), parts.next(), parts.next())
        else {
            continue; // Not a leaf cell of this exact (route, dir, stop).
        };
        let (Some(tod_bucket), Some(day_type)) = (
            TodBucket::from_token(bucket_tok),
            DayType::from_token(day_tok),
        ) else {
            continue;
        };
        cells.push(Tier2Cell {
            tod_bucket,
            day_type,
            readout: Readout::of(&agg),
        });
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reliability::rollup::Tier2;
    use std::collections::BTreeMap;

    fn seed(root: &Path, map: &BTreeMap<String, Tier2>) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join(TIER2_FILE), serde_json::to_vec(map).unwrap()).unwrap();
    }

    #[test]
    fn sanitize_neutralizes_traversal_and_separators() {
        assert_eq!(sanitize_token("MEA"), "MEA");
        assert_eq!(sanitize_token("70001"), "70001");
        assert_eq!(sanitize_token("ok_chars-only"), "ok_chars-only");
        let cleaned = sanitize_token("../../etc/passwd");
        assert!(!cleaned.contains('/'));
        assert!(!cleaned.contains(".."));
        assert!(!sanitize_token("a/b").contains('/'));
        assert!(!sanitize_token("..").contains(".."));
        assert_eq!(sanitize_token(""), "_");
    }

    #[test]
    fn sanitize_is_injective_over_disallowed_bytes() {
        assert_ne!(sanitize_token("a/b"), sanitize_token("a_b"));
        assert_ne!(sanitize_token("a.b"), sanitize_token("a/b"));
    }

    #[test]
    fn read_resolves_a_present_key() {
        let tmp = tempfile::tempdir().unwrap();
        let mut t = Tier2::default();
        for d in [0, 0, 600] {
            t.observe(d);
        }
        let mut map = BTreeMap::new();
        map.insert(
            tier2_key("MEA", 0, "70001", TodBucket::AmPeak, DayType::Weekday),
            t,
        );
        seed(tmp.path(), &map);

        let r = read_tier2_readout(
            tmp.path(),
            "MEA",
            0,
            "70001",
            TodBucket::AmPeak,
            DayType::Weekday,
        )
        .expect("present key resolves");
        assert_eq!(r.count, 3);
        assert!(r.on_time_rate.is_some());
    }

    #[test]
    fn read_of_absent_key_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), &BTreeMap::new());
        assert!(
            read_tier2_readout(
                tmp.path(),
                "NOPE",
                0,
                "0",
                TodBucket::Night,
                DayType::Saturday
            )
            .is_none()
        );
    }

    #[test]
    fn read_of_missing_store_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        // No tier2.json written at all.
        assert!(
            read_tier2_readout(
                tmp.path(),
                "MEA",
                0,
                "70001",
                TodBucket::AmPeak,
                DayType::Weekday
            )
            .is_none()
        );
    }

    #[test]
    fn read_of_corrupt_store_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(TIER2_FILE), b"{ not json").unwrap();
        assert!(
            read_tier2_readout(
                tmp.path(),
                "MEA",
                0,
                "70001",
                TodBucket::AmPeak,
                DayType::Weekday
            )
            .is_none()
        );
    }

    #[test]
    fn read_of_oversized_store_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        // A file past the cap is treated as corrupt, not loaded.
        let blob = vec![b' '; (TIER2_MAX_BYTES + 1) as usize];
        std::fs::write(tmp.path().join(TIER2_FILE), &blob).unwrap();
        assert!(read_tier2_map(tmp.path()).is_empty());
    }

    #[test]
    fn read_cells_returns_every_slice_of_a_stop() {
        let tmp = tempfile::tempdir().unwrap();
        let mut map = BTreeMap::new();
        // Two slices for the same stop, plus an unrelated stop that must not leak.
        let mut am = Tier2::default();
        am.observe(0);
        let mut pm = Tier2::default();
        pm.observe(600);
        map.insert(
            tier2_key("MEA", 0, "70001", TodBucket::AmPeak, DayType::Weekday),
            am,
        );
        map.insert(
            tier2_key("MEA", 0, "70001", TodBucket::PmPeak, DayType::Weekday),
            pm,
        );
        let mut other = Tier2::default();
        other.observe(0);
        map.insert(
            tier2_key("MEA", 0, "70002", TodBucket::AmPeak, DayType::Weekday),
            other,
        );
        seed(tmp.path(), &map);

        let cells = read_tier2_cells(tmp.path(), "MEA", 0, "70001");
        assert_eq!(cells.len(), 2, "only the two slices of 70001");
        assert!(cells.iter().any(|c| c.tod_bucket == TodBucket::AmPeak));
        assert!(cells.iter().any(|c| c.tod_bucket == TodBucket::PmPeak));
        assert!(cells.iter().all(|c| c.day_type == DayType::Weekday));
    }

    #[test]
    fn read_cells_of_absent_stop_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), &BTreeMap::new());
        assert!(read_tier2_cells(tmp.path(), "NOPE", 0, "0").is_empty());
        // A missing store is also empty, not an error.
        let tmp2 = tempfile::tempdir().unwrap();
        assert!(read_tier2_cells(tmp2.path(), "MEA", 0, "70001").is_empty());
    }

    #[test]
    fn read_cells_does_not_prefix_match_a_longer_stop() {
        // A stop whose token is a prefix of another must not bleed across.
        let tmp = tempfile::tempdir().unwrap();
        let mut map = BTreeMap::new();
        let mut a = Tier2::default();
        a.observe(0);
        map.insert(
            tier2_key("MEA", 0, "7", TodBucket::AmPeak, DayType::Weekday),
            a,
        );
        let mut b = Tier2::default();
        b.observe(0);
        map.insert(
            tier2_key("MEA", 0, "70", TodBucket::AmPeak, DayType::Weekday),
            b,
        );
        seed(tmp.path(), &map);
        assert_eq!(read_tier2_cells(tmp.path(), "MEA", 0, "7").len(), 1);
        assert_eq!(read_tier2_cells(tmp.path(), "MEA", 0, "70").len(), 1);
    }

    #[test]
    fn traversal_path_param_resolves_to_a_normal_absent_key() {
        let tmp = tempfile::tempdir().unwrap();
        // Seed one real key.
        let mut t = Tier2::default();
        t.observe(0);
        let mut map = BTreeMap::new();
        map.insert(
            tier2_key("MEA", 0, "70001", TodBucket::AmPeak, DayType::Weekday),
            t,
        );
        seed(tmp.path(), &map);

        // A traversal-shaped route param sanitizes to a flat token: it cannot
        // collide with the real key and cannot escape the key space.
        let key = tier2_key(
            "../../../../etc/passwd",
            0,
            "../../escape",
            TodBucket::AmPeak,
            DayType::Weekday,
        );
        assert!(!key.contains(".."), "key still carries traversal: {key}");
        assert!(
            read_tier2_readout(
                tmp.path(),
                "../../../../etc/passwd",
                0,
                "../../escape",
                TodBucket::AmPeak,
                DayType::Weekday
            )
            .is_none()
        );
    }
}
