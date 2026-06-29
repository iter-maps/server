//! No-live-RT historical delay prediction over an OTP plan response (ADR 0030).
//!
//! This module is the **pure, I/O-free core**: it takes an already-parsed OTP
//! plan value plus a typical-delay lookup closure and annotates each transit leg
//! that carries **no live realtime delay** with a *historical* expected delay
//! drawn from the Tier-2 archive. It is the dual of the reranker — where the
//! reranker reorders itineraries, the annotator leaves order, times, modes, and
//! feasibility entirely untouched and only *adds* an optional `predictedDelay`
//! object per leg (and an optional per-itinerary summary). Existing clients that
//! grep known keys ignore the additive fields.
//!
//! AUTHORITATIVE IS THE FLOOR. A leg whose OTP `realTime` is `true` already
//! carries a live delay (`arrivalDelay` / `departureDelay`); history never
//! overrides or contradicts live data, so such a leg is **never** annotated. The
//! prediction only fills gaps where there is no live feed.
//!
//! WHICH PERCENTILE. The surfaced `seconds` is the **p85** delay, not the median.
//! A traveler planning around a missing live feed is better served by a
//! conservative "budget at least this much" tail than by the median, which is
//! beaten half the time; p85 is the same conservative percentile the read
//! endpoint already exposes (ADR 0024). The median is carried alongside as
//! `p50Seconds` for clients that want the typical case, and `sampleCount` lets a
//! client gate out low-confidence cells.
//!
//! Legs are keyed by `(route gtfsId, trip directionId, boarding-stop gtfsId)` via
//! the shared [`crate::legkey`] helpers — the same keying and feed-prefix strip
//! the reranker uses (ADR 0027) — so the OTP `FEED:LOCAL` ids meet the unprefixed
//! index.
//!
//! FAIL-SOFT: every helper is total. A value that isn't a plan, an itineraries
//! field that isn't an array, a leg that already has live RT, a leg that can't be
//! keyed, or a key with no history all degrade to "no annotation" rather than
//! erroring — the caller returns the original bytes untouched. See
//! [`annotate_plan`].

use serde_json::{Value, json};

use crate::legkey::leg_key;

/// A leg's historical typical delay, resolved from the Tier-2 archive. The
/// handler builds the lookup over the on-disk index; tests pass a synthetic
/// closure. Mirrors `iter_core::reliability::store_read::TypicalDelay` but is
/// declared here so the pure core carries no read-side dependency.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TypicalDelay {
    /// Median delay across the stop's history, seconds. Negative is early.
    pub p50_s: f64,
    /// 85th-percentile (conservative) delay, seconds — the surfaced estimate.
    pub p85_s: f64,
    /// Observations behind the figures, for client-side confidence gating.
    pub count: u64,
}

/// Resolves a transit leg's `(route, direction, stop)` key to its historical
/// [`TypicalDelay`], or `None` when there is no history.
pub type DelayLookup<'a> = dyn Fn(&str, i32, &str) -> Option<TypicalDelay> + 'a;

/// The source tag stamped on every historical annotation, distinguishing it from
/// a live realtime delay so a client never mistakes the estimate for measured
/// data.
const SOURCE_HISTORICAL: &str = "historical";

/// Annotate every RT-less transit leg in `plan.data.plan.itineraries` with its
/// historical typical delay. Returns `true` when the value was a well-formed plan
/// (whether or not any leg gained an annotation) and `false` when it didn't look
/// like an OTP plan — in which case `plan` is untouched and the caller returns the
/// original response verbatim.
///
/// Additive and non-destructive: leg times, modes, order, and the itinerary order
/// are never changed. A leg already carrying live realtime data is skipped (live
/// wins). A leg that can't be keyed or has no history is left as-is. When at least
/// one leg in an itinerary is annotated, the itinerary also gains a compact
/// `predictedDelaySummary` (annotated-leg count + worst surfaced delay).
pub fn annotate_plan(plan: &mut Value, lookup: &DelayLookup<'_>) -> bool {
    let Some(itineraries) = plan
        .get_mut("data")
        .and_then(|d| d.get_mut("plan"))
        .and_then(|p| p.get_mut("itineraries"))
        .and_then(Value::as_array_mut)
    else {
        return false;
    };

    for itinerary in itineraries.iter_mut() {
        annotate_itinerary(itinerary, lookup);
    }
    true
}

/// Annotate one itinerary's legs in place and, if any leg was annotated, attach a
/// per-itinerary summary. Total: a malformed itinerary (no `legs` array) is left
/// untouched.
fn annotate_itinerary(itinerary: &mut Value, lookup: &DelayLookup<'_>) {
    let Some(legs) = itinerary.get_mut("legs").and_then(Value::as_array_mut) else {
        return;
    };

    let mut annotated = 0u32;
    let mut worst_s = f64::NEG_INFINITY;

    for leg in legs.iter_mut() {
        if let Some(seconds) = annotate_leg(leg, lookup) {
            annotated += 1;
            worst_s = worst_s.max(seconds);
        }
    }

    if annotated > 0 {
        if let Some(obj) = itinerary.as_object_mut() {
            obj.insert(
                "predictedDelaySummary".to_string(),
                json!({
                    "annotatedLegs": annotated,
                    "worstSeconds": round1(worst_s),
                    "source": SOURCE_HISTORICAL,
                }),
            );
        }
    }
}

/// Annotate a single leg if it is an RT-less transit leg with history, returning
/// the surfaced (p85) delay in seconds on success so the caller can roll up an
/// itinerary summary. Returns `None` — and leaves the leg untouched — when the leg
/// already has live RT (live wins), can't be keyed, or has no history.
fn annotate_leg(leg: &mut Value, lookup: &DelayLookup<'_>) -> Option<f64> {
    // AUTHORITATIVE FLOOR: a leg already carrying a live realtime delay is never
    // annotated. OTP marks live legs with `realTime: true`; we only fill gaps.
    if has_live_realtime(leg) {
        return None;
    }
    // Read-only key + lookup first; the borrow ends before we mutate.
    let td = {
        let key = leg_key(leg)?;
        lookup(key.route, key.direction, key.stop)?
    };

    let seconds = round1(td.p85_s);
    let obj = leg.as_object_mut()?;
    obj.insert(
        "predictedDelay".to_string(),
        json!({
            "seconds": seconds,
            "p50Seconds": round1(td.p50_s),
            "sampleCount": td.count,
            "source": SOURCE_HISTORICAL,
        }),
    );
    Some(seconds)
}

/// Whether a leg carries a live realtime delay that the prediction must not
/// override. OTP exposes a boolean `realTime` per leg; `true` means the leg's
/// times reflect a live feed. We treat only an explicit `realTime: true` as live —
/// a missing or `false` flag is the no-live-feed case the annotator exists for.
fn has_live_realtime(leg: &Value) -> bool {
    leg.get("realTime").and_then(Value::as_bool) == Some(true)
}

/// Round to one decimal — delays are whole seconds in practice, but the percentile
/// estimate is a histogram-interpolated float, so one decimal keeps the wire value
/// small and stable.
fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No history at all — every leg misses.
    fn no_history(_r: &str, _d: i32, _s: &str) -> Option<TypicalDelay> {
        None
    }

    fn plan_with(itineraries: Vec<Value>) -> Value {
        json!({ "data": { "plan": { "itineraries": itineraries } } })
    }

    /// A transit leg with no live RT (the annotatable case).
    fn rtless_transit(route: &str, direction: i64, stop: &str) -> Value {
        json!({
            "transitLeg": true,
            "mode": "BUS",
            "realTime": false,
            "route": { "gtfsId": route },
            "trip": { "directionId": direction },
            "from": { "stop": { "gtfsId": stop } },
        })
    }

    fn td(p50: f64, p85: f64, count: u64) -> TypicalDelay {
        TypicalDelay {
            p50_s: p50,
            p85_s: p85,
            count,
        }
    }

    // --- the core behaviour ---------------------------------------------------

    #[test]
    fn rtless_leg_with_history_gets_the_p85_annotation() {
        let mut plan = plan_with(vec![json!({ "legs": [rtless_transit("R", 0, "s")] })]);
        let lookup = |_r: &str, _d: i32, _s: &str| Some(td(45.0, 180.0, 12));
        assert!(annotate_plan(&mut plan, &lookup));
        let leg = &plan["data"]["plan"]["itineraries"][0]["legs"][0];
        let pd = &leg["predictedDelay"];
        // The surfaced `seconds` is the conservative p85, not the median.
        assert_eq!(pd["seconds"], json!(180.0));
        assert_eq!(pd["p50Seconds"], json!(45.0));
        assert_eq!(pd["sampleCount"], json!(12));
        assert_eq!(pd["source"], json!("historical"));
    }

    #[test]
    fn itinerary_gains_a_summary_when_a_leg_is_annotated() {
        let mut plan = plan_with(vec![json!({ "legs": [
            rtless_transit("A", 0, "s1"),
            rtless_transit("B", 0, "s2"),
        ]})]);
        let lookup = |route: &str, _d: i32, _s: &str| match route {
            "A" => Some(td(30.0, 120.0, 5)),
            "B" => Some(td(60.0, 300.0, 9)),
            _ => None,
        };
        assert!(annotate_plan(&mut plan, &lookup));
        let summary = &plan["data"]["plan"]["itineraries"][0]["predictedDelaySummary"];
        assert_eq!(summary["annotatedLegs"], json!(2));
        // Worst is the larger p85.
        assert_eq!(summary["worstSeconds"], json!(300.0));
        assert_eq!(summary["source"], json!("historical"));
    }

    // --- the authoritative floor: live wins ----------------------------------

    #[test]
    fn leg_with_live_realtime_is_never_annotated() {
        let live = json!({
            "transitLeg": true, "mode": "BUS", "realTime": true,
            "arrivalDelay": 240, "departureDelay": 200,
            "route": { "gtfsId": "R" }, "trip": { "directionId": 0 },
            "from": { "stop": { "gtfsId": "s" } },
        });
        let mut plan = plan_with(vec![json!({ "legs": [live] })]);
        // History exists for this key, but live data must win → no annotation.
        let lookup = |_r: &str, _d: i32, _s: &str| Some(td(45.0, 180.0, 12));
        assert!(annotate_plan(&mut plan, &lookup));
        let leg = &plan["data"]["plan"]["itineraries"][0]["legs"][0];
        assert!(leg.get("predictedDelay").is_none());
        // No summary either — nothing was annotated.
        assert!(
            plan["data"]["plan"]["itineraries"][0]
                .get("predictedDelaySummary")
                .is_none()
        );
        // Live fields are untouched.
        assert_eq!(leg["arrivalDelay"], json!(240));
    }

    #[test]
    fn realtime_true_blocks_even_with_history_but_false_allows() {
        // Same key, two itineraries: one live (realTime true), one RT-less.
        let mut plan = plan_with(vec![
            json!({ "legs": [json!({
                "transitLeg": true, "realTime": true,
                "route": { "gtfsId": "R" }, "trip": { "directionId": 0 },
                "from": { "stop": { "gtfsId": "s" } } })]}),
            json!({ "legs": [rtless_transit("R", 0, "s")] }),
        ]);
        let lookup = |_r: &str, _d: i32, _s: &str| Some(td(45.0, 180.0, 12));
        assert!(annotate_plan(&mut plan, &lookup));
        let its = plan["data"]["plan"]["itineraries"].as_array().unwrap();
        assert!(its[0]["legs"][0].get("predictedDelay").is_none());
        assert!(its[1]["legs"][0].get("predictedDelay").is_some());
    }

    // --- gaps and keying ------------------------------------------------------

    #[test]
    fn leg_with_no_history_gets_no_field() {
        let mut plan = plan_with(vec![json!({ "legs": [rtless_transit("R", 0, "s")] })]);
        assert!(annotate_plan(&mut plan, &no_history));
        let leg = &plan["data"]["plan"]["itineraries"][0]["legs"][0];
        assert!(leg.get("predictedDelay").is_none());
        assert!(
            plan["data"]["plan"]["itineraries"][0]
                .get("predictedDelaySummary")
                .is_none()
        );
    }

    #[test]
    fn feed_prefixed_ids_match_the_unprefixed_index() {
        // The regression that pins the ADR 0027 normalization for the annotator:
        // OTP sends `ATAC:R` / `ATAC:70001`; the index is keyed by the bare locals.
        let mut plan = plan_with(vec![
            json!({ "legs": [rtless_transit("ATAC:R", 0, "ATAC:70001")] }),
        ]);
        let lookup = |route: &str, _d: i32, stop: &str| match (route, stop) {
            ("R", "70001") => Some(td(20.0, 90.0, 7)),
            _ => None,
        };
        assert!(annotate_plan(&mut plan, &lookup));
        let pd = &plan["data"]["plan"]["itineraries"][0]["legs"][0]["predictedDelay"];
        assert_eq!(pd["seconds"], json!(90.0));
        assert_eq!(pd["sampleCount"], json!(7));
    }

    #[test]
    fn direction_id_is_part_of_the_lookup_key() {
        let mut plan = plan_with(vec![json!({ "legs": [rtless_transit("R", 1, "s")] })]);
        let lookup = |_r: &str, dir: i32, _s: &str| match dir {
            1 => Some(td(10.0, 50.0, 3)),
            _ => None,
        };
        assert!(annotate_plan(&mut plan, &lookup));
        let pd = &plan["data"]["plan"]["itineraries"][0]["legs"][0]["predictedDelay"];
        assert_eq!(pd["seconds"], json!(50.0));
    }

    #[test]
    fn non_transit_legs_are_left_alone() {
        let mut plan = plan_with(vec![json!({ "legs": [
            json!({ "transitLeg": false, "mode": "WALK", "duration": 300.0 }),
            rtless_transit("R", 0, "s"),
        ]})]);
        let lookup = |_r: &str, _d: i32, _s: &str| Some(td(45.0, 180.0, 12));
        assert!(annotate_plan(&mut plan, &lookup));
        let legs = plan["data"]["plan"]["itineraries"][0]["legs"]
            .as_array()
            .unwrap();
        assert!(legs[0].get("predictedDelay").is_none()); // the walk leg
        assert!(legs[1].get("predictedDelay").is_some()); // the transit leg
    }

    // --- fail-soft ------------------------------------------------------------

    #[test]
    fn non_plan_value_returns_false_and_is_untouched() {
        let mut not_a_plan = json!({ "errors": [{ "message": "boom" }] });
        let before = not_a_plan.clone();
        assert!(!annotate_plan(&mut not_a_plan, &no_history));
        assert_eq!(not_a_plan, before);

        let mut wrong_shape = json!({ "data": { "plan": { "itineraries": "nope" } } });
        let before2 = wrong_shape.clone();
        assert!(!annotate_plan(&mut wrong_shape, &no_history));
        assert_eq!(wrong_shape, before2);

        let mut bare = json!(42);
        assert!(!annotate_plan(&mut bare, &no_history));
        assert_eq!(bare, json!(42));
    }

    #[test]
    fn malformed_legs_never_panic_and_gain_nothing() {
        let mut plan = plan_with(vec![
            json!({ "legs": [json!(7), json!({ "route": "not-an-object" })] }),
            json!({ "legs": "not-an-array" }),
            json!({ "no": "legs" }),
        ]);
        let lookup = |_r: &str, _d: i32, _s: &str| Some(td(45.0, 180.0, 12));
        // Must not panic; all itineraries kept, none gains an annotation.
        assert!(annotate_plan(&mut plan, &lookup));
        let its = plan["data"]["plan"]["itineraries"].as_array().unwrap();
        assert_eq!(its.len(), 3);
        for it in its {
            assert!(it.get("predictedDelaySummary").is_none());
        }
    }

    #[test]
    fn empty_itineraries_is_a_no_op_success() {
        let mut plan = plan_with(vec![]);
        assert!(annotate_plan(&mut plan, &no_history));
        assert_eq!(
            plan["data"]["plan"]["itineraries"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn annotation_does_not_reorder_or_alter_other_legs() {
        // Two itineraries; only the second's leg has history. Order and the
        // first itinerary's leg data are preserved exactly.
        let mut plan = plan_with(vec![
            json!({ "legs": [rtless_transit("FIRST", 0, "s1")] }),
            json!({ "legs": [rtless_transit("SECOND", 0, "s2")] }),
        ]);
        let lookup = |route: &str, _d: i32, _s: &str| match route {
            "SECOND" => Some(td(45.0, 180.0, 12)),
            _ => None,
        };
        assert!(annotate_plan(&mut plan, &lookup));
        let its = plan["data"]["plan"]["itineraries"].as_array().unwrap();
        // Order unchanged.
        assert_eq!(its[0]["legs"][0]["route"]["gtfsId"], "FIRST");
        assert_eq!(its[1]["legs"][0]["route"]["gtfsId"], "SECOND");
        // FIRST got nothing; SECOND got the annotation.
        assert!(its[0]["legs"][0].get("predictedDelay").is_none());
        assert!(its[1]["legs"][0].get("predictedDelay").is_some());
    }

    #[test]
    fn sibling_leg_in_an_annotated_itinerary_stays_byte_identical() {
        // One itinerary, two legs sharing the same itinerary; only the second has
        // history. The first leg must be left exactly as it came in — annotating a
        // sibling never perturbs the untouched leg.
        let untouched = rtless_transit("NOHIST", 0, "s1");
        let before = untouched.clone();
        let mut plan = plan_with(vec![json!({ "legs": [
            untouched,
            rtless_transit("HIST", 0, "s2"),
        ]})]);
        let lookup = |route: &str, _d: i32, _s: &str| match route {
            "HIST" => Some(td(45.0, 180.0, 12)),
            _ => None,
        };
        assert!(annotate_plan(&mut plan, &lookup));
        let legs = plan["data"]["plan"]["itineraries"][0]["legs"]
            .as_array()
            .unwrap();
        // The unkeyed sibling is byte-identical to its input; the other was annotated.
        assert_eq!(legs[0], before);
        assert!(legs[1].get("predictedDelay").is_some());
    }

    #[test]
    fn worst_seconds_rolls_up_the_largest_even_with_a_negative_sibling() {
        // A two-leg itinerary mixing an early (negative p85) leg and a late one:
        // the summary's worst must be the larger positive tail, not max-of-abs.
        let mut plan = plan_with(vec![json!({ "legs": [
            rtless_transit("EARLY", 0, "s1"),
            rtless_transit("LATE", 0, "s2"),
        ]})]);
        let lookup = |route: &str, _d: i32, _s: &str| match route {
            "EARLY" => Some(td(-90.0, -40.0, 6)),
            "LATE" => Some(td(20.0, 75.0, 9)),
            _ => None,
        };
        assert!(annotate_plan(&mut plan, &lookup));
        let summary = &plan["data"]["plan"]["itineraries"][0]["predictedDelaySummary"];
        assert_eq!(summary["annotatedLegs"], json!(2));
        assert_eq!(summary["worstSeconds"], json!(75.0));
    }

    #[test]
    fn negative_early_delay_is_surfaced_verbatim() {
        // History can say a stop runs early; the annotation reports it as a
        // negative delay rather than clamping to zero.
        let mut plan = plan_with(vec![json!({ "legs": [rtless_transit("R", 0, "s")] })]);
        let lookup = |_r: &str, _d: i32, _s: &str| Some(td(-120.0, -30.0, 8));
        assert!(annotate_plan(&mut plan, &lookup));
        let pd = &plan["data"]["plan"]["itineraries"][0]["legs"][0]["predictedDelay"];
        assert_eq!(pd["seconds"], json!(-30.0));
        assert_eq!(pd["p50Seconds"], json!(-120.0));
    }
}
