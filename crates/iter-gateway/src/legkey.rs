//! Shared leg-keying for the OTP plan post-processors (the reranker, ADR 0026/
//! 0028, and the no-RT delay annotator, ADR 0030). Both read the same transit
//! legs out of a buffered plan and key them by
//! `(route gtfsId, trip directionId, boarding-stop gtfsId)` against the
//! unprefixed Tier-2 reliability index, so the keying lives here once rather than
//! being re-derived per consumer.
//!
//! Every helper is total and panic-free: a value that isn't a well-formed leg
//! degrades to "not transit" / "not keyable" rather than erroring.

use serde_json::Value;

/// The reliability key for one transit leg: the local (feed-prefix-stripped)
/// route id, the trip direction, and the local boarding-stop id. These match the
/// bare tokens the worker recorded from GTFS-RT (ADR 0027).
pub struct LegKey<'a> {
    pub route: &'a str,
    pub direction: i32,
    pub stop: &'a str,
}

/// Whether a leg is a transit leg. A leg is transit when it carries a
/// `route.gtfsId`, unless an explicit `transitLeg: false` marks it otherwise.
pub fn is_transit_leg(leg: &Value) -> bool {
    if leg.get("transitLeg").and_then(Value::as_bool) == Some(false) {
        return false;
    }
    leg.get("route")
        .and_then(|r| r.get("gtfsId"))
        .and_then(Value::as_str)
        .is_some()
}

/// Extract a transit leg's reliability key, or `None` when the leg is non-transit
/// or can't be keyed (no route gtfsId, no boarding-stop gtfsId). The direction
/// comes from `trip.directionId` (default `0`) and the stop from the boarding
/// `from.stop.gtfsId`. Route and stop ids are stripped of their OTP feed prefix
/// so they meet the unprefixed index key space.
pub fn leg_key(leg: &Value) -> Option<LegKey<'_>> {
    if leg.get("transitLeg").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    let route = leg.get("route")?.get("gtfsId")?.as_str()?;
    let direction = leg
        .get("trip")
        .and_then(|t| t.get("directionId"))
        .and_then(Value::as_i64)
        .and_then(|d| i32::try_from(d).ok())
        .unwrap_or(0);
    let stop = leg
        .get("from")
        .and_then(|f| f.get("stop"))
        .and_then(|s| s.get("gtfsId"))
        .and_then(Value::as_str)?;
    Some(LegKey {
        route: local_id(route),
        direction,
        stop: local_id(stop),
    })
}

/// Strip OTP's `FEED:` namespace prefix off a `gtfsId`, leaving the bare local id
/// the worker keyed the reliability index by (ADR 0027). OTP ids are `FEED:LOCAL`
/// (e.g. `ATAC:MEA`); an id with no colon is already local and passes through.
pub fn local_id(gtfs_id: &str) -> &str {
    gtfs_id
        .split_once(':')
        .map_or(gtfs_id, |(_feed, local)| local)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn local_id_strips_only_the_feed_prefix() {
        assert_eq!(local_id("ATAC:MEA"), "MEA");
        assert_eq!(local_id("MEA"), "MEA");
        // Only the first colon is the feed separator.
        assert_eq!(local_id("FEED:a:b"), "a:b");
    }

    #[test]
    fn transit_leg_is_keyed_with_local_ids_and_direction() {
        let leg = json!({
            "transitLeg": true,
            "route": { "gtfsId": "ATAC:SLOW" },
            "trip": { "directionId": 1 },
            "from": { "stop": { "gtfsId": "ATAC:70001" } },
        });
        let k = leg_key(&leg).expect("keyable");
        assert_eq!(k.route, "SLOW");
        assert_eq!(k.direction, 1);
        assert_eq!(k.stop, "70001");
    }

    #[test]
    fn direction_defaults_to_zero_when_absent() {
        let leg = json!({
            "route": { "gtfsId": "R" },
            "from": { "stop": { "gtfsId": "s" } },
        });
        assert_eq!(leg_key(&leg).unwrap().direction, 0);
    }

    #[test]
    fn non_transit_and_unkeyable_legs_return_none() {
        // Explicit non-transit.
        assert!(leg_key(&json!({ "transitLeg": false, "mode": "WALK" })).is_none());
        // No route gtfsId.
        assert!(leg_key(&json!({ "from": { "stop": { "gtfsId": "s" } } })).is_none());
        // No boarding stop.
        assert!(leg_key(&json!({ "route": { "gtfsId": "R" } })).is_none());
        // Malformed leg never panics.
        assert!(leg_key(&json!(7)).is_none());
    }

    #[test]
    fn is_transit_leg_reads_the_route_and_the_flag() {
        assert!(is_transit_leg(&json!({ "route": { "gtfsId": "R" } })));
        assert!(!is_transit_leg(
            &json!({ "transitLeg": false, "route": { "gtfsId": "R" } })
        ));
        assert!(!is_transit_leg(&json!({ "mode": "WALK" })));
    }
}
