//! Pure, I/O-free rollup core for the reliability archive. Folds derived
//! stop-events (concept doc 23) into mergeable aggregates over three tiers, and
//! reads percentiles + on-time rate back out. Everything here is a plain
//! function or struct with no filesystem touch, so the merge algebra is fully
//! unit-testable; the store adapter (`reliability::store`) wires it to disk.
//!
//! The aggregate state is **mergeable**: two `Tier1`/`Tier2` records over
//! disjoint (or overlapping) event sets combine associatively, so the daily
//! fold and the permanent Tier-2 roll never need the raw events again.

use serde::{Deserialize, Serialize};

/// On-time window: a stop is "on time" when its delay is within [-60s, +300s]
/// (early by ≤1 min, late by ≤5 min). From concept doc 23.
pub const ON_TIME_MIN_S: i32 = -60;
pub const ON_TIME_MAX_S: i32 = 300;

/// Histogram bin edges (upper bounds, seconds). A delay falls in the first bin
/// whose upper bound it does not exceed; anything past the last edge lands in a
/// final overflow bin. Bins are fixed so two histograms always share a layout
/// and merge bin-for-bin. Negative (early) and positive (late) delays both have
/// resolution where it matters around the on-time window.
const BIN_EDGES_S: &[i32] = &[-300, -120, -60, 0, 60, 120, 180, 300, 600, 900, 1800, 3600];

/// Number of histogram buckets: one per edge plus the overflow bucket.
pub const N_BINS: usize = BIN_EDGES_S.len() + 1;

/// A bounded, fixed-layout, mergeable delay histogram for percentile estimation.
/// Counts are per bin; merging is element-wise addition, which is associative
/// and commutative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Histogram {
    pub bins: [u64; N_BINS],
}

impl Default for Histogram {
    fn default() -> Self {
        Self { bins: [0; N_BINS] }
    }
}

impl Histogram {
    pub fn new() -> Self {
        Self::default()
    }

    /// Index of the bin a delay falls into.
    fn bin_of(delay_s: i32) -> usize {
        for (i, &edge) in BIN_EDGES_S.iter().enumerate() {
            if delay_s <= edge {
                return i;
            }
        }
        BIN_EDGES_S.len()
    }

    pub fn observe(&mut self, delay_s: i32) {
        self.bins[Self::bin_of(delay_s)] += 1;
    }

    /// Total observations recorded.
    pub fn count(&self) -> u64 {
        self.bins.iter().sum()
    }

    /// Merge another histogram into this one, bin-for-bin. Associative and
    /// commutative — the load-bearing property for tiered rollups.
    pub fn merge(&mut self, other: &Histogram) {
        for (a, b) in self.bins.iter_mut().zip(other.bins.iter()) {
            *a = a.saturating_add(*b);
        }
    }

    /// Estimate the q-quantile (0.0..=1.0) of the delay distribution. Linear
    /// interpolation within the containing bin, using the bin's [lower, upper]
    /// edge bounds; the overflow bin is reported at its lower edge (we can't
    /// know how far past it the tail runs). `None` when empty.
    pub fn quantile(&self, q: f64) -> Option<f64> {
        let total = self.count();
        if total == 0 {
            return None;
        }
        let q = q.clamp(0.0, 1.0);
        let target = q * total as f64;

        let mut cum = 0u64;
        for (i, &c) in self.bins.iter().enumerate() {
            let prev = cum;
            cum += c;
            if c == 0 || (cum as f64) < target {
                continue;
            }
            // The target rank lands inside bin `i`. Interpolate across the bin's
            // span using how far into the bin the target rank sits.
            let (lo, hi) = Self::edges(i);
            let into_bin = (target - prev as f64) / c as f64; // 0.0..=1.0
            return Some(lo + (hi - lo) * into_bin);
        }
        // q == 1.0 with everything counted; report the last populated edge.
        Self::edges(N_BINS - 1).0.into()
    }

    /// [lower, upper] edge of bin `i` as f64 seconds. The first bin's lower edge
    /// and the overflow bin's upper edge are open; we clamp them to the nearest
    /// real edge so interpolation stays bounded.
    fn edges(i: usize) -> (f64, f64) {
        let lo = if i == 0 {
            BIN_EDGES_S[0] as f64
        } else {
            BIN_EDGES_S[i - 1] as f64
        };
        let hi = if i < BIN_EDGES_S.len() {
            BIN_EDGES_S[i] as f64
        } else {
            *BIN_EDGES_S.last().unwrap_or(&0) as f64
        };
        (lo, hi)
    }
}

/// Time-of-day buckets (concept doc 23). Derived from the local wall-clock hour
/// of the observation; we don't carry a finer timestamp than the service-date,
/// so the bucket is assigned at fold time from the feed timestamp where present
/// (see `tod_bucket_from_hour`). Six coarse buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodBucket {
    Early,   // 00:00–05:59
    AmPeak,  // 06:00–08:59
    Midday,  // 09:00–14:59
    PmPeak,  // 15:00–18:59
    Evening, // 19:00–21:59
    Night,   // 22:00–23:59
}

impl TodBucket {
    /// Stable token used in record keys and on disk.
    pub fn token(self) -> &'static str {
        match self {
            TodBucket::Early => "early",
            TodBucket::AmPeak => "am-peak",
            TodBucket::Midday => "midday",
            TodBucket::PmPeak => "pm-peak",
            TodBucket::Evening => "evening",
            TodBucket::Night => "night",
        }
    }
}

/// Bucket a local hour-of-day (0..=23). Hours outside that range clamp to Night.
pub fn tod_bucket_from_hour(hour: i32) -> TodBucket {
    match hour {
        0..=5 => TodBucket::Early,
        6..=8 => TodBucket::AmPeak,
        9..=14 => TodBucket::Midday,
        15..=18 => TodBucket::PmPeak,
        19..=21 => TodBucket::Evening,
        _ => TodBucket::Night,
    }
}

/// Day-type classes (concept doc 23). Sunday and public holidays collapse
/// together because they share a reduced-service profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayType {
    Weekday,
    Saturday,
    SundayOrHoliday,
}

impl DayType {
    pub fn token(self) -> &'static str {
        match self {
            DayType::Weekday => "weekday",
            DayType::Saturday => "saturday",
            DayType::SundayOrHoliday => "sunday-holiday",
        }
    }
}

/// Italian national public holidays with a fixed (month, day):
///   Jan 1 (Capodanno), Jan 6 (Epifania), Apr 25 (Liberazione),
///   May 1 (Festa del Lavoro), Jun 2 (Festa della Repubblica),
///   Aug 15 (Ferragosto), Nov 1 (Ognissanti), Dec 8 (Immacolata),
///   Dec 25 (Natale), Dec 26 (Santo Stefano).
/// The one movable feast that is a national reduced-service day, Easter Monday
/// (Lunedì dell'Angelo), is derived per-year via `easter_monday` rather than
/// listed here.
const IT_FIXED_HOLIDAYS: &[(u32, u32)] = &[
    (1, 1),
    (1, 6),
    (4, 25),
    (5, 1),
    (6, 2),
    (8, 15),
    (11, 1),
    (12, 8),
    (12, 25),
    (12, 26),
];

/// True if `(month, day)` is one of the encoded fixed Italian public holidays.
pub fn is_it_fixed_holiday(month: u32, day: u32) -> bool {
    IT_FIXED_HOLIDAYS.contains(&(month, day))
}

/// Easter Sunday `(month, day)` for a Gregorian `year`, via the Anonymous
/// Gregorian (Meeus/Jones/Butcher) computus. Valid for any Gregorian year.
fn easter_sunday(year: i64) -> (u32, u32) {
    let a = year % 19;
    let b = year / 100;
    let c = year % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let month = (h + l - 7 * m + 114) / 31;
    let day = (h + l - 7 * m + 114) % 31 + 1;
    (month as u32, day as u32)
}

/// True if `(month, day)` is Easter Monday (Lunedì dell'Angelo) in `year` — the
/// movable national reduced-service day. Derived from the computus.
fn is_easter_monday(year: i64, month: u32, day: u32) -> bool {
    let (em, ed) = easter_sunday(year);
    // Easter Monday is the day after Easter Sunday. Adding one day can roll into
    // the next month (Easter Sunday on the 30th/31st); resolve via the day count.
    let Some(sunday) = days_from_ymd(&format!("{year:04}{em:02}{ed:02}")) else {
        return false;
    };
    let monday = sunday + 1;
    let monday_ymd = ymd_from_days(monday);
    monday_ymd == format!("{year:04}{month:02}{day:02}")
}

/// Days since 1970-01-01 → `YYYYMMDD` (Howard Hinnant's `civil_from_days`).
fn ymd_from_days(z: i64) -> String {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}")
}

/// True if `year` is a Gregorian leap year.
fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// The number of days in `month` (1..=12) of `year`. Returns 0 for an
/// out-of-range month so the caller rejects it.
fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// `YYYYMMDD` → days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
/// Returns `None` on a malformed date — service-dates come from an external
/// feed, so this is fail-soft.
pub fn days_from_ymd(s: &str) -> Option<i64> {
    if s.len() != 8 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let y: i64 = s[0..4].parse().ok()?;
    let m: i64 = s[4..6].parse().ok()?;
    let d: i64 = s[6..8].parse().ok()?;
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    let y = y - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe - 719468)
}

/// Derive the `DayType` for a `YYYYMMDD` service-date, calendar-aware over the
/// encoded fixed Italian holidays. `None` on a malformed date.
pub fn day_type_of(service_date: &str) -> Option<DayType> {
    let days = days_from_ymd(service_date)?;
    let year: i64 = service_date.get(0..4)?.parse().ok()?;
    let month: u32 = service_date.get(4..6)?.parse().ok()?;
    let day: u32 = service_date.get(6..8)?.parse().ok()?;
    if is_it_fixed_holiday(month, day) || is_easter_monday(year, month, day) {
        return Some(DayType::SundayOrHoliday);
    }
    // 1970-01-01 was a Thursday → weekday index 3 (Mon=0..Sun=6).
    let dow = (days + 3).rem_euclid(7);
    Some(match dow {
        5 => DayType::Saturday,
        6 => DayType::SundayOrHoliday,
        _ => DayType::Weekday,
    })
}

/// Mergeable warm aggregate (Tier-1): one per
/// (route, direction, stop, service_date, tod_bucket). Holds enough moments to
/// recover mean/variance plus the histogram for percentiles. Merging is field-
/// wise and associative.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tier1 {
    pub count: u64,
    pub sum_delay: i64,
    pub sum_delay_sq: i128,
    pub min: i32,
    pub max: i32,
    pub on_time_count: u64,
    pub hist: Histogram,
}

impl Default for Tier1 {
    fn default() -> Self {
        Self {
            count: 0,
            sum_delay: 0,
            sum_delay_sq: 0,
            min: i32::MAX,
            max: i32::MIN,
            on_time_count: 0,
            hist: Histogram::new(),
        }
    }
}

/// True if a delay falls in the on-time window [-60s, +300s].
pub fn is_on_time(delay_s: i32) -> bool {
    (ON_TIME_MIN_S..=ON_TIME_MAX_S).contains(&delay_s)
}

impl Tier1 {
    /// Fold a single delay observation into the aggregate.
    pub fn observe(&mut self, delay_s: i32) {
        self.count += 1;
        self.sum_delay += i64::from(delay_s);
        self.sum_delay_sq += i128::from(delay_s) * i128::from(delay_s);
        self.min = self.min.min(delay_s);
        self.max = self.max.max(delay_s);
        if is_on_time(delay_s) {
            self.on_time_count += 1;
        }
        self.hist.observe(delay_s);
    }

    /// Merge another aggregate into this one (associative, commutative).
    pub fn merge(&mut self, other: &Tier1) {
        if other.count == 0 {
            return;
        }
        self.count += other.count;
        self.sum_delay += other.sum_delay;
        self.sum_delay_sq += other.sum_delay_sq;
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
        self.on_time_count += other.on_time_count;
        self.hist.merge(&other.hist);
    }
}

/// Permanent cold aggregate (Tier-2): one per
/// (route, direction, stop, tod_bucket, day_type) — bounded, does not grow with
/// time. The merge over all history; what a future reranker reads. Structurally
/// identical to `Tier1`, so the fold reuses the same algebra.
pub type Tier2 = Tier1;

/// Read-side summary derived from an aggregate: percentiles + on-time rate.
/// This is the surface a future reranker / gateway read-endpoint consumes; the
/// rollup job already logs a merged overview built from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Readout {
    pub count: u64,
    pub p50_s: Option<f64>,
    pub p85_s: Option<f64>,
    pub p90_s: Option<f64>,
    pub on_time_rate: Option<f64>,
    pub mean_s: Option<f64>,
}

impl Readout {
    /// Derive the metrics a reranker/UI reads from a Tier-1/Tier-2 aggregate.
    pub fn of(agg: &Tier1) -> Self {
        let (mean, on_time_rate) = if agg.count > 0 {
            (
                Some(agg.sum_delay as f64 / agg.count as f64),
                Some(agg.on_time_count as f64 / agg.count as f64),
            )
        } else {
            (None, None)
        };
        Self {
            count: agg.count,
            p50_s: agg.hist.quantile(0.50),
            p85_s: agg.hist.quantile(0.85),
            p90_s: agg.hist.quantile(0.90),
            on_time_rate,
            mean_s: mean,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_merge_is_associative_and_commutative() {
        let build = |samples: &[i32]| {
            let mut h = Histogram::new();
            for &s in samples {
                h.observe(s);
            }
            h
        };
        let a = build(&[-200, -50, 0, 30, 90]);
        let b = build(&[120, 400, 700, 2000]);
        let c = build(&[10, 10, 10, 5000]);

        // (a ∪ b) ∪ c
        let mut left = a.clone();
        left.merge(&b);
        left.merge(&c);
        // a ∪ (b ∪ c)
        let mut bc = b.clone();
        bc.merge(&c);
        let mut right = a.clone();
        right.merge(&bc);
        assert_eq!(left, right, "merge must be associative");

        // a ∪ b == b ∪ a
        let mut ab = a.clone();
        ab.merge(&b);
        let mut ba = b.clone();
        ba.merge(&a);
        assert_eq!(ab, ba, "merge must be commutative");

        // Total count is preserved across merges.
        assert_eq!(left.count(), 5 + 4 + 4);
    }

    #[test]
    fn merged_histogram_equals_one_built_from_all_samples() {
        let mut whole = Histogram::new();
        for s in [-300, -61, -60, 0, 60, 300, 301, 4000] {
            whole.observe(s);
        }
        let mut p1 = Histogram::new();
        for s in [-300, -61, -60, 0] {
            p1.observe(s);
        }
        let mut p2 = Histogram::new();
        for s in [60, 300, 301, 4000] {
            p2.observe(s);
        }
        p1.merge(&p2);
        assert_eq!(whole, p1);
    }

    #[test]
    fn quantile_on_a_known_distribution() {
        // 100 evenly-spread late delays 0..=990 in steps of 10.
        let mut h = Histogram::new();
        for i in 0..100 {
            h.observe(i * 10);
        }
        let p50 = h.quantile(0.50).unwrap();
        let p90 = h.quantile(0.90).unwrap();
        // Median ~500s, p90 ~900s; bin interpolation keeps us within a bin width.
        assert!((300.0..=700.0).contains(&p50), "p50 was {p50}");
        assert!((600.0..=1000.0).contains(&p90), "p90 was {p90}");
        assert!(p90 >= p50);
    }

    #[test]
    fn quantile_is_none_when_empty() {
        assert_eq!(Histogram::new().quantile(0.5), None);
    }

    #[test]
    fn quantile_in_the_overflow_bin_reports_the_lower_edge() {
        // All observations are beyond the last edge (3600s) → the tail is
        // reported at the overflow bin's lower edge, finite and non-panicking.
        let mut h = Histogram::new();
        for d in [5000, 8000, 12000] {
            h.observe(d);
        }
        let overflow_lo = 3600.0;
        assert_eq!(h.quantile(1.0), Some(overflow_lo));
        assert_eq!(h.quantile(0.99), Some(overflow_lo));
        assert_eq!(h.quantile(0.5), Some(overflow_lo));
    }

    #[test]
    fn bin_layout_around_the_on_time_window_is_stable() {
        // The on-time edges (-60, +300) are exact bin boundaries; pin the bins so
        // an off-by-one in bin_of around the window is caught. Edges are upper
        // bounds: a value lands in the first bin whose edge it does not exceed.
        // BIN_EDGES_S = [-300,-120,-60,0,60,120,180,300,600,900,1800,3600].
        assert_eq!(Histogram::bin_of(-60), 2); // <= -60 → bin 2 (edge -60)
        assert_eq!(Histogram::bin_of(0), 3); // <= 0 → bin 3 (edge 0)
        assert_eq!(Histogram::bin_of(300), 7); // <= 300 → bin 7 (edge 300)
        assert_eq!(Histogram::bin_of(301), 8); // > 300 → next bin (edge 600)
        assert_eq!(Histogram::bin_of(10_000), N_BINS - 1); // overflow
    }

    #[test]
    fn on_time_window_classification() {
        assert!(!is_on_time(-61));
        assert!(is_on_time(-60));
        assert!(is_on_time(0));
        assert!(is_on_time(300));
        assert!(!is_on_time(301));
    }

    #[test]
    fn tod_buckets_cover_the_day() {
        assert_eq!(tod_bucket_from_hour(0), TodBucket::Early);
        assert_eq!(tod_bucket_from_hour(7), TodBucket::AmPeak);
        assert_eq!(tod_bucket_from_hour(12), TodBucket::Midday);
        assert_eq!(tod_bucket_from_hour(17), TodBucket::PmPeak);
        assert_eq!(tod_bucket_from_hour(20), TodBucket::Evening);
        assert_eq!(tod_bucket_from_hour(23), TodBucket::Night);
        // Out-of-range clamps to Night rather than panicking.
        assert_eq!(tod_bucket_from_hour(99), TodBucket::Night);
    }

    #[test]
    fn day_type_derivation_including_a_holiday() {
        // 2026-06-29 is a Monday.
        assert_eq!(day_type_of("20260629"), Some(DayType::Weekday));
        // 2026-06-27 is a Saturday.
        assert_eq!(day_type_of("20260627"), Some(DayType::Saturday));
        // 2026-06-28 is a Sunday.
        assert_eq!(day_type_of("20260628"), Some(DayType::SundayOrHoliday));
        // 2026-12-25 (Natale) is a Friday but classifies as holiday.
        assert_eq!(day_type_of("20261225"), Some(DayType::SundayOrHoliday));
        // Malformed input is fail-soft.
        assert_eq!(day_type_of("notadate"), None);
        assert_eq!(day_type_of("20261301"), None);
    }

    #[test]
    fn day_type_holiday_check_wins_over_the_weekday_branch() {
        // Aug 15 2026 (Ferragosto) is a Saturday — it must classify as
        // SundayOrHoliday, not Saturday, proving the holiday check runs first.
        assert_eq!(day_type_of("20260815"), Some(DayType::SundayOrHoliday));
        // Apr 25 2026 (Liberazione) is a Saturday too.
        assert_eq!(day_type_of("20260425"), Some(DayType::SundayOrHoliday));
        // Jun 2 2026 (Festa della Repubblica) is a Tuesday — a non-Friday weekday.
        assert_eq!(day_type_of("20260602"), Some(DayType::SundayOrHoliday));
    }

    #[test]
    fn easter_monday_is_a_holiday() {
        // Easter Sunday 2026 is Apr 5; Easter Monday is Apr 6 (a Monday).
        assert_eq!(easter_sunday(2026), (4, 5));
        assert_eq!(day_type_of("20260406"), Some(DayType::SundayOrHoliday));
        // The surrounding Tuesday is a plain weekday.
        assert_eq!(day_type_of("20260407"), Some(DayType::Weekday));
        // Easter Monday 2024 was Apr 1 (Easter Sunday Mar 31).
        assert_eq!(easter_sunday(2024), (3, 31));
        assert_eq!(day_type_of("20240401"), Some(DayType::SundayOrHoliday));
    }

    #[test]
    fn invalid_day_of_month_is_rejected() {
        // Feb 31 must not silently roll into March.
        assert_eq!(days_from_ymd("20260231"), None);
        assert_eq!(day_type_of("20260231"), None);
        // Feb 29 is valid only in a leap year.
        assert!(days_from_ymd("20240229").is_some());
        assert_eq!(days_from_ymd("20260229"), None);
        // 30-day months reject day 31.
        assert_eq!(days_from_ymd("20260431"), None);
        assert!(days_from_ymd("20260430").is_some());
    }

    #[test]
    fn tier1_fold_matches_hand_computed_moments() {
        let mut t = Tier1::default();
        for d in [-60, 0, 300, 600] {
            t.observe(d);
        }
        assert_eq!(t.count, 4);
        assert_eq!(t.sum_delay, 840);
        assert_eq!(t.sum_delay_sq, 60 * 60 + 300 * 300 + 600 * 600);
        assert_eq!(t.min, -60);
        assert_eq!(t.max, 600);
        // -60, 0, 300 are on-time; 600 is not.
        assert_eq!(t.on_time_count, 3);
    }

    #[test]
    fn tier1_to_tier2_merge_is_associative() {
        let build = |ds: &[i32]| {
            let mut t = Tier1::default();
            for &d in ds {
                t.observe(d);
            }
            t
        };
        let a = build(&[-60, 0, 120]);
        let b = build(&[300, 305, 900]);
        let c = build(&[10, 20, 30]);

        let mut left: Tier2 = Tier2::default();
        left.merge(&a);
        left.merge(&b);
        left.merge(&c);

        let mut right: Tier2 = Tier2::default();
        let mut bc = b.clone();
        bc.merge(&c);
        right.merge(&a);
        right.merge(&bc);

        assert_eq!(left, right);
        assert_eq!(left.count, 9);
        // Merged moments equal one aggregate folded from all nine samples.
        let whole = build(&[-60, 0, 120, 300, 305, 900, 10, 20, 30]);
        assert_eq!(left, whole);
    }

    #[test]
    fn merging_empty_aggregate_is_a_noop() {
        let mut t = Tier1::default();
        for d in [10, 20] {
            t.observe(d);
        }
        let snapshot = t.clone();
        t.merge(&Tier1::default());
        assert_eq!(t, snapshot);
    }

    #[test]
    fn readout_reports_percentiles_and_on_time_rate() {
        let mut t = Tier1::default();
        // 10 on-time (0s), 10 very late (1000s) → on-time rate 0.5.
        for _ in 0..10 {
            t.observe(0);
        }
        for _ in 0..10 {
            t.observe(1000);
        }
        let r = Readout::of(&t);
        assert_eq!(r.count, 20);
        assert_eq!(r.on_time_rate, Some(0.5));
        assert_eq!(r.mean_s, Some(500.0));
        assert!(r.p50_s.is_some());
        assert!(r.p90_s.unwrap() >= r.p50_s.unwrap());
    }

    #[test]
    fn readout_of_empty_is_none() {
        let r = Readout::of(&Tier1::default());
        assert_eq!(r.count, 0);
        assert_eq!(r.on_time_rate, None);
        assert_eq!(r.mean_s, None);
        assert_eq!(r.p50_s, None);
    }
}
