//! Weather-aware itinerary reranking — the gateway's first outbound runtime data
//! dependency (ADR 0033, 0035, 0036). Two cooperating pieces live here:
//!
//! 1. The **pure, I/O-free factor**: given an itinerary's legs and the journey's
//!    sampled weather, it scores the journey's exposure split by weather *type*
//!    (ADR 0035) and **per segment** (ADR 0036). Each exposed segment — a walk
//!    leg, an outdoor wait/transfer, the in-vehicle ride — is scored against the
//!    forecast **local to that segment's place and time**, not a single
//!    journey-origin forecast: a cross-town transfer hub reads its own cell and
//!    its own (later) hour, and the destination reads the arrival cell.
//!    **Precipitation** hits truly-outdoor minutes only (walking + outdoor
//!    waiting/transfer time) — every vehicle, bus included, keeps the rain off.
//!    **Temperature** (driven by apparent/feels-like, folding UV and wind, ADR
//!    0036) also hits in-vehicle minutes, scaled by a per-mode climate-control
//!    coefficient (air-conditioned rail/metro near-sheltered; bus/tram partially
//!    exposed). Each segment's exposure is multiplied by its own local badness and
//!    summed into a raw penalty (higher is worse) the composite reranker
//!    min-max-normalizes across the response like any other factor, so a
//!    bad-weather + high-exposure itinerary ranks **lower** while good weather or
//!    zero exposure contributes nothing. With no sampled weather (disabled or a
//!    failed fetch) every itinerary's penalty is `0.0`, so the factor is neutral.
//!    [`weather_penalty`] remains as the single-point (origin-only) form for the
//!    continuity contract; [`weather_penalty_multi`] is the per-segment form.
//!
//! 2. A thin **async client** ([`WeatherClient`]) over the keyless Open-Meteo
//!    forecast API plus a bounded TTL [`WeatherCache`]. The client is opt-in and
//!    default-off: an unset/empty base URL disables it (no call, neutral factor).
//!    It fetches the journey's distinct, cache-miss sample points in **one**
//!    multi-location request (Open-Meteo accepts comma-separated coordinates and
//!    returns a parallel array), bounded to the journey's date range. It is
//!    short-timeout, fail-soft (any transport/timeout/parse error, or a missing
//!    point in the array, yields a neutral segment), and sends only **coarse**
//!    (≈1 km, two-decimal) coordinates for privacy. The cache mirrors the Tier-2
//!    read cache's lock discipline — the lock is never held across the fetch
//!    `.await`.

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use iter_core::reliability::rollup::days_from_ymd;
use serde_json::Value;

use crate::legkey::is_transit_leg;

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

// --- UV and wind comfort thresholds (ADR 0036) ------------------------------
//
// Wave 2 folds two more outdoor-comfort signals into the temperature bucket: a
// long sunny midday wait burns under high UV, and wind drives windchill / pushes
// rain sideways. Like the precipitation and temperature ramps these are deliberate
// pedestrian-comfort thresholds, only ever compared relatively within one
// response. Both fold into the *temperature* (felt-comfort) badness component:
// they describe how the outdoor air feels, not whether it is raining.

/// UV index at/above which standing outdoors is fully unpleasant (badness
/// saturates to its UV ceiling). The WHO "very high / extreme" band starts here.
const UV_SATURATION: f64 = 8.0;
/// The badness contributed by saturating UV. A secondary signal — strong sun adds
/// discomfort but never dominates an actual cold/heat extreme on its own.
const UV_MAX_BADNESS: f64 = 0.25;
/// Wind speed (km/h, Open-Meteo's `wind_speed_10m` default unit) at/above which
/// wind is fully unpleasant (badness saturates to its wind ceiling). Roughly a
/// strong breeze / near-gale where walking and waiting get notably worse.
const WIND_SATURATION_KMH: f64 = 40.0;
/// The badness contributed by saturating wind. Like UV a secondary signal that
/// nudges the felt-comfort badness without overruling a temperature extreme.
const WIND_MAX_BADNESS: f64 = 0.25;

// --- per-mode temperature exposure coefficients (ADR 0035) ------------------
//
// In-vehicle time is sheltered from *rain* for every mode, but temperature
// comfort depends on the vehicle's climate control. These coefficients scale a
// transit leg's in-vehicle minutes into temperature-exposed minutes: 0.0 means
// the cabin fully neutralizes the outside temperature, 1.0 means the rider feels
// the outdoor extreme as if standing in it. They are a **deliberate heuristic** —
// we have no per-vehicle A/C data — chosen from the typical fleet reality that
// metro/rail run reliably climate-controlled while many buses and trams do not.
// Like the carbon and badness constants they are only ever compared relatively
// within one response, so the ordering matters more than the exact values, and
// retuning them later is a non-breaking change.

/// Heavy/commuter/regional rail and metro/subway: reliably air-conditioned, so
/// almost none of the outdoor temperature reaches the rider.
const TEMP_COEFF_RAIL: f64 = 0.1;
/// Bus and tram: frequently weak or no climate control, so a meaningful share of
/// the outdoor temperature is felt in the cabin.
const TEMP_COEFF_BUS_TRAM: f64 = 0.4;
/// Any unrecognized motorized mode: a conservative mid coefficient so an unknown
/// mode neither escapes the temperature penalty nor dominates it.
const TEMP_COEFF_UNKNOWN: f64 = 0.3;

/// Map an OTP transit leg `mode` to its temperature-exposure coefficient (ADR
/// 0035). Air-conditioned rail/metro are near-sheltered; bus/tram are partially
/// exposed; an unrecognized mode gets a conservative mid value. A non-transit mode
/// never reaches this — its outdoor minutes are counted in full elsewhere.
fn mode_temp_coeff(mode: &str) -> f64 {
    match mode {
        "RAIL" | "TRAIN" | "SUBWAY" | "METRO" => TEMP_COEFF_RAIL,
        "BUS" | "TROLLEYBUS" | "COACH" | "TRAM" | "LIGHT_RAIL" => TEMP_COEFF_BUS_TRAM,
        _ => TEMP_COEFF_UNKNOWN,
    }
}

/// One hour's forecast for a single sample point, parsed from the Open-Meteo
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
    /// UV index for the hour, when supplied (ADR 0036). Folds into the felt-comfort
    /// badness — a long sunny outdoor wait is worse. `None` simply contributes
    /// nothing, so an older/partial body still scores.
    pub uv_index: Option<f64>,
    /// Wind speed 10 m above ground (km/h), when supplied (ADR 0036). Folds into
    /// the felt-comfort badness — windchill / driving rain. `None` contributes
    /// nothing.
    pub wind_speed_kmh: Option<f64>,
}

impl Forecast {
    /// The apparent (feels-like) temperature if present, else the air temperature.
    fn felt_temperature_c(&self) -> f64 {
        self.apparent_temperature_c.unwrap_or(self.temperature_c)
    }

    /// The precipitation badness component in `0.0..=1.0`: how unpleasant this
    /// hour's rain is on its own, independent of temperature. `0.0` is dry; it
    /// ramps to `RAIN_MAX_BADNESS` at `RAIN_SATURATION_MM` and beyond. A
    /// non-finite or non-positive value is treated as dry so a malformed reading
    /// can never inflate the penalty. Precipitation only hits truly-outdoor time
    /// (ADR 0035), so this is the badness applied to the precip exposure bucket.
    pub fn precip_badness(&self) -> f64 {
        if self.precipitation_mm.is_finite() && self.precipitation_mm > 0.0 {
            (self.precipitation_mm / RAIN_SATURATION_MM).clamp(0.0, 1.0) * RAIN_MAX_BADNESS
        } else {
            0.0
        }
    }

    /// The felt-comfort badness component in `0.0..=1.0`: how unpleasant the air
    /// *feels* on its own, independent of rain. It folds the apparent-temperature
    /// extreme (the dominant signal) with the UV and wind secondary signals (ADR
    /// 0036): the temperature ramp sets the floor, then UV and wind nudge it up,
    /// capped at 1.0. A non-finite/absent reading for any signal contributes
    /// nothing, so a partial body never inflates the penalty. This reaches
    /// in-vehicle time too (a hot bus is still hot), scaled per mode by the
    /// climate-control coefficient (ADR 0035), so it is the badness applied to the
    /// temperature exposure bucket.
    pub fn temp_badness(&self) -> f64 {
        let thermal = self.thermal_badness();
        let uv = self.uv_badness();
        let wind = self.wind_badness();
        // Thermal extremes dominate; UV and wind are secondary nudges that lift the
        // felt discomfort without overruling an actual cold/heat extreme.
        (thermal.max(uv).max(wind) + (uv + wind) * 0.5).min(1.0)
    }

    /// The apparent-temperature extreme badness in `0.0..=1.0`: `0.0` inside the
    /// comfort band, ramping toward `TEMP_MAX_BADNESS` at the cold/heat saturation
    /// edges. A non-finite value is treated as comfortable.
    fn thermal_badness(&self) -> f64 {
        let t = self.felt_temperature_c();
        if !t.is_finite() {
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
        }
    }

    /// The UV badness in `0.0..=1.0` (ADR 0036): `0.0` below the band, ramping to
    /// `UV_MAX_BADNESS` at `UV_SATURATION`. Absent/non-finite/non-positive → `0.0`.
    fn uv_badness(&self) -> f64 {
        match self.uv_index {
            Some(uv) if uv.is_finite() && uv > 0.0 => {
                (uv / UV_SATURATION).clamp(0.0, 1.0) * UV_MAX_BADNESS
            }
            _ => 0.0,
        }
    }

    /// The wind badness in `0.0..=1.0` (ADR 0036): `0.0` below the band, ramping to
    /// `WIND_MAX_BADNESS` at `WIND_SATURATION_KMH`. Absent/non-finite/non-positive
    /// → `0.0`.
    fn wind_badness(&self) -> f64 {
        match self.wind_speed_kmh {
            Some(w) if w.is_finite() && w > 0.0 => {
                (w / WIND_SATURATION_KMH).clamp(0.0, 1.0) * WIND_MAX_BADNESS
            }
            _ => 0.0,
        }
    }

    /// The combined `weather_badness` in `0.0..=1.0` for a single hour, folding the
    /// precipitation and temperature components together: the worse of the two sets
    /// the floor, the other nudges it, capped at 1.0. The split penalty model (ADR
    /// 0035) applies the two components to their own exposure buckets, but this
    /// combined scalar remains the single-hour "how bad overall" summary.
    pub fn badness(&self) -> f64 {
        let rain = self.precip_badness();
        let temp = self.temp_badness();
        // Combine without double-counting past the ceiling: the worse of the two
        // sets the floor, the other nudges it, capped at 1.0.
        (rain.max(temp) + rain.min(temp) * 0.5).min(1.0)
    }
}

/// The weather penalty for one itinerary, split by weather type (ADR 0035).
/// **Higher is worse.**
///
/// ```text
/// penalty = precip_badness * precip_exposure + temp_badness * temp_exposure
/// ```
///
/// where exposure is in minutes:
///
/// - **precip exposure** is truly-outdoor time only — walk legs plus outdoor
///   wait/transfer gaps. In-vehicle time is sheltered from rain for *every* mode
///   (a bus keeps the rain off), so it never enters this bucket.
/// - **temp exposure** is the same outdoor time *plus* each transit leg's
///   in-vehicle minutes scaled by its mode's climate-control coefficient
///   (`mode_temp_coeff`): a hot/cold cabin still reaches the rider, more on a
///   bus/tram than on air-conditioned rail/metro.
///
/// With both badness components `0.0` (good weather) or zero exposure the penalty
/// is `0.0`, so the factor stays neutral for that itinerary. Total and
/// panic-free: a malformed itinerary yields `0.0`.
pub fn weather_penalty(itinerary: &Value, forecast: &Forecast) -> f64 {
    let precip_badness = forecast.precip_badness();
    let temp_badness = forecast.temp_badness();
    if precip_badness <= 0.0 && temp_badness <= 0.0 {
        return 0.0;
    }
    let exposure = exposure_seconds(itinerary);
    let precip_minutes = exposure.precip / 60.0;
    let temp_minutes = exposure.temp / 60.0;
    precip_badness * precip_minutes + temp_badness * temp_minutes
}

/// An itinerary's two weather-exposure buckets, in seconds (ADR 0035).
struct Exposure {
    /// Truly-outdoor seconds: walk legs + outdoor wait/transfer gaps. Rain only
    /// reaches the traveler here.
    precip: f64,
    /// `precip` seconds plus each transit leg's in-vehicle seconds scaled by its
    /// mode temperature coefficient. Temperature reaches in-vehicle time too.
    temp: f64,
}

/// Split an itinerary into its precipitation- and temperature-exposure seconds in
/// one pass over the legs. Outdoor time (walk legs + outdoor wait/transfer gaps)
/// counts toward both buckets; a transit leg's in-vehicle duration adds to the
/// temperature bucket only, scaled by its mode coefficient. Total: a missing or
/// malformed `legs` array, or legs without usable times, yield zero on both.
fn exposure_seconds(itinerary: &Value) -> Exposure {
    let Some(legs) = itinerary.get("legs").and_then(Value::as_array) else {
        return Exposure {
            precip: 0.0,
            temp: 0.0,
        };
    };

    let mut outdoor = 0.0;
    let mut in_vehicle_temp = 0.0;
    let mut prev_end: Option<f64> = None;

    for leg in legs {
        let mode = leg.get("mode").and_then(Value::as_str).unwrap_or("");

        if mode == "WALK" {
            // A walk leg is fully outdoor.
            outdoor += leg_duration_seconds(leg);
        } else if is_transit_leg(leg) {
            // In-vehicle time is rain-sheltered for every mode, but the cabin's
            // temperature comfort depends on the mode — scale it accordingly.
            in_vehicle_temp += leg_duration_seconds(leg) * mode_temp_coeff(mode);
        }

        // The gap between the previous leg's end and this leg's start is outdoor
        // wait/transfer time — exposure spent at a stop. OTP reports leg endpoints
        // as epoch-millisecond `startTime`/`endTime`; when both are present and a
        // positive gap exists, count it. Missing/garbled times simply skip the gap.
        if let (Some(end), Some(start)) = (prev_end, leg_start_millis(leg)) {
            let gap_s = (start - end) / 1000.0;
            if gap_s.is_finite() && gap_s > 0.0 {
                outdoor += gap_s;
            }
        }
        prev_end = leg_end_millis(leg);
    }

    Exposure {
        precip: outdoor,
        temp: outdoor + in_vehicle_temp,
    }
}

// --- per-segment multi-point sampling (ADR 0036) ----------------------------
//
// Wave 1 sampled one forecast at the journey origin and applied it to every
// segment. Wave 2 samples each EXPOSED segment — each walk leg, each outdoor
// wait/transfer, each in-vehicle ride — against the forecast LOCAL to that
// segment's place and time: a cross-town transfer reads its own cell and its own
// later hour; the destination reads the arrival cell. The same per-segment walk
// drives both the penalty (`weather_penalty_multi`) and the set of distinct
// sample points the client fetches (`journey_sample_points`), so they never
// disagree on where/when a segment is sampled.

/// A point-and-hour the forecast is sampled at: the quantized coarse cell, the
/// UTC hour-of-day, and the UTC date. The same key shape the per-point cache uses,
/// so a journey's sample points and the cache speak the same language.
pub type SampleKey = (i64, i64, usize, Option<String>);

/// The journey's sampled weather: a forecast per distinct sample point, keyed by
/// [`SampleKey`]. Built from the multi-location fetch + per-point cache (ADR 0036).
/// A missing key (a point that failed to resolve, or a partial response) simply
/// leaves that segment neutral — the penalty falls back to `0.0` for it.
pub type JourneyWeather = HashMap<SampleKey, Forecast>;

/// Quantize a coarse (already two-decimal) coordinate into an integer key
/// component. `12.49` → `1249`.
fn quantize_coord(deg: f64) -> i64 {
    (deg * 100.0).round() as i64
}

/// Build the [`SampleKey`] for a sample point at `(lat, lon)` and absolute epoch
/// `time_ms`, or `None` when the coordinates aren't finite. The coordinates are
/// coarsened (~1 km, ADR 0033) before quantizing; the hour-of-day and UTC date come
/// from the segment's own absolute time (ADR 0034/0036), so a later transfer keys a
/// later hour and a multi-day trip keys the right day. A missing/garbled time pins
/// hour 0 with no date (the default window).
fn sample_key(lat: f64, lon: f64, time_ms: Option<f64>) -> Option<SampleKey> {
    if !lat.is_finite() || !lon.is_finite() {
        return None;
    }
    let (q_lat, q_lon) = (
        quantize_coord(round_coord(lat)),
        quantize_coord(round_coord(lon)),
    );
    let secs = time_ms.filter(|ms| ms.is_finite() && *ms >= 0.0);
    match secs {
        Some(ms) => {
            let s = (ms / 1000.0) as u64;
            let hour = ((s / 3600) % 24) as usize;
            Some((q_lat, q_lon, hour, Some(utc_ymd(s))))
        }
        None => Some((q_lat, q_lon, 0, None)),
    }
}

/// A leg endpoint's coordinates (`from`/`to` → `lat`/`lon`), if both are present
/// and numeric. Endpoints are how OTP reports a leg's geometry; the segment sampler
/// reads them to place a sample point at the boarding/walk/arrival location.
fn endpoint_coords(leg: &Value, endpoint: &str) -> Option<(f64, f64)> {
    let e = leg.get(endpoint)?;
    let lat = e.get("lat").and_then(Value::as_f64)?;
    let lon = e.get("lon").and_then(Value::as_f64)?;
    Some((lat, lon))
}

/// One exposure segment of a journey, located in space and time (ADR 0036): the
/// sample key for its local forecast plus its precipitation- and
/// temperature-exposure seconds. Rain only reaches `precip_secs` (outdoor time);
/// `temp_secs` is `precip_secs` for outdoor segments and the mode-scaled in-vehicle
/// seconds for a ride.
struct Segment {
    key: SampleKey,
    precip_secs: f64,
    temp_secs: f64,
}

/// Walk an itinerary's legs and call `emit` once per exposure segment with its
/// located [`Segment`] (ADR 0036). Segments:
///
/// - **walk leg** → outdoor, sampled at the leg's `from` at its `startTime`;
/// - **outdoor wait/transfer gap** before a leg → outdoor, sampled at that leg's
///   `from` (where the wait happens) at the gap's start (the previous leg's end);
/// - **transit ride** → in-vehicle (temperature only, mode-scaled), sampled at the
///   leg's `from` at its `startTime`.
///
/// A segment whose coordinates can't be read is skipped (it cannot be sampled), so
/// it contributes nothing — fail-soft, like a missing forecast. Total and
/// panic-free: a missing/garbled `legs` array emits nothing.
fn for_each_segment(itinerary: &Value, mut emit: impl FnMut(Segment)) {
    let Some(legs) = itinerary.get("legs").and_then(Value::as_array) else {
        return;
    };

    let mut prev_end: Option<f64> = None;
    for leg in legs {
        let mode = leg.get("mode").and_then(Value::as_str).unwrap_or("");
        let start = leg_start_millis(leg);

        // Outdoor wait/transfer gap before this leg: exposure spent waiting at this
        // leg's boarding location, beginning when the previous leg ended.
        if let (Some(end), Some(s)) = (prev_end, start) {
            let gap_s = (s - end) / 1000.0;
            if gap_s.is_finite() && gap_s > 0.0 {
                if let Some((lat, lon)) = endpoint_coords(leg, "from") {
                    if let Some(key) = sample_key(lat, lon, Some(end)) {
                        emit(Segment {
                            key,
                            precip_secs: gap_s,
                            temp_secs: gap_s,
                        });
                    }
                }
            }
        }

        if mode == "WALK" {
            // A walk leg is fully outdoor, sampled at its origin at its start.
            let secs = leg_duration_seconds(leg);
            if secs > 0.0 {
                if let Some((lat, lon)) = endpoint_coords(leg, "from") {
                    if let Some(key) = sample_key(lat, lon, start) {
                        emit(Segment {
                            key,
                            precip_secs: secs,
                            temp_secs: secs,
                        });
                    }
                }
            }
        } else if is_transit_leg(leg) {
            // In-vehicle time is rain-sheltered for every mode; temperature reaches
            // the cabin scaled by the mode coefficient (ADR 0035).
            let secs = leg_duration_seconds(leg) * mode_temp_coeff(mode);
            if secs > 0.0 {
                if let Some((lat, lon)) = endpoint_coords(leg, "from") {
                    if let Some(key) = sample_key(lat, lon, start) {
                        emit(Segment {
                            key,
                            precip_secs: 0.0,
                            temp_secs: secs,
                        });
                    }
                }
            }
        }

        prev_end = leg_end_millis(leg);
    }
}

/// The per-segment weather penalty for one itinerary (ADR 0036). **Higher is
/// worse.** Each exposure segment is scored against the forecast LOCAL to its
/// place and time, looked up in `weather` by the segment's [`SampleKey`]:
///
/// ```text
/// penalty = Σ_segments (precip_badness_local · precip_minutes
///                       + temp_badness_local · temp_minutes)
/// ```
///
/// A segment whose local forecast is absent from `weather` (a point that failed to
/// resolve, or a partial multi-location response) contributes `0.0` — that segment
/// is simply neutral, the rest still score. With no bad weather anywhere, zero
/// exposure, or an empty `weather` map the penalty is `0.0`, so the factor stays
/// neutral for that itinerary. Total and panic-free: a malformed itinerary yields
/// `0.0`.
pub fn weather_penalty_multi(itinerary: &Value, weather: &JourneyWeather) -> f64 {
    let mut penalty = 0.0;
    for_each_segment(itinerary, |seg| {
        let Some(forecast) = weather.get(&seg.key) else {
            return; // unsampled point → neutral segment
        };
        let precip_badness = forecast.precip_badness();
        let temp_badness = forecast.temp_badness();
        penalty +=
            precip_badness * (seg.precip_secs / 60.0) + temp_badness * (seg.temp_secs / 60.0);
    });
    penalty
}

/// Upper bound on the distinct sample points one journey's *whole response* may
/// fetch (ADR 0036) — the cap is across ALL itineraries, not per itinerary. A trip's
/// exposed segments — origin, each transfer/boarding wait, the destination, the walk
/// legs — collapse onto a handful of coarse cells; this caps the multi-location
/// request and response so both stay bounded. On overflow the extra points are
/// dropped and their segments simply contribute a neutral `0.0` penalty (fail-soft),
/// never an unbounded request.
pub const MAX_SAMPLE_POINTS: usize = 8;

/// The distinct sample points a journey needs forecasts for (ADR 0036): the union of
/// **every itinerary's** exposure-segment [`SampleKey`]s in the response,
/// deduplicated and capped at [`MAX_SAMPLE_POINTS`] across them all. The order is
/// deterministic (sorted ascending), so the capped subset is stable across calls; the
/// current selection is purely sort-order (a future refinement could prioritize the
/// most-exposed segments instead). A dropped point is fail-soft: its segment is just
/// absent from the resolved map, so [`weather_penalty_multi`] scores it a neutral
/// `0.0`. Each key already carries coarse, quantized coordinates, so nothing precise
/// leaves the server. Total and panic-free: a malformed plan yields an empty set.
pub fn journey_sample_points(plan: &Value) -> Vec<SampleKey> {
    let mut keys: BTreeSet<SampleKey> = BTreeSet::new();
    if let Some(itineraries) = plan
        .get("data")
        .and_then(|d| d.get("plan"))
        .and_then(|p| p.get("itineraries"))
        .and_then(Value::as_array)
    {
        for it in itineraries {
            for_each_segment(it, |seg| {
                keys.insert(seg.key);
            });
        }
    }
    keys.into_iter().take(MAX_SAMPLE_POINTS).collect()
}

/// A leg's duration in seconds (OTP reports leg duration in seconds), `0.0` when
/// absent or malformed. Guarded finite and non-negative so a garbled negative
/// duration can never subtract from either exposure bucket — matching the
/// finite/positive guards on the transfer-gap and badness paths.
fn leg_duration_seconds(leg: &Value) -> f64 {
    leg.get("duration")
        .and_then(Value::as_f64)
        .filter(|d| d.is_finite() && *d >= 0.0)
        .unwrap_or(0.0)
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

    /// Fetch forecasts for a batch of distinct sample points in ONE multi-location
    /// request (ADR 0036). The wire request lists the distinct *cells* (one
    /// `(lat, lon)` per coarse cell, first-seen order) — two points in the same cell
    /// at different hours/days send the coordinate only once. Open-Meteo accepts
    /// comma-separated `latitude`/`longitude` and returns a parallel JSON array (one
    /// forecast object per cell, in input order); a single cell still returns one
    /// object, which we normalize to a one-element array. The request is restricted to
    /// `[start_date, end_date]` (the journey's date span); for a span of N days the
    /// hourly array holds `24*N` rows per cell, and each point's row is
    /// `day_offset*24 + hour` relative to the window start (see `parse_multi`).
    /// Returns one `(SampleKey, Forecast)` per point that parsed — a point whose cell
    /// is missing from the array, or with no `temperature_2m` for its row, is simply
    /// omitted (its segment stays neutral). Fail-soft: any transport error, timeout,
    /// non-success status, oversized/length-less body, or unparsable body yields an
    /// empty vec, so every segment stays neutral and the rerank still completes.
    /// `points` must already be coarse/quantized; the caller supplies them via
    /// [`journey_sample_points`].
    pub async fn fetch_multi(&self, points: &[SampleKey]) -> Vec<(SampleKey, Forecast)> {
        if points.is_empty() {
            return Vec::new();
        }
        // Collapse the points onto their distinct cells in first-seen order: two keys
        // in the same cell (different hour/day) send the coordinate once, so the wire
        // request and the response array are per-cell, not per-key.
        let mut cells: Vec<(i64, i64)> = Vec::new();
        for (la, lo, _, _) in points {
            if !cells.contains(&(*la, *lo)) {
                cells.push((*la, *lo));
            }
        }
        // De-quantize each distinct cell back to two decimals for the wire.
        let lats: Vec<String> = cells
            .iter()
            .map(|(la, _)| format!("{}", *la as f64 / 100.0))
            .collect();
        let lons: Vec<String> = cells
            .iter()
            .map(|(_, lo)| format!("{}", *lo as f64 / 100.0))
            .collect();
        let mut url = format!(
            "{}?latitude={}&longitude={}&hourly={HOURLY_FIELDS}",
            self.base_url,
            lats.join(","),
            lons.join(","),
        );
        // The widest [min, max] date across the points bounds the window; the window
        // start anchors each point's day offset into the hourly array.
        let dates: BTreeSet<&str> = points
            .iter()
            .filter_map(|(_, _, _, d)| d.as_deref())
            .collect();
        let start_date = dates.iter().next().copied();
        if let (Some(start), Some(end)) = (start_date, dates.iter().next_back()) {
            url.push_str("&timezone=UTC&start_date=");
            url.push_str(start);
            url.push_str("&end_date=");
            url.push_str(end);
        }
        // The response holds one object per distinct cell, so the body grows with the
        // cell count; scale the cap by the (bounded) number of cells.
        let cap = WEATHER_MAX_BODY_BYTES * cells.len() as u64;
        let Some(body) = self.get_json(&url, cap).await else {
            return Vec::new();
        };
        parse_multi(&body, &cells, points, start_date)
    }

    /// GET `url` and parse a JSON body, bounding the buffered body to `max_bytes`.
    /// Fail-soft: a transport error, timeout, non-success status, a body with no
    /// advertised length or one over the cap, or an unparsable body all yield `None`.
    /// Shared by the single-point and multi-location fetches so both bound the body
    /// the same way (ADR 0034/0036).
    async fn get_json(&self, url: &str, max_bytes: u64) -> Option<Value> {
        let resp = self
            .http
            .get(url)
            .timeout(WEATHER_TIMEOUT)
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        match resp.content_length() {
            Some(len) if len <= max_bytes => {}
            _ => return None,
        }
        resp.json().await.ok()
    }
}

/// Whole days from the window `start` date to `iso` (`YYYY-MM-DD`), clamped to
/// `>= 0`. The Open-Meteo hourly array starts at the window's first day, so a point
/// on day `d` after the start reads rows `d*24..`; an undated point or an
/// unparsable date pins day 0 (the window start). Reuses the shared civil-date
/// helper ([`days_from_ymd`]) on the digit-only form so the gateway and the worker
/// agree on the day arithmetic.
fn day_offset(start: Option<&str>, iso: Option<&str>) -> i64 {
    let (Some(start), Some(iso)) = (start, iso) else {
        return 0;
    };
    let to_days = |s: &str| days_from_ymd(&s.replace('-', ""));
    match (to_days(start), to_days(iso)) {
        (Some(s), Some(d)) => (d - s).max(0),
        _ => 0,
    }
}

/// Parse a multi-location Open-Meteo response into `(SampleKey, Forecast)` pairs,
/// one per input point that resolved (ADR 0036). The body is a JSON array parallel
/// to the DISTINCT `cells` (Open-Meteo returns the objects in input order); a
/// single-cell request may instead return a bare object, which we treat as a
/// one-element array. Each distinct cell maps positionally to its element's `hourly`
/// block; a short response simply leaves later cells unmapped (their points stay
/// neutral). For each point, its cell's `hourly` is indexed at
/// `row = day_offset*24 + hour`, where `day_offset` is whole days from the window
/// `start_date` to the point's own date — so a day-2 segment reads day-2's row, not
/// day-1's. A point whose cell is absent, or with no `temperature_2m` for its row, is
/// skipped — that point's segment stays neutral. Total and panic-free.
fn parse_multi(
    body: &Value,
    cells: &[(i64, i64)],
    points: &[SampleKey],
    start_date: Option<&str>,
) -> Vec<(SampleKey, Forecast)> {
    // Normalize a bare single object to a one-element slice view.
    let single;
    let elems: &[Value] = match body {
        Value::Array(a) => a.as_slice(),
        obj @ Value::Object(_) => {
            single = [obj.clone()];
            &single
        }
        _ => return Vec::new(),
    };
    // Each distinct cell -> its element's `hourly` block, positionally; a short
    // response leaves later cells without an entry (their points stay neutral).
    let mut hourly_by_cell: HashMap<(i64, i64), &Value> = HashMap::new();
    for (cell, elem) in cells.iter().zip(elems.iter()) {
        if let Some(hourly) = elem.get("hourly") {
            hourly_by_cell.insert(*cell, hourly);
        }
    }
    let mut out = Vec::with_capacity(points.len());
    for key in points {
        let Some(hourly) = hourly_by_cell.get(&(key.0, key.1)) else {
            continue; // cell missing from the (short) response → neutral
        };
        let row = (day_offset(start_date, key.3.as_deref()) * 24 + key.2 as i64) as usize;
        if let Some(forecast) = parse_forecast_hourly(hourly, row) {
            out.push((key.clone(), forecast));
        }
    }
    out
}

/// The hourly fields the client requests, comma-joined for the `&hourly=` query
/// parameter (ADR 0036): temperature (the fallback), apparent/feels-like
/// temperature (the preferred comfort signal), precipitation, UV index, and wind.
const HOURLY_FIELDS: &str =
    "temperature_2m,apparent_temperature,precipitation,uv_index,wind_speed_10m";

/// Parse one row out of an Open-Meteo `hourly` block into a [`Forecast`]. The block
/// shape is `{"temperature_2m": [...], "apparent_temperature": [...], "precipitation":
/// [...], "uv_index": [...], "wind_speed_10m": [...]}`. Returns `None` when the
/// required `temperature_2m` array or the requested index is absent, so a
/// malformed/empty body degrades to neutral. Every other field is optional — an
/// older/partial body without apparent temperature, UV, or wind still parses, those
/// signals just contribute nothing. The multi-location parser ([`parse_multi`]) calls
/// it per array element with that element's own row.
fn parse_forecast_hourly(hourly: &Value, hour: usize) -> Option<Forecast> {
    let temperature_c = hourly_at(hourly, "temperature_2m", hour)?;
    Some(Forecast {
        temperature_c,
        precipitation_mm: hourly_at(hourly, "precipitation", hour).unwrap_or(0.0),
        apparent_temperature_c: hourly_at(hourly, "apparent_temperature", hour),
        uv_index: hourly_at(hourly, "uv_index", hour),
        wind_speed_kmh: hourly_at(hourly, "wind_speed_10m", hour),
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
/// Integer quantization avoids float-as-key fragility (see [`quantize_coord`]); the
/// date keeps same-hour journeys on different days from colliding now that the
/// forecast window is pinned to the journey's day. This is exactly a [`SampleKey`],
/// so a journey's sample points and the cache share one key space (ADR 0036).
type CacheKey = SampleKey;

/// A cached forecast plus the instant it was stored, for TTL expiry.
type CacheEntry = (Forecast, Instant);

/// A bounded, thread-safe TTL cache of forecasts keyed by `(coarse-lat, coarse-lon,
/// hour, date)`. Mirrors the Tier-2 read cache's discipline: the lock guards only the
/// in-memory map for cheap get/put and is **never** held across the network
/// `.await` — [`get_or_fetch_journey`] reads under the lock, releases it, fetches,
/// then re-locks to publish. A poisoned lock is recovered rather than unwrapped so
/// one panic can't wedge later requests. The cache holds no user data — only coarse,
/// public weather for a ~1 km cell.
///
/// [`get_or_fetch_journey`]: WeatherCache::get_or_fetch_journey
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

    /// Resolve the journey's sampled weather for `points` (ADR 0036): serve every
    /// fresh cache hit from memory, fetch ONLY the cache-miss points in a single
    /// multi-location request, store each result, and return the whole
    /// [`JourneyWeather`] map. A fully-cached journey makes **zero** calls; a
    /// partially-cached one fetches only the misses. Fail-soft: a failed fetch (or a
    /// point the response omitted) just leaves that key out of the map, so its
    /// segment stays neutral and the rerank still completes. The lock is released
    /// before the fetch `.await` and retaken only to publish — never held across the
    /// await. `points` carry coarse, quantized coordinates (see
    /// [`journey_sample_points`]).
    pub async fn get_or_fetch_journey(
        &self,
        client: &WeatherClient,
        points: &[SampleKey],
    ) -> JourneyWeather {
        let mut resolved: JourneyWeather = HashMap::new();
        let mut misses: Vec<SampleKey> = Vec::new();
        for key in points {
            match self.fresh(key) {
                Some(f) => {
                    resolved.insert(key.clone(), f);
                }
                None => misses.push(key.clone()),
            }
        }
        // Record the hit/miss split (ADR 0037 phase 2). `hit` = points served from
        // the fresh cache, `miss` = points we will fetch. Bounded `outcome` label
        // only; fail-soft, a no-op without a recorder. Zero counts are skipped so a
        // series only appears once there is real activity.
        record_cache_lookups("hit", resolved.len() as u64);
        record_cache_lookups("miss", misses.len() as u64);
        if misses.is_empty() {
            return resolved;
        }
        for (key, forecast) in client.fetch_multi(&misses).await {
            self.store(key.clone(), forecast);
            resolved.insert(key, forecast);
        }
        resolved
    }
}

/// Lock the cache, recovering from a poisoned lock instead of unwrapping. The
/// guarded data is a plain map with no broken invariant, so recovering and reusing
/// it is always safe and keeps one panic from wedging every later request.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Record `count` weather-cache lookups with the given bounded `outcome`
/// (`hit`|`miss`) on the `weather_cache_lookups_total` counter (ADR 0037 phase 2).
/// A zero count is skipped so the series only appears once there is real activity.
/// Fail-soft: a no-op without an installed recorder.
fn record_cache_lookups(outcome: &'static str, count: u64) {
    if count == 0 {
        return;
    }
    metrics::counter!(
        iter_core::metrics::WEATHER_CACHE_LOOKUPS_TOTAL,
        iter_core::metrics::LABEL_OUTCOME => outcome,
    )
    .increment(count);
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
            uv_index: None,
            wind_speed_kmh: None,
        }
    }

    fn rainy() -> Forecast {
        Forecast {
            temperature_c: 12.0,
            precipitation_mm: 6.0, // past saturation
            apparent_temperature_c: Some(11.0),
            uv_index: None,
            wind_speed_kmh: None,
        }
    }

    /// A walk leg of `secs` seconds.
    fn walk(secs: f64) -> Value {
        json!({ "transitLeg": false, "mode": "WALK", "duration": secs })
    }

    /// A transit (in-vehicle, sheltered) leg with explicit start/end epoch millis and
    /// a real `duration` (seconds) consistent with them, so its in-vehicle segment is
    /// emitted just like a production OTP leg.
    fn ride(start_ms: f64, end_ms: f64) -> Value {
        json!({
            "transitLeg": true, "mode": "BUS",
            "route": { "gtfsId": "R" }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s" } },
            "startTime": start_ms, "endTime": end_ms,
            "duration": (end_ms - start_ms) / 1000.0,
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
            uv_index: None,
            wind_speed_kmh: None,
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
            uv_index: None,
            wind_speed_kmh: None,
        };
        let hot = Forecast {
            temperature_c: 40.0,
            precipitation_mm: 0.0,
            apparent_temperature_c: Some(40.0),
            uv_index: None,
            wind_speed_kmh: None,
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
            uv_index: None,
            wind_speed_kmh: None,
        };
        assert!(f.badness() > 0.0);
    }

    #[test]
    fn non_finite_forecast_fields_never_inflate_badness() {
        let f = Forecast {
            temperature_c: f64::NAN,
            precipitation_mm: f64::INFINITY,
            apparent_temperature_c: None,
            uv_index: Some(f64::NAN),
            wind_speed_kmh: Some(f64::INFINITY),
        };
        let b = f.badness();
        assert!(b.is_finite());
        assert!((0.0..=1.0).contains(&b));
    }

    // --- exposed time + penalty ---------------------------------------------

    #[test]
    fn walk_minutes_are_exposed() {
        // 600s walk feeds both buckets fully — outdoor time is exposed to rain and
        // temperature alike.
        let it = itin(vec![walk(600.0)]);
        let e = exposure_seconds(&it);
        assert_eq!(e.precip, 600.0);
        assert_eq!(e.temp, 600.0);
    }

    #[test]
    fn in_vehicle_time_is_rain_sheltered_but_temperature_reaches_it() {
        // A single bus ride with no preceding leg → no walk, no gap → zero precip
        // exposure (the cabin keeps rain off), but the temperature bucket carries
        // the in-vehicle minutes scaled by the bus coefficient (ADR 0035).
        let it = itin(vec![json!({
            "transitLeg": true, "mode": "BUS", "duration": 600.0,
            "route": { "gtfsId": "R" }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s" } },
            "startTime": 1_000_000.0, "endTime": 1_600_000.0,
        })]);
        let e = exposure_seconds(&it);
        assert_eq!(e.precip, 0.0);
        assert_eq!(e.temp, 600.0 * TEMP_COEFF_BUS_TRAM);
    }

    #[test]
    fn transfer_gap_between_legs_is_exposed() {
        // Ride A ends at t=1_600_000ms; ride B starts at t=1_900_000ms → a 300s
        // outdoor wait counts as exposure in both buckets even though both legs are
        // sheltered rides. Only the gap reaches the precip bucket — the rides keep the
        // rain off.
        let it = itin(vec![
            ride(1_000_000.0, 1_600_000.0),
            ride(1_900_000.0, 2_500_000.0),
        ]);
        let e = exposure_seconds(&it);
        assert_eq!(e.precip, 300.0);
        // The temperature bucket is the outdoor gap PLUS each 600s ride's in-vehicle
        // minutes scaled by the bus coefficient (the rides now carry a real duration).
        assert_eq!(e.temp, 300.0 + 2.0 * 600.0 * TEMP_COEFF_BUS_TRAM);
    }

    #[test]
    fn temperature_exposure_is_mode_aware() {
        // Equal-duration in-vehicle legs contribute different temperature exposure:
        // an air-conditioned metro leg contributes near-nothing while a bus leg
        // contributes a meaningful share (ADR 0035). Rain exposure is zero for both.
        let metro = itin(vec![json!({
            "transitLeg": true, "mode": "SUBWAY", "duration": 1000.0,
            "route": { "gtfsId": "M" }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s" } },
        })]);
        let bus = itin(vec![json!({
            "transitLeg": true, "mode": "BUS", "duration": 1000.0,
            "route": { "gtfsId": "B" }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s" } },
        })]);
        // A heavy-rail leg and an unknown mode flow through the same wiring, so the
        // whole coefficient table is exercised end-to-end, not just SUBWAY/BUS.
        let rail = itin(vec![json!({
            "transitLeg": true, "mode": "RAIL", "duration": 1000.0,
            "route": { "gtfsId": "R" }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s" } },
        })]);
        let unknown = itin(vec![json!({
            "transitLeg": true, "mode": "ZEPPELIN", "duration": 1000.0,
            "route": { "gtfsId": "Z" }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s" } },
        })]);
        let em = exposure_seconds(&metro);
        let eb = exposure_seconds(&bus);
        let er = exposure_seconds(&rail);
        let eu = exposure_seconds(&unknown);
        assert_eq!(em.precip, 0.0);
        assert_eq!(eb.precip, 0.0);
        assert_eq!(em.temp, 1000.0 * TEMP_COEFF_RAIL);
        assert_eq!(eb.temp, 1000.0 * TEMP_COEFF_BUS_TRAM);
        // Rail shares metro's sheltered bucket; the unknown mode sits strictly
        // between the rail and bus exposures.
        assert_eq!(er.temp, em.temp, "rail is as sheltered as metro");
        assert!(em.temp < eu.temp && eu.temp < eb.temp);
        assert!(
            eb.temp > em.temp,
            "a bus is more temperature-exposed than metro"
        );
    }

    #[test]
    fn negative_leg_duration_never_subtracts_from_exposure() {
        // A garbled negative duration must be ignored, not subtracted: a walk leg
        // with a negative duration contributes zero outdoor time, and a transit leg
        // with a negative duration contributes zero in-vehicle temperature (ADR 0035
        // hardening — OTP never emits these, but the buckets stay non-negative).
        let bad_walk = itin(vec![json!({
            "transitLeg": false, "mode": "WALK", "duration": -100.0,
        })]);
        let ew = exposure_seconds(&bad_walk);
        assert_eq!(ew.precip, 0.0);
        assert_eq!(ew.temp, 0.0);

        let bad_ride = itin(vec![json!({
            "transitLeg": true, "mode": "BUS", "duration": -600.0,
            "route": { "gtfsId": "R" }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s" } },
        })]);
        let er = exposure_seconds(&bad_ride);
        assert_eq!(er.precip, 0.0);
        assert_eq!(er.temp, 0.0);
    }

    #[test]
    fn mode_temp_coeff_orders_modes_sensibly() {
        // Air-conditioned rail/metro are near-sheltered; bus/tram are partially
        // exposed; an unknown mode lands at a conservative mid value. The exact
        // numbers are a heuristic; the ordering is the contract (ADR 0035).
        assert!(mode_temp_coeff("SUBWAY") < mode_temp_coeff("BUS"));
        assert!(mode_temp_coeff("RAIL") < mode_temp_coeff("TRAM"));
        // These equalities deliberately pin the bucket grouping (rail with metro,
        // bus with tram) so splitting a bucket would trip the test.
        assert_eq!(mode_temp_coeff("SUBWAY"), mode_temp_coeff("RAIL"));
        assert_eq!(mode_temp_coeff("BUS"), mode_temp_coeff("TRAM"));
        // An unknown mode sits between the sheltered and the exposed bands.
        let unknown = mode_temp_coeff("ZEPPELIN");
        assert!(mode_temp_coeff("SUBWAY") <= unknown);
        assert!(unknown <= mode_temp_coeff("BUS"));
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
        // Even a long walk costs nothing when the weather is calm. Because the split
        // penalty short-circuits to 0.0 whenever precip_badness and temp_badness are
        // both 0, mild/dry composites stay byte-identical to the pre-0035 behavior —
        // the type split changes nothing in good weather.
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

    // --- split badness + type-aware penalty (ADR 0035) -----------------------

    /// A dry but hot hour: temperature extreme with no rain, so only the temp
    /// component (and thus in-vehicle exposure) bites.
    fn hot() -> Forecast {
        Forecast {
            temperature_c: 40.0,
            precipitation_mm: 0.0,
            apparent_temperature_c: Some(40.0),
            uv_index: None,
            wind_speed_kmh: None,
        }
    }

    #[test]
    fn badness_splits_into_precipitation_and_temperature_components() {
        // A pouring, comfortable-temperature hour: all precip badness, no temp.
        let wet = Forecast {
            temperature_c: 18.0,
            precipitation_mm: 8.0,
            apparent_temperature_c: Some(18.0),
            uv_index: None,
            wind_speed_kmh: None,
        };
        assert!(wet.precip_badness() > 0.0);
        assert_eq!(wet.temp_badness(), 0.0);

        // A dry, scorching hour: all temp badness, no precip.
        assert_eq!(hot().precip_badness(), 0.0);
        assert!(hot().temp_badness() > 0.0);

        // The combined badness still folds both, so the calm case is neutral.
        assert_eq!(calm().precip_badness(), 0.0);
        assert_eq!(calm().temp_badness(), 0.0);
        assert_eq!(calm().badness(), 0.0);
    }

    #[test]
    fn rain_penalizes_outdoor_time_not_in_vehicle_time() {
        // In the rain, a long bus ride (sheltered) must out-score an equal-duration
        // walk: precipitation only reaches the outdoor walk, so the ride's penalty
        // is zero while the walk's is positive (ADR 0035).
        let walk_it = itin(vec![walk(1200.0)]);
        let ride_it = itin(vec![json!({
            "transitLeg": true, "mode": "BUS", "duration": 1200.0,
            "route": { "gtfsId": "R" }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s" } },
        })]);
        // Heavy rain, comfortable temperature → temp badness is zero, so the ride's
        // in-vehicle temperature exposure contributes nothing either.
        let pouring = Forecast {
            temperature_c: 18.0,
            precipitation_mm: 8.0,
            apparent_temperature_c: Some(18.0),
            uv_index: None,
            wind_speed_kmh: None,
        };
        let p_walk = weather_penalty(&walk_it, &pouring);
        let p_ride = weather_penalty(&ride_it, &pouring);
        assert!(p_walk > 0.0);
        assert_eq!(p_ride, 0.0, "a sheltered ride takes no rain penalty");
    }

    #[test]
    fn heat_penalizes_a_bus_more_than_an_equal_metro_ride() {
        // In a heatwave, an equal-duration bus ride is more temperature-exposed than
        // a metro ride because the bus coefficient is higher (ADR 0035). Neither has
        // outdoor time, so the whole penalty is the mode-scaled in-vehicle exposure.
        let bus = itin(vec![json!({
            "transitLeg": true, "mode": "BUS", "duration": 1800.0,
            "route": { "gtfsId": "B" }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s" } },
        })]);
        let metro = itin(vec![json!({
            "transitLeg": true, "mode": "SUBWAY", "duration": 1800.0,
            "route": { "gtfsId": "M" }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s" } },
        })]);
        let p_bus = weather_penalty(&bus, &hot());
        let p_metro = weather_penalty(&metro, &hot());
        assert!(p_bus > p_metro, "the bus cabin is hotter than the metro");
        assert!(p_metro > 0.0, "even an A/C metro is not fully sheltered");
    }

    #[test]
    fn heat_leaves_a_pure_ride_below_an_equal_walk() {
        // Sanity that the temperature bucket still ranks an outdoor walk above any
        // in-vehicle ride: the walk feels the full extreme (coeff 1.0) while a ride
        // feels only a fraction.
        let walk_it = itin(vec![walk(1800.0)]);
        let bus = itin(vec![json!({
            "transitLeg": true, "mode": "BUS", "duration": 1800.0,
            "route": { "gtfsId": "B" }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s" } },
        })]);
        assert!(weather_penalty(&walk_it, &hot()) > weather_penalty(&bus, &hot()));
    }

    // --- coarse coordinates --------------------------------------------------

    #[test]
    fn coordinates_are_rounded_to_two_decimals() {
        assert_eq!(round_coord(41.90278), 41.9);
        assert_eq!(round_coord(12.49636), 12.5);
        assert_eq!(round_coord(-0.123456), -0.12);
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

    /// A single-cell multi-location body (one-element array), for cache mechanics that
    /// fetch one miss at a time through the journey path.
    const ONE_CELL_BODY: &str = r#"[{"hourly":{"temperature_2m":[20.0],"precipitation":[0.0],"apparent_temperature":[19.0]}}]"#;

    #[tokio::test]
    async fn cache_refetches_after_ttl_expiry() {
        // The expiry branch: with a tiny TTL the entry goes stale, so a second journey
        // over the same point past the TTL hits the stub again rather than serving
        // stale. Driven through the surviving journey path (get_or_fetch_journey).
        let (base, hits, _h) = counting_stub(ONE_CELL_BODY).await;
        let client = WeatherClient::new(reqwest::Client::new(), base);
        let cache = WeatherCache::with_ttl_and_cap(Duration::from_millis(20), WEATHER_CACHE_CAP);
        let key = (4190, 1250, 1usize, Some("2026-07-01".to_string()));
        cache
            .get_or_fetch_journey(&client, std::slice::from_ref(&key))
            .await;
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        tokio::time::sleep(Duration::from_millis(40)).await;
        cache.get_or_fetch_journey(&client, &[key]).await;
        assert_eq!(hits.load(Ordering::SeqCst), 2, "stale entry should refetch");
    }

    #[tokio::test]
    async fn cache_clears_wholesale_on_overflow() {
        // The clear-on-overflow eviction: with cap 2, storing a 3rd distinct key
        // clears the map first, so an earlier key misses and refetches afterward. Each
        // journey fetches one fresh cell (the stub serves a one-element array).
        let (base, hits, _h) = counting_stub(ONE_CELL_BODY).await;
        let client = WeatherClient::new(reqwest::Client::new(), base);
        let cache = WeatherCache::with_ttl_and_cap(WEATHER_TTL, 2);
        // Three distinct cells, each at hour 0 so a one-element body parses row 0.
        let key_a = (4190, 1250, 0usize, Some("2026-07-01".to_string()));
        let key_b = (4200, 1300, 0usize, Some("2026-07-01".to_string()));
        let key_c = (4300, 1400, 0usize, Some("2026-07-01".to_string()));
        cache
            .get_or_fetch_journey(&client, std::slice::from_ref(&key_a))
            .await; // key A
        cache.get_or_fetch_journey(&client, &[key_b]).await; // key B (now full)
        cache.get_or_fetch_journey(&client, &[key_c]).await; // key C clears, count 3
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        // Key A was evicted by the wholesale clear, so it refetches → 4 hits.
        cache.get_or_fetch_journey(&client, &[key_a]).await;
        assert_eq!(
            hits.load(Ordering::SeqCst),
            4,
            "cleared key A should refetch"
        );
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

    // --- UV and wind comfort signals (ADR 0036) ------------------------------

    /// A bare forecast at `temp` with no rain and no UV/wind, for isolating one
    /// comfort signal at a time.
    fn at_temp(temp: f64) -> Forecast {
        Forecast {
            temperature_c: temp,
            precipitation_mm: 0.0,
            apparent_temperature_c: Some(temp),
            uv_index: None,
            wind_speed_kmh: None,
        }
    }

    #[test]
    fn high_uv_adds_felt_comfort_badness() {
        // A comfortable, dry hour gains badness once a strong-UV reading is present:
        // a long sunny outdoor wait burns (ADR 0036). With no UV the same hour is
        // neutral, so the lift is the UV signal alone.
        let mut sunny = at_temp(22.0);
        assert_eq!(sunny.temp_badness(), 0.0);
        sunny.uv_index = Some(UV_SATURATION); // saturating UV
        assert!(sunny.temp_badness() > 0.0);
        assert!(sunny.temp_badness() <= 1.0);
    }

    #[test]
    fn strong_wind_adds_felt_comfort_badness() {
        // Wind drives windchill / sideways rain; a saturating wind reading lifts the
        // felt-comfort badness of an otherwise-comfortable hour (ADR 0036).
        let mut breezy = at_temp(15.0);
        assert_eq!(breezy.temp_badness(), 0.0);
        breezy.wind_speed_kmh = Some(WIND_SATURATION_KMH);
        assert!(breezy.temp_badness() > 0.0);
        assert!(breezy.temp_badness() <= 1.0);
    }

    #[test]
    fn uv_and_wind_never_overrule_a_real_temperature_extreme() {
        // The thermal extreme dominates: a scorching hour with no UV/wind already
        // carries temp badness, and adding UV/wind only nudges it, never past 1.0.
        let plain_hot = at_temp(40.0);
        let mut hot_uv_wind = plain_hot;
        hot_uv_wind.uv_index = Some(11.0);
        hot_uv_wind.wind_speed_kmh = Some(60.0);
        assert!(hot_uv_wind.temp_badness() >= plain_hot.temp_badness());
        assert!(hot_uv_wind.temp_badness() <= 1.0);
    }

    #[test]
    fn absent_or_garbled_uv_and_wind_contribute_nothing() {
        // An older/partial body (None) and a non-finite/non-positive reading both
        // contribute zero, so they never inflate the penalty.
        let comfy = at_temp(20.0);
        assert_eq!(comfy.temp_badness(), 0.0);
        let mut garbled = comfy;
        garbled.uv_index = Some(f64::NAN);
        garbled.wind_speed_kmh = Some(-5.0);
        assert_eq!(garbled.temp_badness(), 0.0);
    }

    // --- per-segment local weather (ADR 0036) --------------------------------

    /// A leg endpoint object carrying coarse coordinates (and an optional stop).
    fn at(lat: f64, lon: f64) -> Value {
        json!({ "lat": lat, "lon": lon })
    }

    /// A walk leg from `(lat, lon)` starting at `start_ms`, `secs` long.
    fn walk_at(lat: f64, lon: f64, start_ms: f64, secs: f64) -> Value {
        json!({
            "transitLeg": false, "mode": "WALK", "duration": secs,
            "startTime": start_ms, "endTime": start_ms + secs * 1000.0,
            "from": at(lat, lon),
        })
    }

    /// A bus leg boarding at `(lat, lon)` between `start_ms` and `end_ms`, with a real
    /// `duration` (seconds) consistent with them so its in-vehicle temperature segment
    /// is emitted (sampled at the boarding cell), as a production OTP leg would be.
    fn ride_at(lat: f64, lon: f64, start_ms: f64, end_ms: f64) -> Value {
        json!({
            "transitLeg": true, "mode": "BUS",
            "route": { "gtfsId": "R" }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s" }, "lat": lat, "lon": lon },
            "startTime": start_ms, "endTime": end_ms,
            "duration": (end_ms - start_ms) / 1000.0,
        })
    }

    /// A foul (cold, pouring) forecast — any exposed minutes are penalized.
    fn foul() -> Forecast {
        Forecast {
            temperature_c: 6.0,
            precipitation_mm: 9.0,
            apparent_temperature_c: Some(4.0),
            uv_index: None,
            wind_speed_kmh: None,
        }
    }

    #[test]
    fn a_hot_transfer_point_penalizes_only_that_transfers_segment() {
        // A two-ride trip with an outdoor transfer gap. Sampling per segment, a hot
        // ORIGIN cell penalizes the first ride's in-vehicle minutes, while a hot
        // TRANSFER cell penalizes the gap PLUS the second ride boarding there — proving
        // each segment reads its own local cell. The dates here all fall on the same
        // UTC day (epoch second 0/600/900), so only the cell distinguishes the keys.
        let it = itin(vec![
            ride_at(41.9, 12.5, 0.0, 600_000.0),
            // gap 41.9,12.5 -> 42.0,13.0 from 600_000 to 900_000 (the transfer)
            ride_at(42.0, 13.0, 900_000.0, 1_500_000.0),
        ]);
        let date = utc_ymd(0);
        let tb = hot().temp_badness();

        // Only the ORIGIN cell is hot: the first ride's in-vehicle temp segment (600s
        // scaled by the bus coefficient, sampled at the origin) is the only hot one.
        let origin_key = (4190, 1250, 0usize, Some(date.clone()));
        let mut hot_origin: JourneyWeather = HashMap::new();
        hot_origin.insert(origin_key, hot());
        let p_hot_origin = weather_penalty_multi(&it, &hot_origin);
        let ride_temp_min = 600.0 * TEMP_COEFF_BUS_TRAM / 60.0;
        assert!(
            (p_hot_origin - tb * ride_temp_min).abs() < 1e-9,
            "a hot origin penalizes only the first ride's in-vehicle minutes"
        );

        // Only the TRANSFER cell is hot: the 300s outdoor gap (full temp exposure)
        // plus the second ride's 600s in-vehicle minutes, both sampled at the transfer
        // cell, take the penalty — strictly more than the hot origin, and the origin
        // ride stays neutral.
        let transfer_key = (4200, 1300, 0usize, Some(date));
        let mut hot_transfer: JourneyWeather = HashMap::new();
        hot_transfer.insert(transfer_key, hot());
        let p_hot_transfer = weather_penalty_multi(&it, &hot_transfer);
        let expected_transfer = tb * (300.0 / 60.0 + ride_temp_min);
        assert!(
            (p_hot_transfer - expected_transfer).abs() < 1e-9,
            "a hot transfer penalizes the gap and the ride boarding there: {p_hot_transfer}"
        );
        assert!(
            p_hot_transfer > p_hot_origin,
            "the transfer cell carries more hot exposure than the origin cell"
        );
    }

    #[test]
    fn in_vehicle_temperature_tracks_each_rides_own_boarding_cell() {
        // FIX 2: a two-ride trip with real durations, boarding in two DIFFERENT cells
        // with NO outdoor gap (ride B starts exactly when ride A ends). The in-vehicle
        // temperature penalty must track each ride's OWN boarding cell: a hot cell A
        // penalizes only ride A's in-vehicle minutes, a hot cell B only ride B's. With
        // equal durations the two penalties are equal in magnitude but attach to
        // different segments — swapping which cell is hot keeps the same total but it
        // comes from the other ride.
        let it = itin(vec![
            ride_at(41.9, 12.5, 0.0, 600_000.0), // ride A, cell (4190,1250)
            ride_at(45.0, 9.0, 600_000.0, 1_200_000.0), // ride B, cell (4500,900), no gap
        ]);
        let date = utc_ymd(0);
        let tb = hot().temp_badness();
        let ride_temp_min = 600.0 * TEMP_COEFF_BUS_TRAM / 60.0;

        // Hot at ride A's boarding cell only.
        let key_a = (4190, 1250, 0usize, Some(date.clone()));
        let mut hot_a: JourneyWeather = HashMap::new();
        hot_a.insert(key_a, hot());
        let p_a = weather_penalty_multi(&it, &hot_a);
        assert!(
            (p_a - tb * ride_temp_min).abs() < 1e-9,
            "ride A's in-vehicle penalty is its own boarding cell's: {p_a}"
        );

        // Hot at ride B's boarding cell only — same magnitude, different ride.
        let key_b = (4500, 900, 0usize, Some(date));
        let mut hot_b: JourneyWeather = HashMap::new();
        hot_b.insert(key_b, hot());
        let p_b = weather_penalty_multi(&it, &hot_b);
        assert!(
            (p_b - tb * ride_temp_min).abs() < 1e-9,
            "ride B's in-vehicle penalty is its own boarding cell's: {p_b}"
        );
        // Both buckets are real (the in-vehicle segment is actually emitted now).
        assert!(p_a > 0.0 && p_b > 0.0);
    }

    #[test]
    fn rain_at_the_destination_penalizes_the_final_walk() {
        // A ride to a destination, then a final walk in a different (rainy) cell. Only
        // the destination cell is wet; sampling per segment, the rain penalty lands on
        // the final walk (the destination cell), and a dry origin contributes nothing.
        let it = itin(vec![
            ride_at(41.9, 12.5, 0.0, 600_000.0),
            walk_at(42.0, 13.0, 600_000.0, 300.0), // final walk at the destination
        ]);
        let date = utc_ymd(600);
        let dest_key = (4200, 1300, 0usize, Some(date.clone()));
        let mut wet_dest: JourneyWeather = HashMap::new();
        wet_dest.insert(dest_key, rainy());
        let p_wet_dest = weather_penalty_multi(&it, &wet_dest);
        assert!(
            p_wet_dest > 0.0,
            "rain at the destination hits the final walk"
        );

        // Rain only at the ORIGIN (a sheltered ride) takes no precip penalty — the
        // ride is rain-sheltered and the walk's cell is dry.
        let origin_key = (4190, 1250, 0usize, Some(date));
        let mut wet_origin: JourneyWeather = HashMap::new();
        wet_origin.insert(origin_key, rainy());
        assert_eq!(
            weather_penalty_multi(&it, &wet_origin),
            0.0,
            "a sheltered origin ride takes no rain penalty when only the origin is wet"
        );
    }

    #[test]
    fn a_later_leg_samples_a_later_hour() {
        // Two walk legs in the SAME cell on the same day but hours apart: the first at
        // 08:00, the second at 14:00. Per-leg-time sampling keys each to its own hour,
        // so a forecast placed only at the later hour penalizes only the later leg.
        let h8 = 8.0 * 3600.0 * 1000.0;
        let h14 = 14.0 * 3600.0 * 1000.0;
        let it = itin(vec![
            walk_at(41.9, 12.5, h8, 300.0),
            walk_at(41.9, 12.5, h14, 300.0),
        ]);
        let keys = journey_sample_points(&plan_of(vec![it.clone()]));
        // Two distinct keys differing only in the hour component.
        assert_eq!(keys.len(), 2, "a later leg keys a distinct hour: {keys:?}");
        let hours: BTreeSet<usize> = keys.iter().map(|k| k.2).collect();
        assert_eq!(hours, BTreeSet::from([8, 14]));

        // A foul forecast at hour 14 only penalizes the later walk, leaving the 08:00
        // walk neutral — the hours don't share a forecast.
        let date = utc_ymd((h14 / 1000.0) as u64);
        let later_key = (4190, 1250, 14usize, Some(date));
        let mut later_only: JourneyWeather = HashMap::new();
        later_only.insert(later_key, foul());
        let p_later = weather_penalty_multi(&it, &later_only);
        // Only the 300s 14:00 walk scores; the 08:00 walk is unsampled → neutral.
        let single = itin(vec![walk_at(41.9, 12.5, h14, 300.0)]);
        assert_eq!(p_later, weather_penalty_multi(&single, &later_only));
    }

    #[test]
    fn apparent_temperature_drives_the_per_segment_penalty_not_raw_temp() {
        // A walk under air temp 10°C (comfortable) that *feels* like -8°C: the
        // per-segment penalty must come from the felt value, so it is positive even
        // though the raw temperature is inside the comfort band.
        let it = itin(vec![walk_at(41.9, 12.5, 0.0, 600.0)]);
        let feels_freezing = Forecast {
            temperature_c: 10.0,
            precipitation_mm: 0.0,
            apparent_temperature_c: Some(-8.0),
            uv_index: None,
            wind_speed_kmh: None,
        };
        let key = (4190, 1250, 0usize, Some(utc_ymd(0)));
        let mut weather: JourneyWeather = HashMap::new();
        weather.insert(key, feels_freezing);
        assert!(
            weather_penalty_multi(&it, &weather) > 0.0,
            "feels-like, not raw temp, drives the segment penalty"
        );
    }

    #[test]
    fn an_unsampled_point_leaves_its_segment_neutral() {
        // A walk whose local cell is missing from the map contributes nothing; the
        // rest of the journey still scores. Here the only segment is unsampled, so the
        // whole penalty is zero — fail-soft per segment.
        let it = itin(vec![walk_at(41.9, 12.5, 0.0, 1800.0)]);
        let empty: JourneyWeather = HashMap::new();
        assert_eq!(weather_penalty_multi(&it, &empty), 0.0);
    }

    #[test]
    fn single_leg_trip_matches_the_origin_only_model() {
        // Continuity: a single-leg trip sampled at its origin gives the SAME penalty
        // under the per-segment model as the old single-point `weather_penalty`. A
        // lone walk has one segment at the origin, so the two must agree exactly.
        let it = itin(vec![walk_at(41.9, 12.5, 0.0, 1800.0)]);
        let key = (4190, 1250, 0usize, Some(utc_ymd(0)));
        let mut weather: JourneyWeather = HashMap::new();
        weather.insert(key, foul());
        let multi = weather_penalty_multi(&it, &weather);
        let single = weather_penalty(&it, &foul());
        assert!((multi - single).abs() < 1e-9, "{multi} vs {single}");
        assert!(multi > 0.0);
    }

    // --- distinct sample points (ADR 0036) -----------------------------------

    /// A plan wrapping the given itineraries.
    fn plan_of(itineraries: Vec<Value>) -> Value {
        json!({ "data": { "plan": { "itineraries": itineraries } } })
    }

    #[test]
    fn journey_sample_points_dedups_repeated_cells() {
        // Two itineraries both walking the same origin cell at the same hour collapse
        // onto ONE sample point — the distinct union, not one per segment.
        let a = itin(vec![walk_at(41.9, 12.5, 0.0, 300.0)]);
        let b = itin(vec![walk_at(41.9, 12.5, 0.0, 600.0)]);
        let pts = journey_sample_points(&plan_of(vec![a, b]));
        assert_eq!(pts.len(), 1, "same cell + hour dedups: {pts:?}");
    }

    #[test]
    fn journey_sample_points_is_capped() {
        // More distinct cells than the cap → the set is bounded to MAX_SAMPLE_POINTS,
        // so the request and response stay bounded. The keys differ only in latitude
        // (40.00, 40.01, … ascending), so the cap keeps the LOWEST-sorted 8 — q_lat
        // 4000..=4007. Asserting the exact survivors catches a regression that kept the
        // wrong 8 (e.g. dropped the lowest or the destination cell).
        let legs: Vec<Value> = (0..(MAX_SAMPLE_POINTS as i64 + 4))
            .map(|i| walk_at(40.0 + i as f64 / 100.0, 10.0, 0.0, 300.0))
            .collect();
        let it = itin(legs);
        let pts = journey_sample_points(&plan_of(vec![it]));
        assert_eq!(pts.len(), MAX_SAMPLE_POINTS);
        let lats: Vec<i64> = pts.iter().map(|k| k.0).collect();
        let expected: Vec<i64> = (4000..4000 + MAX_SAMPLE_POINTS as i64).collect();
        assert_eq!(lats, expected, "the cap keeps the lowest-sorted cells");
    }

    #[test]
    fn journey_sample_points_is_empty_for_a_malformed_plan() {
        assert!(journey_sample_points(&json!(7)).is_empty());
        assert!(journey_sample_points(&json!({ "data": {} })).is_empty());
        // Legs with no coords can't be placed → no points.
        let no_coords = plan_of(vec![itin(vec![walk(300.0)])]);
        assert!(journey_sample_points(&no_coords).is_empty());
    }

    // --- multi-location fetch (local stub, no real network) ------------------

    /// A multi-location Open-Meteo body: a JSON ARRAY of per-point objects, each with
    /// its own `hourly` block. Point 0 is calm, point 1 is pouring.
    const MULTI_BODY: &str = r#"[
      {"hourly":{"temperature_2m":[20.0],"precipitation":[0.0],"apparent_temperature":[20.0],"uv_index":[2.0],"wind_speed_10m":[5.0]}},
      {"hourly":{"temperature_2m":[6.0],"precipitation":[9.0],"apparent_temperature":[4.0],"uv_index":[0.0],"wind_speed_10m":[10.0]}}
    ]"#;

    /// Stand up a stub that answers every GET with `body`, recording the last query
    /// string so a test can assert how the multi-location request was built.
    async fn recording_stub(
        body: &'static str,
    ) -> (
        String,
        Arc<Mutex<Option<String>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use axum::extract::RawQuery;
        use axum::routing::get;
        let seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let rec = seen.clone();
        let app = axum::Router::new().route(
            "/",
            get(move |RawQuery(q): RawQuery| {
                let rec = rec.clone();
                async move {
                    *rec.lock().unwrap() = q;
                    body
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/"), seen, handle)
    }

    #[tokio::test]
    async fn fetch_multi_builds_one_request_over_distinct_points_and_date_range() {
        let (base, seen, _h) = recording_stub(MULTI_BODY).await;
        let client = WeatherClient::new(reqwest::Client::new(), base);
        // Two distinct cells on two different days; the request must list both
        // coordinates comma-separated and span [min, max] of the dates.
        let p0 = (4190, 1250, 0usize, Some("2026-07-01".to_string()));
        let p1 = (4200, 1300, 0usize, Some("2026-07-03".to_string()));
        let out = client.fetch_multi(&[p0.clone(), p1.clone()]).await;
        let q = seen.lock().unwrap().clone().expect("stub was called");
        // Comma-separated coordinate lists, one entry per distinct cell.
        assert!(q.contains("latitude=41.9,42"), "lat list: {q}");
        assert!(q.contains("longitude=12.5,13"), "lon list: {q}");
        // The window spans the widest [min, max] date.
        assert!(q.contains("start_date=2026-07-01"), "start: {q}");
        assert!(q.contains("end_date=2026-07-03"), "end: {q}");
        assert!(q.contains("timezone=UTC"), "tz: {q}");
        // The richer comfort fields are requested.
        assert!(q.contains("apparent_temperature"), "apparent: {q}");
        assert!(q.contains("uv_index"), "uv: {q}");
        assert!(q.contains("wind_speed_10m"), "wind: {q}");
        // p0 is on the window-start day (offset 0 → row 0), so it parses and carries
        // the new fields. p1 is two days later (offset 2 → row 48), which this short
        // stub body lacks, so it stays neutral — fail-soft. (The day-offset row math is
        // proven exactly in `parse_multi_indexes_each_key_by_its_own_day`.)
        let by_key: HashMap<_, _> = out.into_iter().collect();
        let f0 = by_key.get(&p0).expect("the window-start-day point parses");
        assert_eq!(f0.precipitation_mm, 0.0);
        assert_eq!(f0.apparent_temperature_c, Some(20.0));
        assert_eq!(f0.uv_index, Some(2.0));
        assert!(
            !by_key.contains_key(&p1),
            "the day-2 point needs row 48, absent from the short stub body → neutral"
        );
    }

    #[tokio::test]
    async fn fetch_multi_is_empty_for_no_points() {
        let (base, seen, _h) = recording_stub(MULTI_BODY).await;
        let client = WeatherClient::new(reqwest::Client::new(), base);
        assert!(client.fetch_multi(&[]).await.is_empty());
        assert!(seen.lock().unwrap().is_none(), "no points → no request");
    }

    #[tokio::test]
    async fn fetch_multi_is_fail_soft_on_a_500() {
        use axum::http::StatusCode;
        use axum::routing::get;
        let app = axum::Router::new().route(
            "/",
            get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _h = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = WeatherClient::new(reqwest::Client::new(), format!("http://{addr}/"));
        let p0 = (4190, 1250, 0usize, Some("2026-07-01".to_string()));
        assert!(client.fetch_multi(&[p0]).await.is_empty());
    }

    #[tokio::test]
    async fn fetch_multi_is_fail_soft_on_an_unparsable_body() {
        // A non-JSON body yields an empty vec (every segment stays neutral), never a
        // panic — the shared bounded-buffer parse degrades softly.
        let (base, _seen, _h) = recording_stub("{ not json at all").await;
        let client = WeatherClient::new(reqwest::Client::new(), base);
        let p0 = (4190, 1250, 0usize, Some("2026-07-01".to_string()));
        assert!(client.fetch_multi(&[p0]).await.is_empty());
    }

    #[tokio::test]
    async fn fetch_multi_rejects_an_oversized_body() {
        // A body declaring more than the (cell-scaled) cap is refused before reading,
        // degrading to an empty vec — the buffered body is always bounded. Served with
        // an explicit oversized Content-Length.
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
        let p0 = (4190, 1250, 0usize, Some("2026-07-01".to_string()));
        assert!(
            client.fetch_multi(&[p0]).await.is_empty(),
            "an oversized body must degrade to an empty vec, not buffer"
        );
    }

    #[tokio::test]
    async fn fetch_multi_is_fail_soft_on_a_dead_upstream() {
        // A dead loopback port → connection refused (then the short timeout) → empty
        // vec, never a panic or a stall that outlives the timeout.
        let client = WeatherClient::new(reqwest::Client::new(), "http://127.0.0.1:1/".to_string());
        let p0 = (4190, 1250, 0usize, Some("2026-07-01".to_string()));
        assert!(client.fetch_multi(&[p0]).await.is_empty());
    }

    #[tokio::test]
    async fn fetch_multi_times_out_into_an_empty_vec() {
        // FIX 6: a stub that sleeps past WEATHER_TIMEOUT trips the short client
        // timeout, so fetch_multi returns an empty vec (every segment stays neutral),
        // mirroring the single-point dead-upstream coverage. The stub accepts the
        // connection but stalls the response past the timeout; the whole call must
        // still return within a small margin of WEATHER_TIMEOUT, never hang.
        use axum::routing::get;
        let app = axum::Router::new().route(
            "/",
            get(|| async {
                tokio::time::sleep(WEATHER_TIMEOUT * 5).await;
                MULTI_BODY
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _h = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = WeatherClient::new(reqwest::Client::new(), format!("http://{addr}/"));
        let p0 = (4190, 1250, 0usize, Some("2026-07-01".to_string()));
        let started = Instant::now();
        let out = client.fetch_multi(&[p0]).await;
        assert!(
            out.is_empty(),
            "a slow upstream past the timeout is neutral"
        );
        assert!(
            started.elapsed() < WEATHER_TIMEOUT * 3,
            "the timeout bounds the wait; it must not hang for the full sleep"
        );
    }

    #[test]
    fn parse_multi_skips_a_missing_or_short_array_element() {
        // A response shorter than the cell list (one object, two cells) maps only the
        // present cell; the missing cell's point is simply omitted (stays neutral).
        let p0 = (4190, 1250, 0usize, Some("2026-07-01".to_string()));
        let p1 = (4200, 1300, 0usize, Some("2026-07-01".to_string()));
        let cells = vec![(4190, 1250), (4200, 1300)];
        let body = json!([
            { "hourly": { "temperature_2m": [12.0], "precipitation": [0.0] } }
        ]);
        let out = parse_multi(&body, &cells, &[p0.clone(), p1], Some("2026-07-01"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, p0);
    }

    #[test]
    fn parse_multi_normalizes_a_bare_single_object() {
        // A single-cell request may return a bare object instead of a one-element
        // array; we treat it as a one-element array.
        let p0 = (4190, 1250, 1usize, Some("2026-07-01".to_string()));
        let body = json!({ "hourly": { "temperature_2m": [9.0, 11.0] } });
        let out = parse_multi(&body, &[(4190, 1250)], &[p0], Some("2026-07-01"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1.temperature_c, 11.0);
    }

    #[test]
    fn parse_multi_skips_an_element_without_temperature_for_its_hour() {
        // A point whose row is past the array end yields no forecast for that point —
        // neutral, never a panic.
        let p0 = (4190, 1250, 5usize, Some("2026-07-01".to_string()));
        let body = json!([{ "hourly": { "temperature_2m": [10.0] } }]);
        assert!(parse_multi(&body, &[(4190, 1250)], &[p0], Some("2026-07-01")).is_empty());
    }

    #[test]
    fn parse_multi_indexes_each_key_by_its_own_day() {
        // FIX 1: two points in the SAME cell on DIFFERENT days. A >=2-day window
        // returns 48 hourly rows (day1 = rows 0-23, day2 = rows 24-47). The day-1
        // 08:00 key must read row 8 and the day-2 08:00 key must read row 32 — not
        // both row 8 — proving the day offset is applied.
        let day1 = (4190, 1250, 8usize, Some("2026-07-01".to_string()));
        let day2 = (4190, 1250, 8usize, Some("2026-07-02".to_string()));
        // 48-row arrays where row 8 and row 32 differ. temperature_2m carries the
        // distinguishing value; everything else is filler.
        let mut temps = vec![0.0f64; 48];
        temps[8] = 11.0; // day-1 08:00
        temps[32] = 29.0; // day-2 08:00
        let body = json!([{ "hourly": { "temperature_2m": temps } }]);
        let out = parse_multi(
            &body,
            &[(4190, 1250)],
            &[day1.clone(), day2.clone()],
            Some("2026-07-01"),
        );
        let by_key: HashMap<_, _> = out.into_iter().collect();
        assert_eq!(
            by_key.get(&day1).unwrap().temperature_c,
            11.0,
            "day-1 key reads row 8"
        );
        assert_eq!(
            by_key.get(&day2).unwrap().temperature_c,
            29.0,
            "day-2 key reads row 32, not row 8"
        );
    }

    #[test]
    fn day_offset_counts_whole_days_and_clamps() {
        // Same day → 0; the next day → 1; a date before the window start clamps to 0;
        // an undated point or an absent window start → 0.
        assert_eq!(day_offset(Some("2026-07-01"), Some("2026-07-01")), 0);
        assert_eq!(day_offset(Some("2026-07-01"), Some("2026-07-02")), 1);
        assert_eq!(day_offset(Some("2026-07-01"), Some("2026-07-05")), 4);
        assert_eq!(day_offset(Some("2026-07-05"), Some("2026-07-01")), 0);
        assert_eq!(day_offset(Some("2026-07-01"), None), 0);
        assert_eq!(day_offset(None, Some("2026-07-02")), 0);
    }

    #[tokio::test]
    async fn journey_fetches_only_the_cache_miss_points() {
        // Pre-seed the cache with one of two points; a journey over both must fetch
        // ONLY the miss. The stub serves a one-element array (the single miss), and we
        // assert the request lists exactly the miss coordinate.
        let single_body = r#"[{"hourly":{"temperature_2m":[6.0],"precipitation":[9.0],"apparent_temperature":[4.0]}}]"#;
        let (base, seen, _h) = recording_stub(single_body).await;
        let client = WeatherClient::new(reqwest::Client::new(), base);
        let cache = WeatherCache::new();
        let hit_key = (4190, 1250, 0usize, Some("2026-07-01".to_string()));
        let miss_key = (4200, 1300, 0usize, Some("2026-07-01".to_string()));
        // Seed the hit via the public fetch+store path (store is private, so prime it
        // through get_or_fetch_journey on just the hit first).
        cache.store(hit_key.clone(), calm());

        let weather = cache
            .get_or_fetch_journey(&client, &[hit_key.clone(), miss_key.clone()])
            .await;
        // Both points resolved: the hit from the cache, the miss from the one fetch.
        assert!(weather.contains_key(&hit_key));
        assert!(weather.contains_key(&miss_key));
        let q = seen
            .lock()
            .unwrap()
            .clone()
            .expect("the miss triggered a fetch");
        // The request listed ONLY the miss coordinate (42.0,13.0), not the hit.
        assert!(q.contains("latitude=42"), "miss lat: {q}");
        assert!(
            !q.contains("41.9"),
            "the cached point must not be refetched: {q}"
        );
    }

    #[tokio::test]
    async fn fully_cached_journey_makes_zero_calls() {
        let (base, seen, _h) = recording_stub(MULTI_BODY).await;
        let client = WeatherClient::new(reqwest::Client::new(), base);
        let cache = WeatherCache::new();
        let k0 = (4190, 1250, 0usize, Some("2026-07-01".to_string()));
        let k1 = (4200, 1300, 0usize, Some("2026-07-01".to_string()));
        cache.store(k0.clone(), calm());
        cache.store(k1.clone(), foul());
        let weather = cache.get_or_fetch_journey(&client, &[k0, k1]).await;
        assert_eq!(weather.len(), 2);
        assert!(
            seen.lock().unwrap().is_none(),
            "a fully-cached journey makes no outbound call"
        );
    }
}
