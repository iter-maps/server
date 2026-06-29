//! Weather-aware itinerary reranking — the gateway's first outbound runtime data
//! dependency (ADR 0033). Two cooperating pieces live here:
//!
//! 1. The **pure, I/O-free factor** ([`weather_penalty`]): given an itinerary's
//!    legs and a [`Forecast`], it sums the journey's weather-*exposed* minutes
//!    (walking + outdoor waiting/transfer time) and multiplies by a
//!    `weather_badness` in `0.0..=1.0` derived from precipitation and extreme
//!    heat/cold. The result is a raw penalty (higher is worse) the composite
//!    reranker min-max-normalizes across the response like any other factor, so a
//!    bad-weather + high-exposure itinerary ranks **lower** while good weather or
//!    zero exposure contributes nothing. With no forecast (disabled or a failed
//!    fetch) every itinerary's penalty is `0.0`, so the factor is exactly neutral.
//!
//! 2. A thin **async client** ([`WeatherClient`]) over the keyless Open-Meteo
//!    forecast API plus a bounded TTL [`WeatherCache`]. The client is opt-in and
//!    default-off: an unset/empty base URL disables it (no call, neutral factor).
//!    It is short-timeout, fail-soft (any transport/timeout/parse error yields
//!    `None`), and sends only **coarse** (≈1 km, two-decimal) journey coordinates
//!    for privacy. The cache mirrors the Tier-2 read cache's lock discipline —
//!    the lock is never held across the `.await` that fetches.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

// --- weather-badness thresholds ---------------------------------------------
//
// These map a single hour's forecast to a `weather_badness` in `0.0..=1.0`. They
// are deliberate, documented comfort thresholds — what a pedestrian *feels*, not
// a meteorological standard — and the reranker only ever compares the resulting
// penalties relatively within one response, so the exact cutoffs matter less than
// their ordering. Precipitation dominates; temperature extremes add on top.

/// Precipitation (mm/h) at or above which walking outdoors is fully unpleasant
/// (badness saturates to its rain ceiling). Light drizzle sits well below this.
const RAIN_SATURATION_MM: f64 = 4.0;
/// The badness contributed by precipitation once it reaches saturation. Rain is
/// the strongest single signal, so it can drive badness near the top on its own.
const RAIN_MAX_BADNESS: f64 = 0.9;
/// Apparent-temperature comfort band (°C). Inside it, temperature adds no
/// badness; outside, badness ramps linearly toward the cold/heat ceilings.
const COMFORT_TEMP_MIN_C: f64 = 5.0;
const COMFORT_TEMP_MAX_C: f64 = 30.0;
/// Apparent temperature (°C) at/below which cold is fully unpleasant.
const COLD_SATURATION_C: f64 = -5.0;
/// Apparent temperature (°C) at/above which heat is fully unpleasant.
const HEAT_SATURATION_C: f64 = 38.0;
/// The badness contributed by a fully cold or fully hot hour.
const TEMP_MAX_BADNESS: f64 = 0.6;

/// One hour's forecast for a journey's origin, parsed from the Open-Meteo
/// response. Only the fields the factor needs are kept; everything else is
/// ignored so a richer upstream payload never breaks parsing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Forecast {
    /// Air temperature 2 m above ground, °C.
    pub temperature_c: f64,
    /// Precipitation for the hour, mm.
    pub precipitation_mm: f64,
    /// "Feels-like" apparent temperature, °C, when the upstream supplies it; the
    /// badness model prefers it and falls back to [`Forecast::temperature_c`].
    pub apparent_temperature_c: Option<f64>,
}

impl Forecast {
    /// The apparent (feels-like) temperature if present, else the air temperature.
    fn felt_temperature_c(&self) -> f64 {
        self.apparent_temperature_c.unwrap_or(self.temperature_c)
    }

    /// Map this hour's forecast to a `weather_badness` in `0.0..=1.0`, combining a
    /// precipitation term and a temperature-extreme term. `0.0` is a calm,
    /// comfortable hour; `1.0` is the worst this model represents. Non-finite
    /// inputs are treated as their neutral end (no badness) so a malformed value
    /// can never inflate the penalty.
    pub fn badness(&self) -> f64 {
        let rain = if self.precipitation_mm.is_finite() && self.precipitation_mm > 0.0 {
            (self.precipitation_mm / RAIN_SATURATION_MM).clamp(0.0, 1.0) * RAIN_MAX_BADNESS
        } else {
            0.0
        };

        let t = self.felt_temperature_c();
        let temp = if !t.is_finite() {
            0.0
        } else if t < COMFORT_TEMP_MIN_C {
            // Colder than comfortable: ramp from the band edge down to saturation.
            let span = COMFORT_TEMP_MIN_C - COLD_SATURATION_C;
            ((COMFORT_TEMP_MIN_C - t) / span).clamp(0.0, 1.0) * TEMP_MAX_BADNESS
        } else if t > COMFORT_TEMP_MAX_C {
            // Hotter than comfortable: ramp from the band edge up to saturation.
            let span = HEAT_SATURATION_C - COMFORT_TEMP_MAX_C;
            ((t - COMFORT_TEMP_MAX_C) / span).clamp(0.0, 1.0) * TEMP_MAX_BADNESS
        } else {
            0.0
        };

        // Combine without double-counting past the ceiling: the worse of the two
        // sets the floor, the other nudges it, capped at 1.0.
        (rain.max(temp) + rain.min(temp) * 0.5).min(1.0)
    }
}

/// The weather penalty for one itinerary: its weather-exposed minutes times the
/// forecast's badness. **Higher is worse.** Exposed time is the sum of walk-leg
/// durations and outdoor wait/transfer minutes between consecutive legs (the gaps
/// a traveler spends at a stop, in the open). Riding inside a vehicle is not
/// exposure. With `badness == 0.0` (good weather) or zero exposed time the penalty
/// is `0.0`, so the factor stays neutral for that itinerary. Total and panic-free:
/// a malformed itinerary yields `0.0`.
pub fn weather_penalty(itinerary: &Value, forecast: &Forecast) -> f64 {
    let badness = forecast.badness();
    if badness <= 0.0 {
        return 0.0;
    }
    let exposed_minutes = exposed_seconds(itinerary) / 60.0;
    exposed_minutes * badness
}

/// The weather-exposed time of an itinerary in seconds: every walk leg's duration
/// plus every outdoor gap between consecutive legs (a transfer/wait spent at a
/// stop). In-vehicle transit time is sheltered and excluded. Total: a missing or
/// malformed `legs` array, or legs without usable times, yield `0.0`.
fn exposed_seconds(itinerary: &Value) -> f64 {
    let Some(legs) = itinerary.get("legs").and_then(Value::as_array) else {
        return 0.0;
    };

    let mut exposed = 0.0;
    let mut prev_end: Option<f64> = None;

    for leg in legs {
        let mode = leg.get("mode").and_then(Value::as_str).unwrap_or("");

        // A walk leg is fully exposed.
        if mode == "WALK" {
            exposed += leg_duration_seconds(leg);
        }

        // The gap between the previous leg's end and this leg's start is outdoor
        // wait/transfer time — exposure spent at a stop. OTP reports leg endpoints
        // as epoch-millisecond `startTime`/`endTime`; when both are present and a
        // positive gap exists, count it. Missing/garbled times simply skip the gap.
        if let (Some(end), Some(start)) = (prev_end, leg_start_millis(leg)) {
            let gap_s = (start - end) / 1000.0;
            if gap_s.is_finite() && gap_s > 0.0 {
                exposed += gap_s;
            }
        }
        prev_end = leg_end_millis(leg);
    }

    exposed
}

/// A leg's duration in seconds (OTP reports leg duration in seconds), `0.0` when
/// absent or malformed.
fn leg_duration_seconds(leg: &Value) -> f64 {
    leg.get("duration").and_then(Value::as_f64).unwrap_or(0.0)
}

/// A leg's `startTime` (epoch milliseconds), if present and numeric.
fn leg_start_millis(leg: &Value) -> Option<f64> {
    leg.get("startTime").and_then(Value::as_f64)
}

/// A leg's `endTime` (epoch milliseconds), if present and numeric.
fn leg_end_millis(leg: &Value) -> Option<f64> {
    leg.get("endTime").and_then(Value::as_f64)
}

// --- coarse coordinates ------------------------------------------------------

/// Round a coordinate to two decimals (~1 km) before it leaves for a third party
/// or keys the cache. Coarsening the journey's origin is the privacy posture for
/// this opt-in path (ADR 0033): the weather is the same across a ~1 km cell, so
/// the rounding costs nothing in forecast quality.
pub fn round_coord(deg: f64) -> f64 {
    (deg * 100.0).round() / 100.0
}

/// The journey's origin coordinates for the forecast lookup: the `from.lat`/
/// `from.lon` of the first leg of the first itinerary, rounded coarse. Returns
/// `None` when the plan has no usable origin, in which case the caller skips the
/// fetch and the factor stays neutral. Total and panic-free.
pub fn plan_origin(plan: &Value) -> Option<(f64, f64)> {
    let first_leg = plan
        .get("data")?
        .get("plan")?
        .get("itineraries")?
        .as_array()?
        .first()?
        .get("legs")?
        .as_array()?
        .first()?;
    let from = first_leg.get("from")?;
    let lat = from.get("lat").and_then(Value::as_f64)?;
    let lon = from.get("lon").and_then(Value::as_f64)?;
    if !lat.is_finite() || !lon.is_finite() {
        return None;
    }
    Some((round_coord(lat), round_coord(lon)))
}

/// The journey's first-leg start time as whole UTC seconds since the epoch, if a
/// usable non-negative `startTime` (epoch milliseconds) is present. Shared by the
/// hour-of-day and the date derivations so they always agree on the same instant.
fn plan_start_secs(plan: &Value) -> Option<u64> {
    let ms = plan
        .get("data")
        .and_then(|d| d.get("plan"))
        .and_then(|p| p.get("itineraries"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|it| it.get("legs"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(leg_start_millis)?;
    (ms.is_finite() && ms >= 0.0).then(|| (ms / 1000.0) as u64)
}

/// The journey's hour-of-day index (0..=23) for the forecast lookup, taken from
/// the first leg's `startTime` (epoch milliseconds) as a UTC hour. Paired with
/// [`plan_date`], which pins the forecast window to the journey's day in UTC
/// (`&start_date=…&timezone=UTC`), so this hour-of-day correctly addresses the
/// journey's *own* day — not always day-0 of the response. Falls back to `0` when
/// no usable start time is present, so a plan without times still resolves a
/// (coarse) forecast rather than skipping the factor. Total and panic-free.
pub fn plan_hour(plan: &Value) -> usize {
    match plan_start_secs(plan) {
        Some(secs) => ((secs / 3600) % 24) as usize,
        None => 0,
    }
}

/// The journey's UTC calendar date as `YYYY-MM-DD`, used to pin the Open-Meteo
/// forecast window to the journey's own day (so a tomorrow-08:00 departure reads
/// tomorrow's 08:00 row, not today's). `None` when no usable start time is present,
/// in which case the client requests the default (today-anchored) window and the
/// hour-of-day still resolves a coarse forecast. Total and panic-free: a pure
/// civil-date conversion from whole UTC seconds, no external crate.
pub fn plan_date(plan: &Value) -> Option<String> {
    let secs = plan_start_secs(plan)?;
    Some(utc_ymd(secs))
}

/// Convert whole seconds since the Unix epoch to a UTC `YYYY-MM-DD` string via the
/// civil-from-days algorithm (Howard Hinnant's `civil_from_days`). Pure integer
/// math, valid across the full proleptic Gregorian range; no time-zone or crate.
fn utc_ymd(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    // Shift so the era starts on 0000-03-01, then unwind into y/m/d.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}")
}

// --- async client + cache ----------------------------------------------------

/// Default outbound timeout for the weather fetch. Short on purpose: the forecast
/// is a soft enhancement on the opt-in rerank path, never worth stalling a routing
/// response. A slow/hung upstream trips this and the factor degrades to neutral.
const WEATHER_TIMEOUT: Duration = Duration::from_secs(2);

/// TTL for a cached forecast. Hourly forecasts move slowly; a one-hour memo keeps
/// the gateway from hammering the upstream for every nearby journey in the same
/// cell and hour.
const WEATHER_TTL: Duration = Duration::from_secs(60 * 60);

/// Upper bound on distinct cells × hours retained. Small and fixed: the cache is
/// disposable soft-state, and a bound keeps a flood of distinct origins from
/// growing it without limit. On overflow the whole map is cleared (a coarse but
/// allocation-free eviction) — the worst case is a cold refetch.
const WEATHER_CACHE_CAP: usize = 4096;

/// Upper bound on the forecast body we will buffer (256 KiB). A real Open-Meteo
/// hourly forecast is a few KB; this leaves generous headroom while refusing to
/// allocate an unbounded body from a slow-but-fast-enough, misconfigured, or
/// hostile upstream within the timeout window. Mirrors the rerank path's
/// bounded-buffer discipline ([`crate::proxy`]'s `RERANK_MAX_BODY_BYTES`): a body
/// with no advertised length, or one over the cap, degrades to a neutral factor.
const WEATHER_MAX_BODY_BYTES: u64 = 256 * 1024;

/// A thin async client over the keyless Open-Meteo forecast API. The base URL is
/// configured (`WEATHER_API_URL`); an unset/empty URL disables weather entirely
/// (no client is built). Tests point `base_url` at a local stub.
#[derive(Clone)]
pub struct WeatherClient {
    http: reqwest::Client,
    base_url: String,
}

impl WeatherClient {
    /// Build a client over `base_url`, reusing the shared pooled `http`. The base
    /// URL is the Open-Meteo forecast endpoint (e.g.
    /// `https://api.open-meteo.com/v1/forecast`).
    pub fn new(http: reqwest::Client, base_url: String) -> Self {
        Self { http, base_url }
    }

    /// Fetch the forecast for `(lat, lon)` at `hour` (0..=23 index into the hourly
    /// arrays) on `date` (`Some("YYYY-MM-DD")` pins the forecast window to the
    /// journey's day in UTC so the hour-of-day addresses the right row; `None`
    /// requests the default today-anchored window). Fail-soft: any transport error,
    /// timeout, non-success status, oversized/length-less body, or unparsable body
    /// yields `None` so the caller's factor stays neutral and the rerank still
    /// completes. The request carries only the coarse coordinates it was handed;
    /// rounding is the caller's responsibility (see [`round_coord`]).
    pub async fn fetch(
        &self,
        lat: f64,
        lon: f64,
        hour: usize,
        date: Option<&str>,
    ) -> Option<Forecast> {
        let mut url = format!(
            "{}?latitude={lat}&longitude={lon}\
             &hourly=temperature_2m,precipitation,apparent_temperature",
            self.base_url
        );
        // Pin the window to the journey's UTC day so the hourly array starts at that
        // day's 00:00 UTC and `hour` (hour-of-day) selects the journey's own row.
        if let Some(date) = date {
            url.push_str("&timezone=UTC&start_date=");
            url.push_str(date);
            url.push_str("&end_date=");
            url.push_str(date);
        }
        let resp = self
            .http
            .get(&url)
            .timeout(WEATHER_TIMEOUT)
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        // Bound the buffered body: a forecast is a few KB, so refuse a body with no
        // advertised length or one over the cap before reading it. This keeps the
        // fail-soft fetch from allocating an unbounded response within the timeout.
        match resp.content_length() {
            Some(len) if len <= WEATHER_MAX_BODY_BYTES => {}
            _ => return None,
        }
        let body: Value = resp.json().await.ok()?;
        parse_forecast(&body, hour)
    }
}

/// Parse one hour out of an Open-Meteo `hourly` block into a [`Forecast`]. The
/// response shape is `{"hourly": {"temperature_2m": [...], "precipitation": [...],
/// "apparent_temperature": [...]}}`. Returns `None` when the required arrays or the
/// requested index are absent, so a malformed/empty body degrades to neutral.
/// `apparent_temperature` is optional — a body without it still parses.
fn parse_forecast(body: &Value, hour: usize) -> Option<Forecast> {
    let hourly = body.get("hourly")?;
    let temperature_c = hourly_at(hourly, "temperature_2m", hour)?;
    let precipitation_mm = hourly_at(hourly, "precipitation", hour).unwrap_or(0.0);
    let apparent_temperature_c = hourly_at(hourly, "apparent_temperature", hour);
    Some(Forecast {
        temperature_c,
        precipitation_mm,
        apparent_temperature_c,
    })
}

/// Read `hourly[field][index]` as an `f64`, or `None` when the field isn't an
/// array, the index is out of range, or the value isn't numeric.
fn hourly_at(hourly: &Value, field: &str, index: usize) -> Option<f64> {
    hourly
        .get(field)
        .and_then(Value::as_array)
        .and_then(|a| a.get(index))
        .and_then(Value::as_f64)
}

/// The cache key: quantized `(coarse-lat, coarse-lon, hour-of-day, UTC date)`.
/// Integer quantization avoids float-as-key fragility (see
/// [`WeatherCache::quantize`]); the date keeps same-hour journeys on different days
/// from colliding now that the forecast window is pinned to the journey's day.
type CacheKey = (i64, i64, usize, Option<String>);

/// A cached forecast plus the instant it was stored, for TTL expiry.
type CacheEntry = (Forecast, Instant);

/// A bounded, thread-safe TTL cache of forecasts keyed by `(coarse-lat, coarse-lon,
/// hour)`. Mirrors the Tier-2 read cache's discipline: the lock guards only the
/// in-memory map for cheap get/put and is **never** held across the network
/// `.await` — [`WeatherCache::get_or_fetch`] reads under the lock, releases it,
/// fetches, then re-locks to publish. A poisoned lock is recovered rather than
/// unwrapped so one panic can't wedge later requests. The cache holds no user
/// data — only coarse, public weather for a ~1 km cell.
pub struct WeatherCache {
    entries: Mutex<HashMap<CacheKey, CacheEntry>>,
    ttl: Duration,
    cap: usize,
}

impl Default for WeatherCache {
    fn default() -> Self {
        Self::new()
    }
}

impl WeatherCache {
    pub fn new() -> Self {
        Self::with_ttl_and_cap(WEATHER_TTL, WEATHER_CACHE_CAP)
    }

    /// Build a cache with explicit TTL and capacity. Production uses [`new`], which
    /// pins the 1 h TTL and 4096-entry cap; this seam lets tests drive the expiry
    /// and overflow-eviction branches deterministically (tiny TTL / tiny cap)
    /// without sleeping an hour or inserting thousands of keys.
    ///
    /// [`new`]: WeatherCache::new
    pub fn with_ttl_and_cap(ttl: Duration, cap: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
            cap,
        }
    }

    /// Quantize a coarse (already two-decimal) coordinate into an integer cache
    /// key, avoiding float-as-key fragility. `12.49` → `1249`.
    fn quantize(deg: f64) -> i64 {
        (deg * 100.0).round() as i64
    }

    /// A fresh cached forecast for the key, if any.
    fn fresh(&self, key: &CacheKey) -> Option<Forecast> {
        let entries = lock(&self.entries);
        entries
            .get(key)
            .and_then(|(f, at)| (at.elapsed() < self.ttl).then_some(*f))
    }

    /// Publish a freshly fetched forecast, evicting wholesale if the bound is hit.
    fn store(&self, key: CacheKey, forecast: Forecast) {
        let mut entries = lock(&self.entries);
        if entries.len() >= self.cap && !entries.contains_key(&key) {
            entries.clear();
        }
        entries.insert(key, (forecast, Instant::now()));
    }

    /// Return a fresh cached forecast for `(lat, lon, hour, date)`, else fetch it
    /// via `client`, cache it, and return it. `lat`/`lon` must already be coarse
    /// (see [`round_coord`]); `date` pins the forecast window to the journey's UTC
    /// day (see [`WeatherClient::fetch`]). Fail-soft: a failed fetch returns `None`
    /// and caches nothing, so the caller's factor stays neutral. The lock is
    /// released before the fetch `.await` and retaken only to publish — never held
    /// across the await.
    pub async fn get_or_fetch(
        &self,
        client: &WeatherClient,
        lat: f64,
        lon: f64,
        hour: usize,
        date: Option<&str>,
    ) -> Option<Forecast> {
        let key = (
            Self::quantize(lat),
            Self::quantize(lon),
            hour,
            date.map(str::to_owned),
        );
        if let Some(f) = self.fresh(&key) {
            return Some(f);
        }
        let forecast = client.fetch(lat, lon, hour, date).await?;
        self.store(key, forecast);
        Some(forecast)
    }
}

/// Lock the cache, recovering from a poisoned lock instead of unwrapping. The
/// guarded data is a plain map with no broken invariant, so recovering and reusing
/// it is always safe and keeps one panic from wedging every later request.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn calm() -> Forecast {
        Forecast {
            temperature_c: 18.0,
            precipitation_mm: 0.0,
            apparent_temperature_c: Some(18.0),
        }
    }

    fn rainy() -> Forecast {
        Forecast {
            temperature_c: 12.0,
            precipitation_mm: 6.0, // past saturation
            apparent_temperature_c: Some(11.0),
        }
    }

    /// A walk leg of `secs` seconds.
    fn walk(secs: f64) -> Value {
        json!({ "transitLeg": false, "mode": "WALK", "duration": secs })
    }

    /// A transit (in-vehicle, sheltered) leg with explicit start/end epoch millis.
    fn ride(start_ms: f64, end_ms: f64) -> Value {
        json!({
            "transitLeg": true, "mode": "BUS",
            "route": { "gtfsId": "R" }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s" } },
            "startTime": start_ms, "endTime": end_ms,
        })
    }

    fn itin(legs: Vec<Value>) -> Value {
        json!({ "legs": legs })
    }

    // --- badness model -------------------------------------------------------

    #[test]
    fn calm_weather_has_zero_badness() {
        assert_eq!(calm().badness(), 0.0);
    }

    #[test]
    fn rain_drives_badness_up() {
        assert!(rainy().badness() > 0.5, "heavy rain should be clearly bad");
        // Saturating rain alone reaches the rain ceiling at minimum.
        let pour = Forecast {
            temperature_c: 18.0,
            precipitation_mm: 100.0,
            apparent_temperature_c: Some(18.0),
        };
        assert!(pour.badness() >= RAIN_MAX_BADNESS - 1e-9);
        assert!(pour.badness() <= 1.0);
    }

    #[test]
    fn extreme_cold_and_heat_add_badness() {
        let cold = Forecast {
            temperature_c: -10.0,
            precipitation_mm: 0.0,
            apparent_temperature_c: Some(-10.0),
        };
        let hot = Forecast {
            temperature_c: 40.0,
            precipitation_mm: 0.0,
            apparent_temperature_c: Some(40.0),
        };
        assert!(cold.badness() > 0.0);
        assert!(hot.badness() > 0.0);
        // A comfortable temperature with no rain is neutral.
        assert_eq!(calm().badness(), 0.0);
    }

    #[test]
    fn apparent_temperature_is_preferred_over_air() {
        // Air temp is comfortable but it *feels* freezing → badness from the felt
        // value, proving the apparent temperature wins.
        let f = Forecast {
            temperature_c: 10.0,
            precipitation_mm: 0.0,
            apparent_temperature_c: Some(-8.0),
        };
        assert!(f.badness() > 0.0);
    }

    #[test]
    fn non_finite_forecast_fields_never_inflate_badness() {
        let f = Forecast {
            temperature_c: f64::NAN,
            precipitation_mm: f64::INFINITY,
            apparent_temperature_c: None,
        };
        let b = f.badness();
        assert!(b.is_finite());
        assert!((0.0..=1.0).contains(&b));
    }

    // --- exposed time + penalty ---------------------------------------------

    #[test]
    fn walk_minutes_are_exposed() {
        // 600s walk = 10 exposed minutes.
        let it = itin(vec![walk(600.0)]);
        assert_eq!(exposed_seconds(&it), 600.0);
    }

    #[test]
    fn in_vehicle_time_is_not_exposed() {
        // A single ride leg with no preceding leg → no walk, no gap → zero exposure.
        let it = itin(vec![ride(1_000_000.0, 1_600_000.0)]);
        assert_eq!(exposed_seconds(&it), 0.0);
    }

    #[test]
    fn transfer_gap_between_legs_is_exposed() {
        // Ride A ends at t=1_600_000ms; ride B starts at t=1_900_000ms → a 300s
        // outdoor wait counts as exposure even though both legs are sheltered rides.
        let it = itin(vec![
            ride(1_000_000.0, 1_600_000.0),
            ride(1_900_000.0, 2_500_000.0),
        ]);
        assert_eq!(exposed_seconds(&it), 300.0);
    }

    #[test]
    fn more_exposure_in_bad_weather_ranks_lower() {
        // Same bad forecast: the itinerary with more exposed time gets the larger
        // penalty (which the reranker turns into a lower benefit).
        let little = itin(vec![walk(120.0)]);
        let lots = itin(vec![walk(1800.0)]);
        let p_little = weather_penalty(&little, &rainy());
        let p_lots = weather_penalty(&lots, &rainy());
        assert!(p_lots > p_little);
        assert!(p_little > 0.0);
    }

    #[test]
    fn good_weather_is_a_neutral_zero_penalty() {
        // Even a long walk costs nothing when the weather is calm.
        let lots = itin(vec![walk(3600.0)]);
        assert_eq!(weather_penalty(&lots, &calm()), 0.0);
    }

    #[test]
    fn zero_exposure_is_a_neutral_zero_penalty() {
        // A fully sheltered single ride in foul weather still scores zero — there
        // is no exposure to penalize.
        let ride_only = itin(vec![ride(1_000_000.0, 1_600_000.0)]);
        assert_eq!(weather_penalty(&ride_only, &rainy()), 0.0);
    }

    #[test]
    fn malformed_itinerary_never_panics_and_scores_zero() {
        assert_eq!(weather_penalty(&json!(7), &rainy()), 0.0);
        assert_eq!(weather_penalty(&json!({ "legs": "nope" }), &rainy()), 0.0);
        assert_eq!(weather_penalty(&json!({ "no": "legs" }), &rainy()), 0.0);
        // Legs present but garbled entries.
        let garbled = json!({ "legs": [7, { "mode": 3 }] });
        assert_eq!(weather_penalty(&garbled, &rainy()), 0.0);
    }

    // --- coarse coordinates --------------------------------------------------

    #[test]
    fn coordinates_are_rounded_to_two_decimals() {
        assert_eq!(round_coord(41.90278), 41.9);
        assert_eq!(round_coord(12.49636), 12.5);
        assert_eq!(round_coord(-0.123456), -0.12);
    }

    #[test]
    fn plan_origin_reads_the_first_legs_from_coords_coarsely() {
        let plan = json!({ "data": { "plan": { "itineraries": [
            { "legs": [{ "from": { "lat": 41.90278, "lon": 12.49636 } }] }
        ]}}});
        assert_eq!(plan_origin(&plan), Some((41.9, 12.5)));
    }

    #[test]
    fn plan_origin_is_none_without_usable_coords() {
        assert_eq!(plan_origin(&json!({})), None);
        assert_eq!(plan_origin(&json!({ "data": { "plan": {} } })), None);
        let no_coords = json!({ "data": { "plan": { "itineraries": [
            { "legs": [{ "from": {} }] }
        ]}}});
        assert_eq!(plan_origin(&no_coords), None);
    }

    #[test]
    fn plan_hour_reads_the_first_legs_utc_hour() {
        // 1_800_000_000_000 ms = 2027-01-15T08:00:00Z → hour 8.
        let plan = json!({ "data": { "plan": { "itineraries": [
            { "legs": [{ "startTime": 1_800_000_000_000.0_f64, "from": { "lat": 41.9, "lon": 12.5 } }] }
        ]}}});
        assert_eq!(plan_hour(&plan), 8);
    }

    #[test]
    fn plan_hour_defaults_to_zero_without_a_start_time() {
        assert_eq!(plan_hour(&json!({})), 0);
        let no_time = json!({ "data": { "plan": { "itineraries": [
            { "legs": [{ "from": { "lat": 41.9, "lon": 12.5 } }] }
        ]}}});
        assert_eq!(plan_hour(&no_time), 0);
    }

    // --- forecast parsing ----------------------------------------------------

    #[test]
    fn parses_a_canned_open_meteo_body() {
        // A trimmed Open-Meteo forecast body: hourly arrays indexed by hour.
        let body = json!({
            "latitude": 41.9, "longitude": 12.5,
            "hourly": {
                "time": ["2026-06-29T00:00", "2026-06-29T01:00", "2026-06-29T02:00"],
                "temperature_2m": [16.0, 15.5, 22.0],
                "precipitation": [0.0, 0.2, 3.0],
                "apparent_temperature": [15.0, 14.0, 21.0],
            }
        });
        let f = parse_forecast(&body, 2).unwrap();
        assert_eq!(f.temperature_c, 22.0);
        assert_eq!(f.precipitation_mm, 3.0);
        assert_eq!(f.apparent_temperature_c, Some(21.0));
    }

    #[test]
    fn parse_tolerates_missing_apparent_and_precipitation() {
        let body = json!({ "hourly": { "temperature_2m": [10.0, 11.0] } });
        let f = parse_forecast(&body, 1).unwrap();
        assert_eq!(f.temperature_c, 11.0);
        assert_eq!(f.precipitation_mm, 0.0); // defaulted
        assert_eq!(f.apparent_temperature_c, None);
    }

    #[test]
    fn parse_is_none_for_an_empty_or_short_body() {
        assert!(parse_forecast(&json!({}), 0).is_none());
        assert!(parse_forecast(&json!({ "hourly": {} }), 0).is_none());
        // Index past the array end → None, never a panic.
        let short = json!({ "hourly": { "temperature_2m": [1.0] } });
        assert!(parse_forecast(&short, 5).is_none());
    }

    // --- client + cache (local stub, no real network) ------------------------

    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Spin a local forecast stub that answers every request with `body`, counting
    /// the requests it serves. Returns its base URL and the shared counter.
    async fn counting_stub(
        body: &'static str,
    ) -> (String, Arc<AtomicU32>, tokio::task::JoinHandle<()>) {
        use axum::Router;
        use axum::routing::get;
        let hits = Arc::new(AtomicU32::new(0));
        let counter = hits.clone();
        let app = Router::new().route(
            "/",
            get(move || {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    body
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/"), hits, handle)
    }

    const CANNED_BODY: &str = r#"{"hourly":{"temperature_2m":[16.0,20.0,21.0],"precipitation":[0.0,0.0,0.0],"apparent_temperature":[15.0,19.0,20.0]}}"#;

    #[tokio::test]
    async fn client_parses_a_canned_forecast_from_a_stub() {
        let (base, hits, _h) = counting_stub(CANNED_BODY).await;
        let client = WeatherClient::new(reqwest::Client::new(), base);
        let f = client.fetch(41.9, 12.5, 1, None).await.expect("parsed");
        assert_eq!(f.temperature_c, 20.0);
        assert_eq!(f.apparent_temperature_c, Some(19.0));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cache_avoids_a_second_fetch_within_ttl() {
        let (base, hits, _h) = counting_stub(CANNED_BODY).await;
        let client = WeatherClient::new(reqwest::Client::new(), base);
        let cache = WeatherCache::new();
        let a = cache
            .get_or_fetch(&client, 41.9, 12.5, 1, None)
            .await
            .unwrap();
        let b = cache
            .get_or_fetch(&client, 41.9, 12.5, 1, None)
            .await
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(hits.load(Ordering::SeqCst), 1, "second call hit the cache");
    }

    #[tokio::test]
    async fn cache_distinct_keys_each_fetch() {
        // A different coarse cell, hour, or date is a distinct key → a fresh fetch.
        let (base, hits, _h) = counting_stub(CANNED_BODY).await;
        let client = WeatherClient::new(reqwest::Client::new(), base);
        let cache = WeatherCache::new();
        cache
            .get_or_fetch(&client, 41.9, 12.5, 1, None)
            .await
            .unwrap();
        cache
            .get_or_fetch(&client, 41.9, 12.5, 2, None)
            .await
            .unwrap(); // different hour
        cache
            .get_or_fetch(&client, 45.0, 9.0, 1, None)
            .await
            .unwrap(); // different cell
        cache
            .get_or_fetch(&client, 41.9, 12.5, 1, Some("2026-07-01"))
            .await
            .unwrap(); // different date — same hour/cell must not collide
        assert_eq!(hits.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn cache_refetches_after_ttl_expiry() {
        // The expiry branch: with a tiny TTL the entry goes stale, so a second
        // get_or_fetch past the TTL hits the stub again rather than serving stale.
        let (base, hits, _h) = counting_stub(CANNED_BODY).await;
        let client = WeatherClient::new(reqwest::Client::new(), base);
        let cache = WeatherCache::with_ttl_and_cap(Duration::from_millis(20), WEATHER_CACHE_CAP);
        cache
            .get_or_fetch(&client, 41.9, 12.5, 1, None)
            .await
            .unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        tokio::time::sleep(Duration::from_millis(40)).await;
        cache
            .get_or_fetch(&client, 41.9, 12.5, 1, None)
            .await
            .unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 2, "stale entry should refetch");
    }

    #[tokio::test]
    async fn cache_clears_wholesale_on_overflow() {
        // The clear-on-overflow eviction: with cap 2, inserting a 3rd distinct key
        // clears the map first, so an earlier key misses and refetches afterward.
        let (base, hits, _h) = counting_stub(CANNED_BODY).await;
        let client = WeatherClient::new(reqwest::Client::new(), base);
        let cache = WeatherCache::with_ttl_and_cap(WEATHER_TTL, 2);
        cache
            .get_or_fetch(&client, 41.9, 12.5, 0, None)
            .await
            .unwrap(); // key A
        cache
            .get_or_fetch(&client, 41.9, 12.5, 1, None)
            .await
            .unwrap(); // key B (now full)
        cache
            .get_or_fetch(&client, 41.9, 12.5, 2, None)
            .await
            .unwrap(); // key C clears, count 3
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        // Key A was evicted by the wholesale clear, so it refetches → 4 hits.
        cache
            .get_or_fetch(&client, 41.9, 12.5, 0, None)
            .await
            .unwrap();
        assert_eq!(
            hits.load(Ordering::SeqCst),
            4,
            "cleared key A should refetch"
        );
    }

    #[tokio::test]
    async fn fetch_pins_the_journey_day_in_the_request() {
        // With a date, the request pins the forecast window to that UTC day so the
        // hour-of-day indexes the journey's own day, not always today (day-0).
        use std::sync::Mutex;
        let recorder: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let rec = recorder.clone();
        let app = axum::Router::new().route(
            "/",
            axum::routing::get(move |axum::extract::RawQuery(q): axum::extract::RawQuery| {
                let rec = rec.clone();
                async move {
                    *rec.lock().unwrap() = q;
                    CANNED_BODY
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _h = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = WeatherClient::new(reqwest::Client::new(), format!("http://{addr}/"));

        client
            .fetch(41.9, 12.5, 1, Some("2026-07-15"))
            .await
            .expect("parsed");
        let q = recorder.lock().unwrap().clone().expect("stub was called");
        assert!(
            q.contains("start_date=2026-07-15"),
            "missing start_date: {q}"
        );
        assert!(q.contains("end_date=2026-07-15"), "missing end_date: {q}");
        assert!(q.contains("timezone=UTC"), "missing timezone: {q}");
    }

    #[tokio::test]
    async fn client_is_fail_soft_on_a_500() {
        use axum::Router;
        use axum::http::StatusCode;
        use axum::routing::get;
        let app = Router::new().route(
            "/",
            get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _h = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = WeatherClient::new(reqwest::Client::new(), format!("http://{addr}/"));
        // A non-success status yields None, and the cache caches nothing.
        let cache = WeatherCache::new();
        assert!(
            cache
                .get_or_fetch(&client, 41.9, 12.5, 0, None)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn client_is_fail_soft_on_an_unparsable_body() {
        let (base, _hits, _h) = counting_stub("{ not json").await;
        let client = WeatherClient::new(reqwest::Client::new(), base);
        assert!(client.fetch(41.9, 12.5, 0, None).await.is_none());
    }

    #[tokio::test]
    async fn client_rejects_an_oversized_body() {
        // A body declaring more than the cap is refused before reading, degrading to
        // a neutral factor — the buffered body is always bounded (findings on the
        // unbounded-response gap). Served with an explicit oversized Content-Length.
        use axum::Router;
        use axum::http::header;
        use axum::routing::get;
        let big = "x".repeat((WEATHER_MAX_BODY_BYTES as usize) + 1);
        let app = Router::new().route(
            "/",
            get(move || {
                let big = big.clone();
                async move { ([(header::CONTENT_TYPE, "application/json")], big) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _h = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = WeatherClient::new(reqwest::Client::new(), format!("http://{addr}/"));
        assert!(
            client.fetch(41.9, 12.5, 0, None).await.is_none(),
            "an oversized body must degrade to None, not buffer"
        );
    }

    #[tokio::test]
    async fn client_is_fail_soft_on_a_dead_upstream() {
        // A dead loopback port → connection refused (then the short timeout) → None,
        // never a panic or a stall that outlives the timeout.
        let client = WeatherClient::new(reqwest::Client::new(), "http://127.0.0.1:1/".to_string());
        assert!(client.fetch(41.9, 12.5, 0, None).await.is_none());
    }

    // --- plan date (journey-day pinning) -------------------------------------

    #[test]
    fn plan_date_reads_the_first_legs_utc_day() {
        // 1_800_000_000_000 ms = 2027-01-15T08:00:00Z → date 2027-01-15, hour 8.
        let plan = json!({ "data": { "plan": { "itineraries": [
            { "legs": [{ "startTime": 1_800_000_000_000.0_f64, "from": { "lat": 41.9, "lon": 12.5 } }] }
        ]}}});
        assert_eq!(plan_date(&plan).as_deref(), Some("2027-01-15"));
        assert_eq!(plan_hour(&plan), 8);
    }

    #[test]
    fn plan_date_is_none_without_a_start_time() {
        assert_eq!(plan_date(&json!({})), None);
        let no_time = json!({ "data": { "plan": { "itineraries": [
            { "legs": [{ "from": { "lat": 41.9, "lon": 12.5 } }] }
        ]}}});
        assert_eq!(plan_date(&no_time), None);
    }

    #[test]
    fn utc_ymd_converts_known_instants() {
        assert_eq!(utc_ymd(0), "1970-01-01");
        assert_eq!(utc_ymd(86_399), "1970-01-01"); // last second of the day
        assert_eq!(utc_ymd(86_400), "1970-01-02"); // next day
        // A leap day and a year boundary.
        assert_eq!(utc_ymd(1_582_934_400), "2020-02-29");
        assert_eq!(utc_ymd(1_609_459_199), "2020-12-31");
        assert_eq!(utc_ymd(1_609_459_200), "2021-01-01");
    }
}
