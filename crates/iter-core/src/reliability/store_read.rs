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
    tier2_cells_from(&read_tier2_map(root), route_id, direction_id, stop_id)
}

/// [`read_tier2_cells`] over an already-parsed map. Identical logic, but the
/// caller owns the parse — used by the gateway's mtime-validated cache (ADR
/// 0032) to derive cells without re-reading `tier2.json` per request.
pub fn tier2_cells_from(
    map: &std::collections::BTreeMap<String, Tier2>,
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
    for (key, agg) in map {
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
            readout: Readout::of(agg),
        });
    }
    cells
}

/// A count-weighted on-time rate per `(route, direction, stop)`, reduced over
/// every (tod_bucket, day_type) cell of that stop — the whole Tier-2 archive in
/// one pass. Built for the reranker, which needs a cheap in-memory lookup keyed
/// by leg rather than a per-leg file read. Fail-soft and bounded: a missing or
/// corrupt store yields an empty map. Cells with no on-time rate (count 0) don't
/// contribute; a key with no contributing observation is omitted entirely, so a
/// lookup miss is the natural "no history" signal. The map keys carry the raw
/// (un-sanitized) route and stop tokens recovered from the store, so a leg keys
/// against it with the same `gtfsId` the writer recorded.
pub fn read_tier2_on_time_index(
    root: &Path,
) -> std::collections::HashMap<(String, i32, String), f64> {
    on_time_index_from(&read_tier2_map(root))
}

/// [`read_tier2_on_time_index`] over an already-parsed map — the gateway's
/// cache (ADR 0032) derives the reranker index from the cached map rather than
/// re-reading `tier2.json` per routing request.
pub fn on_time_index_from(
    map: &std::collections::BTreeMap<String, Tier2>,
) -> std::collections::HashMap<(String, i32, String), f64> {
    use std::collections::HashMap;
    // Accumulate (weighted sum, total count) per key, then divide.
    let mut acc: HashMap<(String, i32, String), (f64, u64)> = HashMap::new();
    for (key, agg) in map {
        let Some((route, direction, stop)) = split_stop_key(key) else {
            continue;
        };
        let readout = Readout::of(agg);
        let (Some(rate), count) = (readout.on_time_rate, readout.count) else {
            continue;
        };
        if count == 0 {
            continue;
        }
        let entry = acc.entry((route, direction, stop)).or_insert((0.0, 0));
        entry.0 += rate * count as f64;
        entry.1 += count;
    }
    acc.into_iter()
        .filter(|(_, (_, total))| *total > 0)
        .map(|(k, (weighted, total))| (k, weighted / total as f64))
        .collect()
}

/// A `(route, direction, stop)`'s typical delay, reduced over every
/// (tod_bucket, day_type) cell of that stop. Carries both the median (`p50_s`)
/// and the conservative tail (`p85_s`) in seconds, plus the total observation
/// count behind them, so a consumer can pick the percentile it wants and gate on
/// confidence. Built for the no-RT delay annotator (ADR 0030), which fills a
/// historical delay where OTP carries no live realtime signal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TypicalDelay {
    /// Median delay across the stop's cells, seconds. Negative is early.
    pub p50_s: f64,
    /// 85th-percentile (conservative) delay, seconds.
    pub p85_s: f64,
    /// Total observations behind the figures.
    pub count: u64,
}

/// A typical-delay lookup per `(route, direction, stop)`, reduced over every
/// (tod_bucket, day_type) cell of that stop — the whole Tier-2 archive in one
/// pass. The companion of [`read_tier2_on_time_index`] for the no-RT delay
/// annotator (ADR 0030): where the reranker needs an on-time *rate*, the
/// annotator needs a typical *delay* to surface. Each cell contributes its own
/// p50/p85 weighted by its observation count, so a busy slice dominates a quiet
/// one. Fail-soft and bounded: a missing or corrupt store yields an empty map.
/// Cells with no count don't contribute; a key with no contributing observation
/// is omitted entirely, so a lookup miss is the natural "no history" signal. The
/// map keys carry the raw (un-sanitized) route and stop tokens recovered from the
/// store, so a leg keys against it with the same `gtfsId` the writer recorded.
pub fn read_tier2_typical_delay_index(
    root: &Path,
) -> std::collections::HashMap<(String, i32, String), TypicalDelay> {
    typical_delay_index_from(&read_tier2_map(root))
}

/// [`read_tier2_typical_delay_index`] over an already-parsed map — the gateway's
/// cache (ADR 0032) derives the no-RT annotator index from the cached map
/// rather than re-reading `tier2.json` per routing request.
pub fn typical_delay_index_from(
    map: &std::collections::BTreeMap<String, Tier2>,
) -> std::collections::HashMap<(String, i32, String), TypicalDelay> {
    use std::collections::HashMap;
    // Accumulate (p50-weighted sum, p85-weighted sum, total count) per key.
    let mut acc: HashMap<(String, i32, String), (f64, f64, u64)> = HashMap::new();
    for (key, agg) in map {
        let Some((route, direction, stop)) = split_stop_key(key) else {
            continue;
        };
        let readout = Readout::of(agg);
        // A cell only contributes when it has observations *and* both percentiles
        // read back — a zero-count or degenerate cell would skew the mean.
        let (Some(p50), Some(p85), count) = (readout.p50_s, readout.p85_s, readout.count) else {
            continue;
        };
        if count == 0 {
            continue;
        }
        let entry = acc.entry((route, direction, stop)).or_insert((0.0, 0.0, 0));
        entry.0 += p50 * count as f64;
        entry.1 += p85 * count as f64;
        entry.2 += count;
    }
    acc.into_iter()
        .filter(|(_, (_, _, total))| *total > 0)
        .map(|(k, (p50_sum, p85_sum, total))| {
            let t = total as f64;
            (
                k,
                TypicalDelay {
                    p50_s: p50_sum / t,
                    p85_s: p85_sum / t,
                    count: total,
                },
            )
        })
        .collect()
}

/// Recover the `(route, direction, stop)` triple from a full Tier-2 key
/// `route/dir/stop/bucket/daytype`, reversing [`sanitize_token`] on the route and
/// stop fields. `None` when the key isn't the exact five-field leaf shape or a
/// field can't be parsed/desanitized.
fn split_stop_key(key: &str) -> Option<(String, i32, String)> {
    let mut parts = key.split('/');
    let (Some(route_tok), Some(dir_tok), Some(stop_tok), Some(_bucket), Some(_day), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return None;
    };
    let direction: i32 = dir_tok.parse().ok()?;
    Some((
        desanitize_token(route_tok)?,
        direction,
        desanitize_token(stop_tok)?,
    ))
}

/// Inverse of [`sanitize_token`]: decode the injective `+HH` byte escapes back to
/// the original token. `None` on a malformed escape (so a corrupt key is skipped
/// rather than yielding garbage). The lone non-injective input is the empty
/// token, which sanitizes to `_` — we decode `_` to a literal `_` (the realistic
/// case) rather than the empty string, since no feed id is empty.
fn desanitize_token(tok: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(tok.len());
    let mut chars = tok.bytes();
    while let Some(b) = chars.next() {
        if b == b'+' {
            let hi = chars.next()?;
            let lo = chars.next()?;
            let hex = |c: u8| match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'A'..=b'F' => Some(c - b'A' + 10),
                _ => None,
            };
            bytes.push(hex(hi)? << 4 | hex(lo)?);
        } else {
            bytes.push(b);
        }
    }
    String::from_utf8(bytes).ok()
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

    #[test]
    fn desanitize_round_trips_real_and_escaped_tokens() {
        for raw in ["MEA", "70001", "ATAC:MEA", "a/b", "x.y", "with space"] {
            assert_eq!(desanitize_token(&sanitize_token(raw)).as_deref(), Some(raw));
        }
        // A malformed escape is rejected rather than yielding garbage.
        assert_eq!(desanitize_token("+ZZ"), None);
        assert_eq!(desanitize_token("+9"), None);
    }

    #[test]
    fn on_time_index_weights_cells_by_count() {
        let tmp = tempfile::tempdir().unwrap();
        let mut map = BTreeMap::new();
        // Stop A, two slices: am-peak all on-time (rate 1.0, 3 obs), pm-peak all
        // late (rate 0.0, 1 obs) → count-weighted rate = 3/4 = 0.75.
        let mut am = Tier2::default();
        for _ in 0..3 {
            am.observe(0);
        }
        let mut pm = Tier2::default();
        pm.observe(600);
        map.insert(
            tier2_key("ATAC:MEA", 0, "70001", TodBucket::AmPeak, DayType::Weekday),
            am,
        );
        map.insert(
            tier2_key("ATAC:MEA", 0, "70001", TodBucket::PmPeak, DayType::Weekday),
            pm,
        );
        seed(tmp.path(), &map);

        let index = read_tier2_on_time_index(tmp.path());
        let rate = index
            .get(&("ATAC:MEA".to_string(), 0, "70001".to_string()))
            .copied()
            .expect("keyed by the raw gtfsId tokens");
        assert!((rate - 0.75).abs() < 1e-9, "weighted rate was {rate}");
    }

    #[test]
    fn on_time_index_is_empty_for_missing_or_corrupt_store() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_tier2_on_time_index(tmp.path()).is_empty());
        std::fs::write(tmp.path().join(TIER2_FILE), b"{ not json").unwrap();
        assert!(read_tier2_on_time_index(tmp.path()).is_empty());
    }

    #[test]
    fn typical_delay_index_is_count_weighted_and_keyed_by_raw_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let mut map = BTreeMap::new();
        // One slice, all delayed ~600s → p50 and p85 land in the high bins; the
        // key carries the raw `ATAC:MEA` gtfsId the writer recorded.
        let mut late = Tier2::default();
        for _ in 0..6 {
            late.observe(600);
        }
        map.insert(
            tier2_key("ATAC:MEA", 0, "70001", TodBucket::AmPeak, DayType::Weekday),
            late,
        );
        seed(tmp.path(), &map);

        let index = read_tier2_typical_delay_index(tmp.path());
        let td = index
            .get(&("ATAC:MEA".to_string(), 0, "70001".to_string()))
            .copied()
            .expect("keyed by the raw gtfsId tokens");
        assert_eq!(td.count, 6);
        // All observations sit at +600s, so both percentiles are positive and the
        // tail is at least the median.
        assert!(td.p50_s > 0.0, "p50 was {}", td.p50_s);
        assert!(td.p85_s >= td.p50_s, "p85 {} < p50 {}", td.p85_s, td.p50_s);
    }

    #[test]
    fn typical_delay_index_weights_busy_slices_more() {
        let tmp = tempfile::tempdir().unwrap();
        let mut map = BTreeMap::new();
        // am-peak: 10 obs all early (~-120s); pm-peak: 1 obs very late (~1800s).
        // The count-weighted p50 must lean toward the busy early slice, not sit at
        // the midpoint of the two cells.
        let mut early = Tier2::default();
        for _ in 0..10 {
            early.observe(-120);
        }
        let mut late = Tier2::default();
        late.observe(1800);
        map.insert(
            tier2_key("R", 0, "S", TodBucket::AmPeak, DayType::Weekday),
            early,
        );
        map.insert(
            tier2_key("R", 0, "S", TodBucket::PmPeak, DayType::Weekday),
            late,
        );
        seed(tmp.path(), &map);

        let index = read_tier2_typical_delay_index(tmp.path());
        let td = index
            .get(&("R".to_string(), 0, "S".to_string()))
            .copied()
            .unwrap();
        assert_eq!(td.count, 11);
        // The busy early slice dominates → the weighted p50 stays well below the
        // single late slice's contribution.
        assert!(td.p50_s < 600.0, "p50 leaned late: {}", td.p50_s);
    }

    #[test]
    fn typical_delay_index_is_empty_for_missing_or_corrupt_store() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_tier2_typical_delay_index(tmp.path()).is_empty());
        std::fs::write(tmp.path().join(TIER2_FILE), b"{ not json").unwrap();
        assert!(read_tier2_typical_delay_index(tmp.path()).is_empty());
    }
}
