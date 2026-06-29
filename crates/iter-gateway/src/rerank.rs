//! Soft, opt-in itinerary reranking over an OTP plan response (ADR 0026, 0028).
//!
//! This module is the **pure, I/O-free core**: it takes an already-parsed OTP
//! plan value plus a reliability-lookup closure and a [`Profile`], and returns
//! the same value with only the `itineraries` array stably reordered (best
//! composite score first). It never prunes an itinerary, never alters
//! leg/feasibility data, and preserves the response schema — the lone additive
//! changes are two optional numeric fields per itinerary (`reliabilityScore` and
//! the composite `rerankScore`), which existing clients ignore.
//!
//! Wave 1 (ADR 0026) scored on reliability alone. Wave 1b (ADR 0028) generalizes
//! the score to a **weighted composite** of independent soft factors, each a pure
//! function over one itinerary:
//!
//! 1. **reliability** — the existing Tier-2 on-time signal; neutral (no effect)
//!    when there is no history.
//! 2. **transfers** — number of transit boardings; fewer preferred.
//! 3. **walking effort** — total walk-leg duration; less preferred.
//! 4. **eco/carbon** — per-mode gCO2e/passenger-km intensity × leg distance,
//!    summed; lower preferred.
//! 5. **weather** — the journey's weather exposure scored by type (ADR 0033,
//!    0035): precipitation hits truly-outdoor minutes (walking + outdoor waiting)
//!    while temperature extremes also hit in-vehicle minutes scaled per mode
//!    (air-conditioned rail/metro sheltered, bus/tram partially exposed). Less
//!    preferred. Neutral (no effect) when no forecast is available — disabled, or
//!    the fetch failed — so existing profile behaviour is unchanged unless weather
//!    is configured.
//!
//! Each raw factor is normalized **across the itineraries in this one response**
//! (min-max, see `normalize_benefit`) into a benefit in `0.0..=1.0` where higher
//! is always better, then combined with per-profile weights into one composite
//! score. The array is stable-sorted by descending composite. A single itinerary,
//! or itineraries that tie on every factor, are left in their original order.
//!
//! A transit leg is keyed for reliability by its
//! `(route gtfsId, trip directionId, boarding-stop gtfsId)`; the closure resolves
//! that key to an on-time rate in `0.0..=1.0`. OTP namespaces its `gtfsId`s as
//! `FEED:LOCALID` (e.g. `ATAC:MEA`), while the reliability index is keyed by the
//! bare local ids the worker recorded from GTFS-RT. We strip the leading feed
//! prefix off the route and stop ids before the lookup so the two id spaces meet
//! (ADR 0027).
//!
//! FAIL-SOFT: every helper here is total. A value that doesn't look like an OTP
//! plan, an itineraries field that isn't an array, or a leg that can't be read
//! all degrade to "no change" / "neutral factor" rather than erroring — the
//! caller returns the original bytes untouched. See [`rerank_plan`].

use serde_json::Value;

use crate::legkey::{is_transit_leg, leg_key};
use crate::weather::{Forecast, weather_penalty};

/// Resolves a transit leg's `(route_id, direction_id, stop_id)` reliability key
/// to an on-time rate in `0.0..=1.0`, or `None` when there is no history. The
/// handler builds this over the on-disk Tier-2 archive; tests pass a synthetic
/// closure. `direction_id` is the OTP `directionId` (commonly `0`/`1`); a leg
/// without one is keyed with `0` by the extractor.
pub type ReliabilityLookup<'a> = dyn Fn(&str, i32, &str) -> Option<f64> + 'a;

/// The neutral reliability rate for a leg or itinerary with no resolvable
/// history. It sits between "all on-time" (1.0) and "never on-time" (0.0) so
/// scored itineraries with real history sort around it.
const NEUTRAL_RELIABILITY: f64 = 0.5;

/// A named scoring profile: the weight each soft factor carries in the composite.
/// The opt-in flag (`?rerank=<profile>`) selects one. Keeping profiles rather
/// than a free-form weight vector keeps the API small and the contract testable
/// (ADR 0028).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// Reliability only — the wave-1 contract (ADR 0026). All other factor weights
    /// are `0`, so it reorders exactly as before and only the reliability factor
    /// matters.
    Reliability,
    /// The full composite, balancing every factor (ADR 0028, 0033).
    Balanced,
    /// Leans on the carbon factor: lowest-emission itineraries first.
    Eco,
    /// Leans on fewer transfers and less walking.
    Comfort,
}

/// The per-factor weights for a profile. Each is a non-negative multiplier on
/// that factor's normalized benefit; the composite is their weighted sum. They
/// need not sum to 1 — the composite is only ever compared within one response,
/// so a common scale is unnecessary.
struct Weights {
    reliability: f64,
    transfers: f64,
    walk: f64,
    eco: f64,
    weather: f64,
}

impl Profile {
    /// Parse the opt-in flag value into a profile. `reliability` preserves the
    /// wave-1 contract; unknown values return `None` so the handler treats them
    /// as "no rerank" and stays a passthrough.
    pub fn from_flag(value: &str) -> Option<Self> {
        match value {
            "reliability" => Some(Self::Reliability),
            "balanced" => Some(Self::Balanced),
            "eco" => Some(Self::Eco),
            "comfort" => Some(Self::Comfort),
            _ => None,
        }
    }

    /// The factor weights for this profile. Tunable consts; the relative sizes,
    /// not the absolute scale, decide the ordering (ADR 0028).
    fn weights(self) -> Weights {
        match self {
            // Wave-1 contract: reliability is the only factor with weight. Weather
            // stays out so this profile reorders exactly as before (ADR 0026).
            Self::Reliability => Weights {
                reliability: 1.0,
                transfers: 0.0,
                walk: 0.0,
                eco: 0.0,
                weather: 0.0,
            },
            // An even-handed blend. Reliability leads; walking gets a gentle
            // weight per the wave-1b brief. Weather joins as a gentle factor — it
            // only bites when a forecast is available *and* the journey is exposed
            // (ADR 0033); with no forecast it is neutral and changes nothing.
            Self::Balanced => Weights {
                reliability: 1.0,
                transfers: 0.6,
                walk: 0.4,
                eco: 0.6,
                weather: 0.5,
            },
            // Carbon dominates; the rest only break near-ties.
            Self::Eco => Weights {
                reliability: 0.3,
                transfers: 0.3,
                walk: 0.2,
                eco: 1.5,
                weather: 0.2,
            },
            // Fewer transfers and less walking dominate; weather exposure weighs
            // heavily here since comfort is what the profile optimizes for.
            Self::Comfort => Weights {
                reliability: 0.5,
                transfers: 1.2,
                walk: 1.0,
                eco: 0.2,
                weather: 1.0,
            },
        }
    }
}

// --- carbon intensities -----------------------------------------------------
//
// Typical published *operational* greenhouse-gas intensities per
// passenger-kilometre, in grams CO2-equivalent (gCO2e/p-km). These are
// order-of-magnitude figures consistent with widely reported transport-emission
// factors (e.g. national environment-agency and EEA passenger-transport
// factors): active modes are zero at the tailpipe; electrified rail/metro/tram
// are low; diesel buses sit higher. They are deliberate, documented estimates,
// not a regional measurement — the reranker only ever compares them *relatively*
// within one response, so the exact values matter less than their ordering.

/// Active travel (walking, cycling): no operational emissions.
const CO2_ACTIVE: f64 = 0.0;
/// Heavy/commuter/regional rail: low-emission electrified traction.
const CO2_RAIL: f64 = 35.0;
/// Metro / subway: electrified, slightly higher than mainline rail per p-km.
const CO2_SUBWAY: f64 = 30.0;
/// Tram / light rail: electrified street running.
const CO2_TRAM: f64 = 30.0;
/// Bus / trolleybus / coach: typically diesel, the higher transit intensity.
const CO2_BUS: f64 = 95.0;
/// Ferry: included as a coarse high estimate; ferries vary widely.
const CO2_FERRY: f64 = 120.0;
/// Car (private, e.g. KISS_AND_RIDE / CAR legs): highest of the set.
const CO2_CAR: f64 = 170.0;
/// Any unrecognized motorized mode: a neutral mid estimate so an unknown mode
/// neither dominates nor disappears.
const CO2_UNKNOWN: f64 = 80.0;

/// Map an OTP leg `mode` string to its carbon intensity (gCO2e/p-km). OTP modes
/// are upper-case (`WALK`, `BUS`, `TRAM`, `SUBWAY`, `RAIL`, `FERRY`, …). Active
/// and waiting modes are zero; a missing/unknown motorized mode gets a neutral
/// mid value.
fn mode_co2_intensity(mode: &str) -> f64 {
    match mode {
        "WALK" | "BICYCLE" | "BIKE" | "SCOOTER" => CO2_ACTIVE,
        "RAIL" | "TRAIN" => CO2_RAIL,
        "SUBWAY" | "METRO" => CO2_SUBWAY,
        "TRAM" | "LIGHT_RAIL" | "CABLE_CAR" | "FUNICULAR" | "GONDOLA" => CO2_TRAM,
        "BUS" | "TROLLEYBUS" | "COACH" => CO2_BUS,
        "FERRY" => CO2_FERRY,
        "CAR" | "CARPOOL" => CO2_CAR,
        _ => CO2_UNKNOWN,
    }
}

/// Reorder `plan.data.plan.itineraries` by descending composite score, stably,
/// using the factor weights of `profile`. Returns `true` when the value was a
/// well-formed plan and the array was (re)scored — even if the order didn't
/// change — and `false` when the value didn't look like an OTP plan, in which
/// case `plan` is left untouched and the caller should return the original
/// response verbatim.
///
/// The reorder is **stable**: equal composite scores preserve OTP's original
/// ordering, so the default engine ranking still breaks ties. A single-itinerary
/// plan, or itineraries that tie on every factor, keep their original order.
///
/// `forecast` is the journey-origin weather for the weather factor (ADR 0033), or
/// `None` when weather is disabled or its fetch failed — in which case every
/// itinerary's weather penalty is `0.0` and the factor is neutral, leaving the
/// other factors' ordering untouched.
pub fn rerank_plan(
    plan: &mut Value,
    lookup: &ReliabilityLookup<'_>,
    profile: Profile,
    forecast: Option<&Forecast>,
) -> bool {
    let Some(itineraries) = plan
        .get_mut("data")
        .and_then(|d| d.get_mut("plan"))
        .and_then(|p| p.get_mut("itineraries"))
        .and_then(Value::as_array_mut)
    else {
        return false;
    };

    // Pull the raw per-itinerary factor values out first, since min-max
    // normalization needs the whole set before any single score is final. The
    // weather penalty is `0.0` for every itinerary when there is no forecast, so a
    // disabled/failed weather fetch leaves the factor neutral (zero span).
    let raw: Vec<Factors> = itineraries
        .iter()
        .map(|it| factors_of(it, lookup, forecast))
        .collect();

    // Normalize each factor across the response into a benefit (higher better).
    let reliability_b =
        normalize_benefit(raw.iter().map(|f| f.reliability), Direction::HigherBetter);
    let transfers_b = normalize_benefit(raw.iter().map(|f| f.transfers), Direction::LowerBetter);
    let walk_b = normalize_benefit(raw.iter().map(|f| f.walk_seconds), Direction::LowerBetter);
    let eco_b = normalize_benefit(raw.iter().map(|f| f.co2_grams), Direction::LowerBetter);
    let weather_b = normalize_benefit(
        raw.iter().map(|f| f.weather_penalty),
        Direction::LowerBetter,
    );

    let w = profile.weights();

    // Score each itinerary in place, then sort by composite descending. Pair the
    // score with the original index so equal scores fall back to it — a stable
    // sort even though `sort_by` is already stable.
    let mut scored: Vec<(usize, f64, Value)> = itineraries
        .drain(..)
        .enumerate()
        .map(|(i, mut it)| {
            let composite = w.reliability * reliability_b[i]
                + w.transfers * transfers_b[i]
                + w.walk * walk_b[i]
                + w.eco * eco_b[i]
                + w.weather * weather_b[i];
            if let Some(obj) = it.as_object_mut() {
                // Keep the wave-1 additive field (the raw reliability factor),
                // and add the composite. Both are additive and optional.
                obj.insert(
                    "reliabilityScore".to_string(),
                    serde_json::json!(round2(raw[i].reliability)),
                );
                obj.insert(
                    "rerankScore".to_string(),
                    serde_json::json!(round2(composite)),
                );
            }
            (i, composite, it)
        })
        .collect();

    // Descending composite, original index as the stable tie-breaker.
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });

    *itineraries = scored.into_iter().map(|(_, _, it)| it).collect();
    true
}

/// The raw (un-normalized) factor values for one itinerary. Each is a total
/// function of the itinerary's legs; missing/malformed data degrades to a neutral
/// or zero contribution, never a panic.
struct Factors {
    /// Mean on-time rate across transit legs with history (`0.0..=1.0`);
    /// [`NEUTRAL_RELIABILITY`] when none.
    reliability: f64,
    /// Number of transit boardings (fewer is better).
    transfers: f64,
    /// Total walk-leg duration in seconds (less is better).
    walk_seconds: f64,
    /// Total estimated gCO2e across legs (lower is better).
    co2_grams: f64,
    /// Weather penalty: `precip_badness × outdoor-minutes + temp_badness ×
    /// (outdoor + mode-scaled in-vehicle) minutes` (lower is better; ADR 0035).
    /// `0.0` when there is no forecast, so the factor is neutral (ADR 0033).
    weather_penalty: f64,
}

/// Compute every soft factor for one itinerary. The leg-derived factors come from
/// a single pass over the legs; the weather penalty is a pure function of the
/// itinerary and the journey forecast (`0.0` when no forecast is available).
fn factors_of(
    itinerary: &Value,
    lookup: &ReliabilityLookup<'_>,
    forecast: Option<&Forecast>,
) -> Factors {
    let weather = forecast.map_or(0.0, |f| weather_penalty(itinerary, f));

    let Some(legs) = itinerary.get("legs").and_then(Value::as_array) else {
        return Factors {
            reliability: NEUTRAL_RELIABILITY,
            transfers: 0.0,
            walk_seconds: 0.0,
            co2_grams: 0.0,
            weather_penalty: weather,
        };
    };

    let mut rel_sum = 0.0;
    let mut rel_n = 0u32;
    let mut transfers = 0u32;
    let mut walk_seconds = 0.0;
    let mut co2_grams = 0.0;

    for leg in legs {
        let mode = leg.get("mode").and_then(Value::as_str).unwrap_or("");
        let is_transit = is_transit_leg(leg);

        if is_transit {
            transfers += 1;
            if let Some(rate) = leg_on_time_rate(leg, lookup) {
                rel_sum += rate;
                rel_n += 1;
            }
        } else if mode == "WALK" {
            walk_seconds += leg_duration_seconds(leg);
        }

        // Carbon over every leg with a distance (transit and active alike).
        let distance_m = leg.get("distance").and_then(Value::as_f64).unwrap_or(0.0);
        if distance_m > 0.0 {
            let km = distance_m / 1000.0;
            co2_grams += mode_co2_intensity(mode) * km;
        }
    }

    let reliability = if rel_n == 0 {
        NEUTRAL_RELIABILITY
    } else {
        rel_sum / f64::from(rel_n)
    };

    Factors {
        reliability,
        // A trip with N boardings has N-1 transfers, but the absolute count is
        // monotone in boardings, so boarding count is a fine ordering signal.
        transfers: f64::from(transfers),
        walk_seconds,
        co2_grams,
        weather_penalty: weather,
    }
}

/// A leg's duration in seconds, from `duration` (OTP reports leg duration in
/// seconds), defaulting to `0.0` when absent or malformed.
fn leg_duration_seconds(leg: &Value) -> f64 {
    leg.get("duration").and_then(Value::as_f64).unwrap_or(0.0)
}

/// Resolve a single transit leg to its on-time rate, or `None` when the leg is
/// non-transit or can't be keyed/resolved. The direction comes from
/// `trip.directionId` (default `0`) and the stop from the boarding
/// `from.stop.gtfsId`.
fn leg_on_time_rate(leg: &Value, lookup: &ReliabilityLookup<'_>) -> Option<f64> {
    let key = leg_key(leg)?;
    lookup(key.route, key.direction, key.stop)
}

/// Which direction of a raw factor is "good".
#[derive(Clone, Copy)]
enum Direction {
    HigherBetter,
    LowerBetter,
}

/// Min-max normalize raw factor values across one response into a benefit in
/// `0.0..=1.0` where **higher is always better**. We min-max (not rank) so the
/// magnitude of a difference is preserved — two itineraries a hair apart score a
/// hair apart, not a full rank apart. When every value is equal (or there is one
/// itinerary) the spread is zero, so every benefit is the neutral `0.5` and the
/// factor cannot reorder anything (the stable sort then holds original order).
///
/// `HigherBetter` maps the max to 1.0; `LowerBetter` flips so the min maps to
/// 1.0. Non-finite inputs are coerced to the neutral midpoint.
fn normalize_benefit(values: impl Iterator<Item = f64>, dir: Direction) -> Vec<f64> {
    let raw: Vec<f64> = values
        .map(|v| {
            if v.is_finite() {
                v
            } else {
                NEUTRAL_RELIABILITY
            }
        })
        .collect();
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for &v in &raw {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    let span = hi - lo;
    raw.iter()
        .map(|&v| {
            if span <= 0.0 {
                0.5 // all equal → neutral, no reordering from this factor
            } else {
                let unit = (v - lo) / span; // 0 at min, 1 at max
                match dir {
                    Direction::HigherBetter => unit,
                    Direction::LowerBetter => 1.0 - unit,
                }
            }
        })
        .collect()
}

/// Round to two decimals so the additive scores are stable and small on the wire.
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A lookup with no history at all — every leg scores neutral reliability.
    fn no_history(_r: &str, _d: i32, _s: &str) -> Option<f64> {
        None
    }

    /// A synthetic plan with the given itineraries.
    fn plan_with(itineraries: Vec<Value>) -> Value {
        json!({ "data": { "plan": { "itineraries": itineraries } } })
    }

    /// A transit leg keyed by (route, direction, stop), with a mode + distance.
    fn transit_leg(route: &str, direction: i64, stop: &str) -> Value {
        transit_leg_dist(route, direction, stop, 1000.0)
    }

    /// A transit leg with an explicit distance, so a test can hold total distance
    /// (and thus carbon) constant while varying the number of boardings.
    fn transit_leg_dist(route: &str, direction: i64, stop: &str, distance: f64) -> Value {
        json!({
            "transitLeg": true,
            "mode": "BUS",
            "distance": distance,
            "route": { "gtfsId": route },
            "trip": { "directionId": direction },
            "from": { "stop": { "gtfsId": stop } },
        })
    }

    fn walk_leg() -> Value {
        json!({ "transitLeg": false, "mode": "WALK", "duration": 300.0, "distance": 400.0 })
    }

    /// Order the itineraries by reading back the first leg's route id.
    fn route_order(plan: &Value) -> Vec<String> {
        plan["data"]["plan"]["itineraries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|it| {
                it["legs"][0]["route"]["gtfsId"]
                    .as_str()
                    .unwrap_or("")
                    .to_string()
            })
            .collect()
    }

    // --- reliability factor (wave-1 contract carried forward) ----------------

    #[test]
    fn reliability_profile_reorders_best_on_time_first() {
        let mut plan = plan_with(vec![
            json!({ "legs": [transit_leg("A", 0, "s1")] }),
            json!({ "legs": [transit_leg("B", 0, "s2")] }),
            json!({ "legs": [transit_leg("C", 0, "s3")] }),
        ]);
        let lookup = |route: &str, _d: i32, _s: &str| match route {
            "A" => Some(0.40),
            "B" => Some(0.95),
            "C" => Some(0.70),
            _ => None,
        };
        assert!(rerank_plan(&mut plan, &lookup, Profile::Reliability, None));
        assert_eq!(route_order(&plan), vec!["B", "C", "A"]);
    }

    #[test]
    fn reliability_factor_is_neutral_when_tier2_absent() {
        // Flag set, no history → all itineraries tie at neutral, order held.
        let mut plan = plan_with(vec![
            json!({ "legs": [transit_leg("A", 0, "s1")] }),
            json!({ "legs": [transit_leg("B", 0, "s2")] }),
        ]);
        assert!(rerank_plan(
            &mut plan,
            &no_history,
            Profile::Reliability,
            None
        ));
        assert_eq!(route_order(&plan), vec!["A", "B"]);
        // The additive reliability factor reads back as neutral.
        assert_eq!(
            plan["data"]["plan"]["itineraries"][0]["reliabilityScore"],
            json!(0.5)
        );
    }

    #[test]
    fn reliability_profile_is_a_stable_sort_on_ties() {
        let mut plan = plan_with(vec![
            json!({ "legs": [transit_leg("A", 0, "s")] }),
            json!({ "legs": [transit_leg("B", 0, "s")] }),
            json!({ "legs": [transit_leg("C", 0, "s")] }),
        ]);
        let lookup = |_r: &str, _d: i32, _s: &str| Some(0.80);
        assert!(rerank_plan(&mut plan, &lookup, Profile::Reliability, None));
        assert_eq!(route_order(&plan), vec!["A", "B", "C"]);
    }

    #[test]
    fn reliability_profile_attaches_additive_scores() {
        let mut plan = plan_with(vec![json!({ "legs": [transit_leg("A", 0, "s")] })]);
        let lookup = |_r: &str, _d: i32, _s: &str| Some(0.75);
        assert!(rerank_plan(&mut plan, &lookup, Profile::Reliability, None));
        let it = &plan["data"]["plan"]["itineraries"][0];
        assert_eq!(it["reliabilityScore"], json!(0.75));
        // The composite is present and equals the expected weighted sum: with one
        // itinerary every factor's span is zero, so each benefit is the neutral
        // 0.5; under Reliability weights (1,0,0,0) the composite is 1.0*0.5 = 0.5.
        assert_eq!(it["rerankScore"], json!(0.5));
        assert_eq!(it["legs"][0]["route"]["gtfsId"], "A");
    }

    #[test]
    fn rerank_score_equals_the_normalized_reliability_benefit() {
        // Two itineraries differing only in reliability (0.9 vs 0.3). Under the
        // Reliability profile the composite is exactly the normalized reliability
        // benefit: min-max over {0.9, 0.3} maps the higher to 1.0 and the lower to
        // 0.0, weighted by 1.0. This pins round2 and the weighted-sum wiring to a
        // hand-computable value, not just a relative order.
        let mut plan = plan_with(vec![
            json!({ "legs": [transit_leg("HI", 0, "s")] }),
            json!({ "legs": [transit_leg("LO", 0, "s")] }),
        ]);
        let lookup = |route: &str, _d: i32, _s: &str| match route {
            "HI" => Some(0.90),
            "LO" => Some(0.30),
            _ => None,
        };
        assert!(rerank_plan(&mut plan, &lookup, Profile::Reliability, None));
        let its = plan["data"]["plan"]["itineraries"].as_array().unwrap();
        assert_eq!(its[0]["legs"][0]["route"]["gtfsId"], "HI");
        assert_eq!(its[0]["rerankScore"], json!(1.0));
        assert_eq!(its[1]["rerankScore"], json!(0.0));
    }

    #[test]
    fn direction_id_is_part_of_the_reliability_key() {
        let mut plan = plan_with(vec![
            json!({ "legs": [transit_leg("R", 1, "s")] }),
            json!({ "legs": [transit_leg("R", 0, "s")] }),
        ]);
        let lookup = |_r: &str, dir: i32, _s: &str| match dir {
            0 => Some(0.90),
            1 => Some(0.20),
            _ => None,
        };
        assert!(rerank_plan(&mut plan, &lookup, Profile::Reliability, None));
        let dirs: Vec<i64> = plan["data"]["plan"]["itineraries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|it| it["legs"][0]["trip"]["directionId"].as_i64().unwrap())
            .collect();
        assert_eq!(dirs, vec![0, 1]);
    }

    #[test]
    fn otp_feed_prefixed_ids_match_unprefixed_index_keys() {
        let mut plan = plan_with(vec![
            json!({ "legs": [transit_leg("ATAC:SLOW", 0, "ATAC:70001")] }),
            json!({ "legs": [transit_leg("ATAC:FAST", 0, "ATAC:70001")] }),
        ]);
        let lookup = |route: &str, _d: i32, stop: &str| match (route, stop) {
            ("FAST", "70001") => Some(0.95),
            ("SLOW", "70001") => Some(0.10),
            _ => None,
        };
        assert!(rerank_plan(&mut plan, &lookup, Profile::Reliability, None));
        assert_eq!(route_order(&plan), vec!["ATAC:FAST", "ATAC:SLOW"]);
    }

    // --- per-factor monotonicity --------------------------------------------

    #[test]
    fn comfort_prefers_fewer_transfers() {
        // Isolate the transfers factor: both itineraries cover the SAME total
        // distance (1200m) in the SAME mode, so carbon ties exactly; neither walks
        // and neither has history, so walk and reliability tie too. The only thing
        // that differs is the boarding count — one leg vs three 400m legs. With
        // every other factor neutral, Comfort's transfers weight alone decides, so
        // this proves the transfers factor (not carbon) drives the order.
        let one = json!({ "legs": [transit_leg_dist("ONE", 0, "s", 1200.0)] });
        let three = json!({ "legs": [
            transit_leg_dist("T1", 0, "s", 400.0),
            transit_leg_dist("T2", 0, "s", 400.0),
            transit_leg_dist("T3", 0, "s", 400.0),
        ]});
        let mut plan = plan_with(vec![three, one]);
        assert!(rerank_plan(&mut plan, &no_history, Profile::Comfort, None));
        // The one-board itinerary (route ONE) sorts to the front.
        assert_eq!(route_order(&plan), vec!["ONE", "T1"]);
    }

    #[test]
    fn factors_of_counts_boardings_and_walk_directly() {
        // A direct read of the raw factors: three transit boardings and a 300s walk
        // leg → transfers == 3, walk_seconds == 300, independent of any other
        // factor. This pins the transfers/walk extraction without going through the
        // composite, where carbon could otherwise mask it.
        let it = json!({ "legs": [
            transit_leg("A", 0, "s"),
            transit_leg("B", 0, "s"),
            transit_leg("C", 0, "s"),
            walk_leg(),
        ]});
        let f = factors_of(&it, &no_history, None);
        assert_eq!(f.transfers, 3.0);
        assert_eq!(f.walk_seconds, 300.0);
    }

    #[test]
    fn comfort_prefers_less_walking() {
        // Same single transit leg; one itinerary adds a long walk leg. Comfort
        // penalizes walking → the no-walk itinerary leads.
        let little_walk = json!({ "legs": [
            json!({ "transitLeg": false, "mode": "WALK", "duration": 60.0, "distance": 80.0 }),
            transit_leg("LOW", 0, "s"),
        ]});
        let lots_walk = json!({ "legs": [
            json!({ "transitLeg": false, "mode": "WALK", "duration": 1800.0, "distance": 2400.0 }),
            transit_leg("HIGH", 0, "s"),
        ]});
        let mut plan = plan_with(vec![lots_walk, little_walk]);
        assert!(rerank_plan(&mut plan, &no_history, Profile::Comfort, None));
        // The light-walk itinerary's first leg is the walk leg; assert via the
        // transit leg id at index 1.
        let routes: Vec<String> = plan["data"]["plan"]["itineraries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|it| {
                it["legs"][1]["route"]["gtfsId"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(routes, vec!["LOW", "HIGH"]);
    }

    #[test]
    fn eco_prefers_lower_emissions() {
        // Same distance, different modes: a metro leg (low gCO2e) vs a bus leg
        // (high). Eco weights carbon heavily → the metro itinerary leads.
        let metro = json!({ "legs": [json!({
            "transitLeg": true, "mode": "SUBWAY", "distance": 5000.0,
            "route": { "gtfsId": "METRO" }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s" } },
        })]});
        let bus = json!({ "legs": [json!({
            "transitLeg": true, "mode": "BUS", "distance": 5000.0,
            "route": { "gtfsId": "BUSLINE" }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s" } },
        })]});
        let mut plan = plan_with(vec![bus, metro]);
        assert!(rerank_plan(&mut plan, &no_history, Profile::Eco, None));
        assert_eq!(route_order(&plan), vec!["METRO", "BUSLINE"]);
    }

    #[test]
    fn eco_treats_active_modes_as_zero_carbon() {
        // A pure-walk itinerary (zero carbon) beats a bus itinerary under eco.
        let walk_only = json!({ "legs": [walk_leg()] });
        let bus = json!({ "legs": [transit_leg("BUSLINE", 0, "s")] });
        let mut plan = plan_with(vec![bus, walk_only]);
        assert!(rerank_plan(&mut plan, &no_history, Profile::Eco, None));
        // The walk-only itinerary has no route at leg 0; check it landed first by
        // confirming the bus itinerary is now last.
        let last = &plan["data"]["plan"]["itineraries"][1];
        assert_eq!(last["legs"][0]["route"]["gtfsId"], "BUSLINE");
    }

    // --- composite & profiles ------------------------------------------------

    #[test]
    fn profiles_weight_the_same_plan_differently() {
        // Itinerary FAST: one short bus leg, good reliability, no walk.
        // Itinerary GREEN: one metro leg (low carbon), poor reliability.
        // Eco should prefer GREEN; reliability should prefer FAST.
        let fast = json!({ "legs": [json!({
            "transitLeg": true, "mode": "BUS", "distance": 3000.0,
            "route": { "gtfsId": "FAST" }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s1" } },
        })]});
        let green = json!({ "legs": [json!({
            "transitLeg": true, "mode": "SUBWAY", "distance": 3000.0,
            "route": { "gtfsId": "GREEN" }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s2" } },
        })]});
        let lookup = |route: &str, _d: i32, _s: &str| match route {
            "FAST" => Some(0.95),
            "GREEN" => Some(0.20),
            _ => None,
        };

        let mut p_rel = plan_with(vec![green.clone(), fast.clone()]);
        assert!(rerank_plan(&mut p_rel, &lookup, Profile::Reliability, None));
        assert_eq!(route_order(&p_rel), vec!["FAST", "GREEN"]);

        let mut p_eco = plan_with(vec![fast, green]);
        assert!(rerank_plan(&mut p_eco, &lookup, Profile::Eco, None));
        assert_eq!(route_order(&p_eco), vec!["GREEN", "FAST"]);
    }

    #[test]
    fn single_itinerary_is_never_reordered_and_factors_are_neutral() {
        // One itinerary → every factor's spread is zero → benefit 0.5 → no change.
        let mut plan = plan_with(vec![json!({ "legs": [transit_leg("ONLY", 0, "s")] })]);
        assert!(rerank_plan(&mut plan, &no_history, Profile::Balanced, None));
        assert_eq!(route_order(&plan), vec!["ONLY"]);
    }

    #[test]
    fn all_equal_itineraries_keep_original_order() {
        // Three itineraries with distinct route ids but identical factor values →
        // tie on every factor → stable order.
        let mut plan = plan_with(vec![
            json!({ "legs": [transit_leg("A", 0, "s")] }),
            json!({ "legs": [transit_leg("B", 0, "s")] }),
            json!({ "legs": [transit_leg("C", 0, "s")] }),
        ]);
        assert!(rerank_plan(&mut plan, &no_history, Profile::Balanced, None));
        assert_eq!(route_order(&plan), vec!["A", "B", "C"]);
    }

    // --- weather factor (ADR 0033) ------------------------------------------

    /// A foul-weather forecast: heavy rain, so any exposed minutes are penalized.
    fn foul() -> Forecast {
        Forecast {
            temperature_c: 8.0,
            precipitation_mm: 8.0,
            apparent_temperature_c: Some(7.0),
        }
    }

    /// A calm forecast: comfortable and dry, so exposure costs nothing.
    fn fair() -> Forecast {
        Forecast {
            temperature_c: 20.0,
            precipitation_mm: 0.0,
            apparent_temperature_c: Some(20.0),
        }
    }

    /// An itinerary that walks `secs` seconds plus one sheltered transit leg, so
    /// the two siblings differ only in exposed (walk) time.
    fn walk_then_ride(route: &str, walk_secs: f64) -> Value {
        json!({ "legs": [
            json!({ "transitLeg": false, "mode": "WALK", "duration": walk_secs }),
            transit_leg(route, 0, "s"),
        ]})
    }

    #[test]
    fn weather_factor_ranks_more_exposure_lower_in_bad_weather() {
        // Two siblings: SHELTERED walks 60s, EXPOSED walks 1800s. Everything else
        // ties (same mode/distance, no history). Under the Comfort profile with a
        // foul forecast the weather factor penalizes the long exposed walk, so the
        // sheltered itinerary leads.
        let mut plan = plan_with(vec![
            walk_then_ride("EXPOSED", 1800.0),
            walk_then_ride("SHELTERED", 60.0),
        ]);
        assert!(rerank_plan(
            &mut plan,
            &no_history,
            Profile::Comfort,
            Some(&foul())
        ));
        // Read the transit-leg id at index 1 (index 0 is the walk leg).
        let routes: Vec<String> = plan["data"]["plan"]["itineraries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|it| {
                it["legs"][1]["route"]["gtfsId"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(routes, vec!["SHELTERED", "EXPOSED"]);
    }

    /// A hot, dry forecast: temperature extreme, no rain — so in-vehicle exposure
    /// is mode-scaled and a bus is hotter than metro/rail (ADR 0035).
    fn hot() -> Forecast {
        Forecast {
            temperature_c: 40.0,
            precipitation_mm: 0.0,
            apparent_temperature_c: Some(40.0),
        }
    }

    /// A transit leg of an explicit mode and duration, sheltered from rain.
    fn ride_leg(mode: &str, route: &str, secs: f64) -> Value {
        json!({
            "transitLeg": true, "mode": mode, "duration": secs,
            "route": { "gtfsId": route }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s" } },
        })
    }

    #[test]
    fn rain_favors_an_in_vehicle_route_over_a_walking_one() {
        // In the rain a route that spends its time in a vehicle (even a bus) beats
        // one with a long outdoor walk: precipitation hits outdoor-only time, so the
        // bus ride takes no rain penalty while the walk does (ADR 0035). RIDE walks
        // 60s then buses; WALK walks 1800s then buses — same modes, only walk time
        // differs. A heavy-rain, comfortable-temperature forecast isolates precip.
        let rainy = Forecast {
            temperature_c: 18.0,
            precipitation_mm: 8.0,
            apparent_temperature_c: Some(18.0),
        };
        let ride = json!({ "legs": [
            json!({ "transitLeg": false, "mode": "WALK", "duration": 60.0 }),
            ride_leg("BUS", "RIDE", 1800.0),
        ]});
        let walk = json!({ "legs": [
            json!({ "transitLeg": false, "mode": "WALK", "duration": 1800.0 }),
            ride_leg("BUS", "WALK", 60.0),
        ]});
        let mut plan = plan_with(vec![walk, ride]);
        assert!(rerank_plan(
            &mut plan,
            &no_history,
            Profile::Comfort,
            Some(&rainy)
        ));
        let routes: Vec<String> = plan["data"]["plan"]["itineraries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|it| {
                it["legs"][1]["route"]["gtfsId"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(routes, vec!["RIDE", "WALK"]);
    }

    #[test]
    fn heat_favors_a_metro_route_over_an_equivalent_bus_route() {
        // In a heatwave two otherwise-equivalent routes — same 1800s in-vehicle
        // time, same (small) walk, no history — differ only in mode: METRO rides a
        // SUBWAY leg, BUSY rides a BUS leg. The per-mode temperature coefficient
        // bites: the bus cabin is hotter, so the metro route ranks ahead (ADR 0035).
        // `ride_leg` carries no `distance`, so the eco/carbon factor is zero for both
        // (carbon only accrues on legs with `distance`); the weather coefficient is
        // the only differing factor, keeping this isolation edit-proof.
        let metro = json!({ "legs": [
            json!({ "transitLeg": false, "mode": "WALK", "duration": 60.0 }),
            ride_leg("SUBWAY", "METRO", 1800.0),
        ]});
        let busy = json!({ "legs": [
            json!({ "transitLeg": false, "mode": "WALK", "duration": 60.0 }),
            ride_leg("BUS", "BUSY", 1800.0),
        ]});
        let mut plan = plan_with(vec![busy, metro]);
        assert!(rerank_plan(
            &mut plan,
            &no_history,
            Profile::Comfort,
            Some(&hot())
        ));
        let routes: Vec<String> = plan["data"]["plan"]["itineraries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|it| {
                it["legs"][1]["route"]["gtfsId"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(routes, vec!["METRO", "BUSY"]);
    }

    #[test]
    fn weather_factor_is_neutral_in_good_weather() {
        // The same two siblings, but a fair forecast → zero penalty for both → the
        // weather factor cannot reorder; the stable sort holds OTP's order. (Walk
        // still differs, but Comfort's walk weight is the same either way; isolate
        // weather by using a profile-free check: with a fair forecast the order is
        // identical to the no-forecast order.)
        let build = || {
            plan_with(vec![
                walk_then_ride("FIRST", 60.0),
                walk_then_ride("SECOND", 60.0), // equal walk → only weather could differ
            ])
        };
        let mut with_fair = build();
        let mut without = build();
        assert!(rerank_plan(
            &mut with_fair,
            &no_history,
            Profile::Comfort,
            Some(&fair())
        ));
        assert!(rerank_plan(
            &mut without,
            &no_history,
            Profile::Comfort,
            None
        ));
        let order = |p: &Value| {
            p["data"]["plan"]["itineraries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|it| {
                    it["legs"][1]["route"]["gtfsId"]
                        .as_str()
                        .unwrap()
                        .to_string()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(order(&with_fair), vec!["FIRST", "SECOND"]);
        assert_eq!(order(&with_fair), order(&without));
    }

    #[test]
    fn weather_factor_is_neutral_without_a_forecast() {
        // No forecast at all → every weather penalty is zero → the factor adds
        // nothing, so the order matches what the other factors alone would produce.
        let mut plan = plan_with(vec![
            walk_then_ride("EXPOSED", 1800.0),
            walk_then_ride("SHELTERED", 60.0),
        ]);
        // Reliability profile has zero weather weight anyway, but pass None to pin
        // the disabled contract: it must not panic and must complete.
        assert!(rerank_plan(
            &mut plan,
            &no_history,
            Profile::Reliability,
            None
        ));
        // Reliability-only with no history → all tie → stable order preserved.
        let routes: Vec<String> = plan["data"]["plan"]["itineraries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|it| {
                it["legs"][1]["route"]["gtfsId"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(routes, vec!["EXPOSED", "SHELTERED"]);
    }

    #[test]
    fn reliability_profile_ignores_weather_even_when_supplied() {
        // The wave-1 contract: even with a foul forecast, the Reliability profile's
        // zero weather weight means weather never reorders. With history, only
        // reliability decides.
        let mut plan = plan_with(vec![
            walk_then_ride("LOWREL", 1800.0), // long exposed walk but...
            walk_then_ride("HIGHREL", 60.0),
        ]);
        let lookup = |route: &str, _d: i32, _s: &str| match route {
            "HIGHREL" => Some(0.95),
            "LOWREL" => Some(0.20),
            _ => None,
        };
        assert!(rerank_plan(
            &mut plan,
            &lookup,
            Profile::Reliability,
            Some(&foul())
        ));
        let routes: Vec<String> = plan["data"]["plan"]["itineraries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|it| {
                it["legs"][1]["route"]["gtfsId"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        // HIGHREL leads on reliability alone; weather is ignored under this profile.
        assert_eq!(routes, vec!["HIGHREL", "LOWREL"]);
    }

    #[test]
    fn balanced_blends_factors_against_a_single_factor_order() {
        // A constructed two-itinerary case where the Balanced blend tips the OTHER
        // way from a pure-eco ordering, proving the weighted sum (not one dominant
        // factor) decides. With two itineraries each factor is 0/1, so we can hand
        // compute the composites under Balanced weights (rel=1.0, tr=0.6, walk=0.4,
        // eco=0.6, weather=0.5). With no forecast (None) the weather penalty is 0.0
        // for both → equal → benefit 0.5 each → a flat +0.5*0.5 = 0.25 on every
        // composite that does not change the order:
        //
        //   DIRECT: one BUS leg (higher carbon), 1 boarding, reliability 0.95.
        //     rel benefit 1, transfers benefit 1, eco benefit 0, walk tie 0.5,
        //     weather tie 0.5
        //     → 1.0*1 + 0.6*1 + 0.4*0.5 + 0.6*0 + 0.5*0.5 = 2.05
        //   GREEN: two SUBWAY legs (low carbon), 2 boardings, reliability 0.20.
        //     rel benefit 0, transfers benefit 0, eco benefit 1, walk tie 0.5,
        //     weather tie 0.5
        //     → 1.0*0 + 0.6*0 + 0.4*0.5 + 0.6*1 + 0.5*0.5 = 1.05
        //
        // Balanced ranks DIRECT first; a pure-eco order would rank GREEN first. So
        // asserting DIRECT leads can only hold if the blend, not eco alone, decides.
        let direct = json!({ "legs": [json!({
            "transitLeg": true, "mode": "BUS", "distance": 4000.0,
            "route": { "gtfsId": "DIRECT" }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s" } } })]});
        let green = json!({ "legs": [
            json!({ "transitLeg": true, "mode": "SUBWAY", "distance": 2000.0,
                "route": { "gtfsId": "G1" }, "trip": { "directionId": 0 },
                "from": { "stop": { "gtfsId": "s" } } }),
            json!({ "transitLeg": true, "mode": "SUBWAY", "distance": 2000.0,
                "route": { "gtfsId": "G2" }, "trip": { "directionId": 0 },
                "from": { "stop": { "gtfsId": "s" } } }),
        ]});
        let lookup = |route: &str, _d: i32, _s: &str| match route {
            "DIRECT" => Some(0.95),
            "G1" | "G2" => Some(0.20),
            _ => None,
        };
        let mut plan = plan_with(vec![green, direct]);
        assert!(rerank_plan(&mut plan, &lookup, Profile::Balanced, None));
        // The blend lifts DIRECT above the lower-carbon GREEN; eco alone would not.
        assert_eq!(route_order(&plan), vec!["DIRECT", "G1"]);
        // Pin the composites the blend produced, confirming the weighted-sum wiring.
        let its = plan["data"]["plan"]["itineraries"].as_array().unwrap();
        assert_eq!(its[0]["rerankScore"], json!(2.05));
        assert_eq!(its[1]["rerankScore"], json!(1.05));
    }

    #[test]
    fn profile_from_flag_parses_known_profiles_only() {
        assert_eq!(
            Profile::from_flag("reliability"),
            Some(Profile::Reliability)
        );
        assert_eq!(Profile::from_flag("balanced"), Some(Profile::Balanced));
        assert_eq!(Profile::from_flag("eco"), Some(Profile::Eco));
        assert_eq!(Profile::from_flag("comfort"), Some(Profile::Comfort));
        assert_eq!(Profile::from_flag("nonsense"), None);
        assert_eq!(Profile::from_flag(""), None);
    }

    #[test]
    fn carbon_intensity_orders_modes_sensibly() {
        // Active = 0 < electrified rail/metro/tram < bus < car. The exact values
        // are estimates; the ordering is the contract.
        assert_eq!(mode_co2_intensity("WALK"), 0.0);
        assert_eq!(mode_co2_intensity("BICYCLE"), 0.0);
        assert!(mode_co2_intensity("SUBWAY") < mode_co2_intensity("BUS"));
        assert!(mode_co2_intensity("TRAM") < mode_co2_intensity("BUS"));
        assert!(mode_co2_intensity("RAIL") < mode_co2_intensity("BUS"));
        assert!(mode_co2_intensity("BUS") < mode_co2_intensity("CAR"));
        // FERRY sits at the high end, at or above bus.
        assert!(mode_co2_intensity("FERRY") >= mode_co2_intensity("BUS"));
        // An unknown motorized mode is a finite mid estimate, never zero, and
        // lands between the low-transit band and a car — neither dominating nor
        // disappearing.
        assert!(mode_co2_intensity("ZEPPELIN") > 0.0);
        assert!(mode_co2_intensity("SUBWAY") < mode_co2_intensity("ZEPPELIN"));
        assert!(mode_co2_intensity("ZEPPELIN") < mode_co2_intensity("CAR"));
    }

    // --- min-max normalization ----------------------------------------------

    #[test]
    fn normalize_higher_better_maps_max_to_one() {
        let b = normalize_benefit([0.0, 5.0, 10.0].into_iter(), Direction::HigherBetter);
        assert_eq!(b, vec![0.0, 0.5, 1.0]);
    }

    #[test]
    fn normalize_lower_better_flips() {
        let b = normalize_benefit([0.0, 5.0, 10.0].into_iter(), Direction::LowerBetter);
        assert_eq!(b, vec![1.0, 0.5, 0.0]);
    }

    #[test]
    fn normalize_all_equal_is_neutral() {
        let b = normalize_benefit([7.0, 7.0, 7.0].into_iter(), Direction::LowerBetter);
        assert_eq!(b, vec![0.5, 0.5, 0.5]);
    }

    #[test]
    fn normalize_non_finite_is_coerced_to_neutral() {
        let b = normalize_benefit([f64::NAN, 0.0, 1.0].into_iter(), Direction::HigherBetter);
        // NaN became 0.5, the span is [0,1], so it maps to 0.5.
        assert_eq!(b[0], 0.5);
    }

    // --- fail-soft -----------------------------------------------------------

    #[test]
    fn non_plan_value_is_left_untouched() {
        let mut not_a_plan = json!({ "errors": [{ "message": "boom" }] });
        let before = not_a_plan.clone();
        assert!(!rerank_plan(
            &mut not_a_plan,
            &no_history,
            Profile::Balanced,
            None
        ));
        assert_eq!(not_a_plan, before);

        let mut wrong_shape = json!({ "data": { "plan": { "itineraries": "nope" } } });
        let before2 = wrong_shape.clone();
        assert!(!rerank_plan(
            &mut wrong_shape,
            &no_history,
            Profile::Balanced,
            None
        ));
        assert_eq!(wrong_shape, before2);

        let mut bare = json!(42);
        assert!(!rerank_plan(
            &mut bare,
            &no_history,
            Profile::Balanced,
            None
        ));
        assert_eq!(bare, json!(42));
    }

    #[test]
    fn empty_itineraries_is_a_no_op_success() {
        let mut plan = plan_with(vec![]);
        assert!(rerank_plan(&mut plan, &no_history, Profile::Balanced, None));
        assert_eq!(
            plan["data"]["plan"]["itineraries"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn malformed_legs_never_panic_and_score_neutral() {
        let plan = plan_with(vec![
            json!({ "legs": [json!(7), json!({ "route": "not-an-object" })] }),
            json!({ "legs": "not-an-array" }),
            json!({ "no": "legs" }),
        ]);
        // Must not panic across every profile; all three are kept.
        for profile in [
            Profile::Reliability,
            Profile::Balanced,
            Profile::Eco,
            Profile::Comfort,
        ] {
            let mut p = plan.clone();
            assert!(rerank_plan(&mut p, &no_history, profile, None));
            assert_eq!(
                p["data"]["plan"]["itineraries"].as_array().unwrap().len(),
                3
            );
        }
    }

    #[test]
    fn transit_leg_without_boarding_stop_scores_neutral_reliability() {
        let no_stop = json!({
            "transitLeg": true, "mode": "BUS", "distance": 1000.0,
            "route": { "gtfsId": "R" }, "trip": { "directionId": 0 },
        });
        let mut plan = plan_with(vec![
            json!({ "legs": [no_stop] }),
            json!({ "legs": [transit_leg("OTHER", 0, "s")] }),
        ]);
        assert!(rerank_plan(
            &mut plan,
            &no_history,
            Profile::Reliability,
            None
        ));
        assert_eq!(
            plan["data"]["plan"]["itineraries"][0]["reliabilityScore"],
            json!(0.5)
        );
        assert_eq!(route_order(&plan), vec!["R", "OTHER"]);
    }

    #[test]
    fn rerank_is_idempotent_on_a_second_pass() {
        let mut plan = plan_with(vec![
            json!({ "legs": [transit_leg("A", 0, "s")] }),
            json!({ "legs": [transit_leg("B", 0, "s")] }),
        ]);
        let lookup = |route: &str, _d: i32, _s: &str| match route {
            "A" => Some(0.30),
            "B" => Some(0.80),
            _ => None,
        };
        assert!(rerank_plan(&mut plan, &lookup, Profile::Reliability, None));
        let order1 = route_order(&plan);
        assert!(rerank_plan(&mut plan, &lookup, Profile::Reliability, None));
        assert_eq!(route_order(&plan), order1, "order is a fixed point");
    }
}
