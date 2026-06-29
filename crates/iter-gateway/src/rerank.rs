//! Soft, opt-in itinerary reranking over an OTP plan response (ADR 0026).
//!
//! This module is the **pure, I/O-free core**: it takes an already-parsed OTP
//! plan value plus a reliability-lookup closure and returns the same value with
//! only the `itineraries` array stably reordered (best reliability first). It
//! never prunes an itinerary, never alters leg/feasibility data, and preserves
//! the response schema — the lone additive change is an optional numeric
//! `reliabilityScore` attached per itinerary, which existing clients ignore.
//!
//! Wave 1 scores on reliability alone. A transit leg is keyed by its
//! `(route gtfsId, trip directionId, boarding-stop gtfsId)`; the closure resolves
//! that key to an on-time rate in `0.0..=1.0`. An itinerary's score is the
//! **mean** of its transit-leg on-time rates; walk/wait legs contribute nothing.
//! Itineraries with no resolvable reliability data get a neutral score and keep
//! their original position (the sort is stable).
//!
//! OTP namespaces its `gtfsId`s as `FEED:LOCALID` (e.g. `ATAC:MEA`), while the
//! reliability index is keyed by the bare local ids the worker recorded from
//! GTFS-RT. We strip the leading feed prefix off the route and stop ids before
//! the lookup so the two id spaces meet (ADR 0027).
//!
//! FAIL-SOFT: every helper here is total. A value that doesn't look like an OTP
//! plan, an itineraries field that isn't an array, or a leg that can't be keyed
//! all degrade to "no change" rather than erroring — the caller returns the
//! original bytes untouched. See [`rerank_plan`].

use serde_json::Value;

/// Resolves a transit leg's `(route_id, direction_id, stop_id)` reliability key
/// to an on-time rate in `0.0..=1.0`, or `None` when there is no history. The
/// handler builds this over the on-disk Tier-2 archive; tests pass a synthetic
/// closure. `direction_id` is the OTP `directionId` (commonly `0`/`1`); a leg
/// without one is keyed with `0` by the extractor.
pub type ReliabilityLookup<'a> = dyn Fn(&str, i32, &str) -> Option<f64> + 'a;

/// The neutral score for an itinerary with no resolvable reliability data. It
/// sits between "all on-time" (1.0) and "never on-time" (0.0) so scored
/// itineraries with real history sort around it, and ties hold original order.
const NEUTRAL_SCORE: f64 = 0.5;

/// Reorder `plan.data.plan.itineraries` by descending reliability score, stably.
/// Returns `true` when the value was a well-formed plan and the array was
/// (re)scored — even if the order didn't change — and `false` when the value
/// didn't look like an OTP plan, in which case `plan` is left untouched and the
/// caller should return the original response verbatim.
///
/// The reorder is **stable**: equal scores (including the neutral score for
/// no-history itineraries) preserve OTP's original ordering, so the default
/// engine ranking still breaks ties.
pub fn rerank_plan(plan: &mut Value, lookup: &ReliabilityLookup<'_>) -> bool {
    let Some(itineraries) = plan
        .get_mut("data")
        .and_then(|d| d.get_mut("plan"))
        .and_then(|p| p.get_mut("itineraries"))
        .and_then(Value::as_array_mut)
    else {
        return false;
    };

    // Score each itinerary in place, then sort by score descending. We pair the
    // score with the original index so the sort can fall back to it, making the
    // reorder a stable sort even though `sort_by` itself is already stable.
    let mut scored: Vec<(usize, f64, Value)> = itineraries
        .drain(..)
        .enumerate()
        .map(|(i, mut it)| {
            let score = score_itinerary(&it, lookup);
            // Additive field; existing clients that grep known keys ignore it.
            if let Some(obj) = it.as_object_mut() {
                obj.insert(
                    "reliabilityScore".to_string(),
                    serde_json::json!(round2(score)),
                );
            }
            (i, score, it)
        })
        .collect();

    // Descending score, original index as the stable tie-breaker.
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });

    *itineraries = scored.into_iter().map(|(_, _, it)| it).collect();
    true
}

/// The reliability score for one itinerary: the mean on-time rate across its
/// transit legs that have history. Walk/wait legs and unkeyable transit legs are
/// skipped. An itinerary with no contributing leg scores [`NEUTRAL_SCORE`].
fn score_itinerary(itinerary: &Value, lookup: &ReliabilityLookup<'_>) -> f64 {
    let Some(legs) = itinerary.get("legs").and_then(Value::as_array) else {
        return NEUTRAL_SCORE;
    };
    let mut sum = 0.0;
    let mut n = 0u32;
    for leg in legs {
        if let Some(rate) = leg_on_time_rate(leg, lookup) {
            sum += rate;
            n += 1;
        }
    }
    if n == 0 {
        NEUTRAL_SCORE
    } else {
        sum / f64::from(n)
    }
}

/// Resolve a single leg to its on-time rate, or `None` when the leg is non-transit
/// or can't be keyed/resolved. A leg is transit when it carries a `route.gtfsId`;
/// the direction comes from `trip.directionId` (default `0`) and the stop from
/// the boarding `from.stop.gtfsId`.
fn leg_on_time_rate(leg: &Value, lookup: &ReliabilityLookup<'_>) -> Option<f64> {
    // Walk/wait legs have no route — skip them (they contribute nothing).
    let route_id = leg.get("route")?.get("gtfsId")?.as_str()?;
    // A transitLeg flag, when present and false, also marks a non-transit leg.
    if leg.get("transitLeg").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    let direction_id = leg
        .get("trip")
        .and_then(|t| t.get("directionId"))
        .and_then(Value::as_i64)
        .and_then(|d| i32::try_from(d).ok())
        .unwrap_or(0);
    let stop_id = leg
        .get("from")
        .and_then(|f| f.get("stop"))
        .and_then(|s| s.get("gtfsId"))
        .and_then(Value::as_str)?;
    lookup(local_id(route_id), direction_id, local_id(stop_id))
}

/// Strip OTP's `FEED:` namespace prefix off a `gtfsId`, leaving the bare local id
/// the worker keyed the reliability index by (ADR 0027). OTP ids are `FEED:LOCAL`
/// (e.g. `ATAC:MEA`); an id with no colon is already local and passes through.
fn local_id(gtfs_id: &str) -> &str {
    gtfs_id
        .split_once(':')
        .map_or(gtfs_id, |(_feed, local)| local)
}

/// Round to two decimals so the additive `reliabilityScore` is stable and small
/// on the wire (the underlying rate already has no more meaningful precision).
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A synthetic plan with `n` itineraries, each carrying the given legs.
    fn plan_with(itineraries: Vec<Value>) -> Value {
        json!({ "data": { "plan": { "itineraries": itineraries } } })
    }

    /// A transit leg keyed by (route, direction, stop).
    fn transit_leg(route: &str, direction: i64, stop: &str) -> Value {
        json!({
            "transitLeg": true,
            "route": { "gtfsId": route },
            "trip": { "directionId": direction },
            "from": { "stop": { "gtfsId": stop } },
        })
    }

    fn walk_leg() -> Value {
        json!({ "transitLeg": false, "mode": "WALK" })
    }

    /// Order the itineraries array by reading back the first leg's route, so
    /// tests can assert the new sequence without depending on score values.
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

    #[test]
    fn reorders_best_on_time_first() {
        // Three single-transit-leg itineraries with distinct on-time rates.
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
        assert!(rerank_plan(&mut plan, &lookup));
        assert_eq!(route_order(&plan), vec!["B", "C", "A"]);
    }

    #[test]
    fn rerank_is_a_stable_sort_on_ties() {
        // All four share the same on-time rate → score ties → original order.
        let mut plan = plan_with(vec![
            json!({ "legs": [transit_leg("A", 0, "s")] }),
            json!({ "legs": [transit_leg("B", 0, "s")] }),
            json!({ "legs": [transit_leg("C", 0, "s")] }),
            json!({ "legs": [transit_leg("D", 0, "s")] }),
        ]);
        let lookup = |_r: &str, _d: i32, _s: &str| Some(0.80);
        assert!(rerank_plan(&mut plan, &lookup));
        assert_eq!(route_order(&plan), vec!["A", "B", "C", "D"]);
    }

    #[test]
    fn no_history_itineraries_keep_neutral_and_stable_position() {
        // A (0.9) beats the two no-history itineraries, which hold their order
        // around the neutral 0.5 and below A.
        let mut plan = plan_with(vec![
            json!({ "legs": [transit_leg("X", 0, "s1")] }), // no history
            json!({ "legs": [transit_leg("A", 0, "s2")] }), // 0.90
            json!({ "legs": [transit_leg("Y", 0, "s3")] }), // no history
        ]);
        let lookup = |route: &str, _d: i32, _s: &str| (route == "A").then_some(0.90);
        assert!(rerank_plan(&mut plan, &lookup));
        // A first (0.9 > 0.5); X and Y keep their relative order (both neutral).
        assert_eq!(route_order(&plan), vec!["A", "X", "Y"]);
    }

    #[test]
    fn walk_and_wait_legs_contribute_nothing() {
        // Itinerary 1: walk + good transit. Itinerary 2: walk + bad transit.
        // Only the transit leg drives the score, so 1 sorts ahead of 2.
        let mut plan = plan_with(vec![
            json!({ "legs": [walk_leg(), transit_leg("GOOD", 0, "s1")] }),
            json!({ "legs": [walk_leg(), transit_leg("BAD", 0, "s2")] }),
        ]);
        let lookup = |route: &str, _d: i32, _s: &str| match route {
            "GOOD" => Some(0.99),
            "BAD" => Some(0.10),
            _ => None,
        };
        assert!(rerank_plan(&mut plan, &lookup));
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
        assert_eq!(routes, vec!["GOOD", "BAD"]);
    }

    #[test]
    fn multi_leg_score_is_the_mean_of_transit_legs() {
        // Itinerary P: legs 0.4 and 0.6 → mean 0.5. Itinerary Q: single 0.55.
        // Q (0.55) edges out P (0.50).
        let mut plan = plan_with(vec![
            json!({ "legs": [transit_leg("P1", 0, "s"), transit_leg("P2", 0, "s")] }),
            json!({ "legs": [transit_leg("Q", 0, "s")] }),
        ]);
        let lookup = |route: &str, _d: i32, _s: &str| match route {
            "P1" => Some(0.40),
            "P2" => Some(0.60),
            "Q" => Some(0.55),
            _ => None,
        };
        assert!(rerank_plan(&mut plan, &lookup));
        assert_eq!(route_order(&plan), vec!["Q", "P1"]);
    }

    #[test]
    fn attaches_additive_reliability_score() {
        let mut plan = plan_with(vec![json!({ "legs": [transit_leg("A", 0, "s")] })]);
        let lookup = |_r: &str, _d: i32, _s: &str| Some(0.75);
        assert!(rerank_plan(&mut plan, &lookup));
        let it = &plan["data"]["plan"]["itineraries"][0];
        assert_eq!(it["reliabilityScore"], json!(0.75));
        // The original leg data is untouched (schema preserved).
        assert_eq!(it["legs"][0]["route"]["gtfsId"], "A");
    }

    #[test]
    fn direction_id_is_part_of_the_key() {
        // Same route+stop, different direction → different on-time rate.
        let mut plan = plan_with(vec![
            json!({ "legs": [transit_leg("R", 1, "s")] }),
            json!({ "legs": [transit_leg("R", 0, "s")] }),
        ]);
        let lookup = |_r: &str, dir: i32, _s: &str| match dir {
            0 => Some(0.90),
            1 => Some(0.20),
            _ => None,
        };
        assert!(rerank_plan(&mut plan, &lookup));
        // Direction 0 (0.9) sorts ahead of direction 1 (0.2).
        let dirs: Vec<i64> = plan["data"]["plan"]["itineraries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|it| it["legs"][0]["trip"]["directionId"].as_i64().unwrap())
            .collect();
        assert_eq!(dirs, vec![0, 1]);
    }

    #[test]
    fn missing_direction_defaults_to_zero() {
        // A transit leg with no trip.directionId is keyed with direction 0.
        let leg = json!({
            "transitLeg": true,
            "route": { "gtfsId": "R" },
            "from": { "stop": { "gtfsId": "s" } },
        });
        let mut plan = plan_with(vec![json!({ "legs": [leg] })]);
        let seen = std::cell::Cell::new(None);
        let lookup = |_r: &str, dir: i32, _s: &str| {
            seen.set(Some(dir));
            Some(0.5)
        };
        assert!(rerank_plan(&mut plan, &lookup));
        assert_eq!(seen.get(), Some(0), "absent directionId keys as 0");
    }

    #[test]
    fn otp_feed_prefixed_ids_match_unprefixed_index_keys() {
        // OTP sends `FEED:LOCAL` gtfsIds; the index holds bare local ids. The
        // feed prefix must be stripped so the lookup hits and the plan reorders.
        let mut plan = plan_with(vec![
            json!({ "legs": [transit_leg("ATAC:SLOW", 0, "ATAC:70001")] }),
            json!({ "legs": [transit_leg("ATAC:FAST", 0, "ATAC:70001")] }),
        ]);
        // The closure only knows the unprefixed ids — exactly what the worker records.
        let lookup = |route: &str, _d: i32, stop: &str| match (route, stop) {
            ("FAST", "70001") => Some(0.95),
            ("SLOW", "70001") => Some(0.10),
            _ => None,
        };
        assert!(rerank_plan(&mut plan, &lookup));
        // FAST (0.95) reorders ahead of SLOW (0.10): the match actually fired.
        assert_eq!(route_order(&plan), vec!["ATAC:FAST", "ATAC:SLOW"]);
    }

    #[test]
    fn rerank_is_idempotent_on_a_second_pass() {
        // Feeding an already-reranked plan back through must not change the order
        // or duplicate/re-nest the additive reliabilityScore.
        let mut plan = plan_with(vec![
            json!({ "legs": [transit_leg("A", 0, "s")] }),
            json!({ "legs": [transit_leg("B", 0, "s")] }),
        ]);
        let lookup = |route: &str, _d: i32, _s: &str| match route {
            "A" => Some(0.30),
            "B" => Some(0.80),
            _ => None,
        };
        assert!(rerank_plan(&mut plan, &lookup));
        let order1 = route_order(&plan);
        let scores1: Vec<Value> = plan["data"]["plan"]["itineraries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|it| it["reliabilityScore"].clone())
            .collect();
        assert!(rerank_plan(&mut plan, &lookup));
        assert_eq!(route_order(&plan), order1, "order is a fixed point");
        let scores2: Vec<Value> = plan["data"]["plan"]["itineraries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|it| it["reliabilityScore"].clone())
            .collect();
        assert_eq!(scores2, scores1, "scores unchanged on the second pass");
    }

    #[test]
    fn transit_leg_without_boarding_stop_scores_neutral() {
        // A well-formed transit leg (route + direction) but no from.stop can't be
        // keyed: it must be skipped, leaving the itinerary at the neutral score
        // (held against a no-history sibling), never a panic.
        let no_stop = json!({
            "transitLeg": true,
            "route": { "gtfsId": "R" },
            "trip": { "directionId": 0 },
        });
        let mut plan = plan_with(vec![
            json!({ "legs": [no_stop] }),
            json!({ "legs": [transit_leg("OTHER", 0, "s")] }), // also no history
        ]);
        let lookup = |_r: &str, _d: i32, _s: &str| None;
        assert!(rerank_plan(&mut plan, &lookup));
        // Both neutral → original order held; the stop-less leg scored as a skip.
        assert_eq!(
            plan["data"]["plan"]["itineraries"][0]["reliabilityScore"],
            json!(0.5)
        );
        assert_eq!(route_order(&plan), vec!["R", "OTHER"]);
    }

    #[test]
    fn non_plan_value_is_left_untouched() {
        // Anything that isn't `data.plan.itineraries[]` returns false + no change.
        let lookup = |_r: &str, _d: i32, _s: &str| Some(0.5);

        let mut not_a_plan = json!({ "errors": [{ "message": "boom" }] });
        let before = not_a_plan.clone();
        assert!(!rerank_plan(&mut not_a_plan, &lookup));
        assert_eq!(not_a_plan, before);

        let mut wrong_shape = json!({ "data": { "plan": { "itineraries": "nope" } } });
        let before2 = wrong_shape.clone();
        assert!(!rerank_plan(&mut wrong_shape, &lookup));
        assert_eq!(wrong_shape, before2);

        let mut bare = json!(42);
        assert!(!rerank_plan(&mut bare, &lookup));
        assert_eq!(bare, json!(42));
    }

    #[test]
    fn empty_itineraries_is_a_no_op_success() {
        let mut plan = plan_with(vec![]);
        let lookup = |_r: &str, _d: i32, _s: &str| Some(0.5);
        assert!(rerank_plan(&mut plan, &lookup));
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
        // Legs with missing/!object shapes must be skipped, not panic. Two such
        // itineraries tie at neutral and keep order.
        let mut plan = plan_with(vec![
            json!({ "legs": [json!(7), json!({ "route": "not-an-object" })] }),
            json!({ "legs": "not-an-array" }),
            json!({ "no": "legs" }),
        ]);
        let lookup = |_r: &str, _d: i32, _s: &str| Some(0.9);
        // Must not panic; all three score neutral and keep their order.
        assert!(rerank_plan(&mut plan, &lookup));
        assert_eq!(
            plan["data"]["plan"]["itineraries"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
    }
}
