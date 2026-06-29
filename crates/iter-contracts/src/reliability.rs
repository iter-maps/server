//! Reliability read contract (`GET /reliability/{route}/{direction}/{stop}`).
//! The gateway serves the worker-written Tier-2 archive as this DTO: a per
//! (tod_bucket, day_type) view of the delay distribution — p50/p85/p90 delay
//! seconds, on-time rate, and the sample count the figures rest on.
//!
//! Field names are camelCase and load-bearing: the Android client greps them.

use iter_core::reliability::rollup::Readout;
use serde::Serialize;

/// One reliability cell: the [`Readout`] for a single (tod_bucket, day_type)
/// slice of a stop, plus the tokens that identify the slice. `sampleCount` is
/// how many observations the percentiles rest on — the client uses it to gate
/// low-confidence cells.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReliabilityCell {
    /// Time-of-day bucket token (`early`, `am-peak`, `midday`, `pm-peak`,
    /// `evening`, `night`).
    pub tod_bucket: String,
    /// Day-type token (`weekday`, `saturday`, `sunday-holiday`).
    pub day_type: String,
    /// Number of observations behind the percentiles.
    pub sample_count: u64,
    /// On-time rate over the [-60s, +300s] window, `0.0..=1.0`. Absent when the
    /// cell has no observations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_time_rate: Option<f64>,
    /// Median delay, seconds. Negative is early.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p50_s: Option<f64>,
    /// 85th-percentile delay, seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p85_s: Option<f64>,
    /// 90th-percentile delay, seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p90_s: Option<f64>,
    /// Mean delay, seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean_s: Option<f64>,
}

impl ReliabilityCell {
    /// Build a cell from a stored [`Readout`] and the slice tokens it belongs to.
    pub fn from_readout(tod_bucket: &str, day_type: &str, r: &Readout) -> Self {
        Self {
            tod_bucket: tod_bucket.to_string(),
            day_type: day_type.to_string(),
            sample_count: r.count,
            on_time_rate: r.on_time_rate,
            p50_s: r.p50_s,
            p85_s: r.p85_s,
            p90_s: r.p90_s,
            mean_s: r.mean_s,
        }
    }
}

/// The reliability response for a (route, direction, stop). Echoes the query
/// tuple and carries the matching cells. Fail-soft: an absent key or a missing/
/// corrupt store yields an empty `cells`, never an error — the client treats an
/// empty list as "no history yet" and falls back to schedule-only ranking.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReliabilityResponse {
    pub route: String,
    /// The direction token exactly as the caller supplied it. Echoed verbatim
    /// rather than the parsed integer so an unparsable direction comes back as
    /// the original token instead of a normalization sentinel.
    pub direction: String,
    pub stop: String,
    pub cells: Vec<ReliabilityCell>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    fn readout() -> Readout {
        Readout {
            count: 12,
            p50_s: Some(45.0),
            p85_s: Some(180.0),
            p90_s: Some(240.0),
            on_time_rate: Some(0.75),
            mean_s: Some(60.0),
        }
    }

    #[test]
    fn cell_serializes_camel_case() {
        let cell = ReliabilityCell::from_readout("am-peak", "weekday", &readout());
        let v = to_value(&cell).unwrap();
        assert_eq!(
            v,
            json!({
                "todBucket": "am-peak",
                "dayType": "weekday",
                "sampleCount": 12,
                "onTimeRate": 0.75,
                "p50S": 45.0,
                "p85S": 180.0,
                "p90S": 240.0,
                "meanS": 60.0,
            })
        );
    }

    #[test]
    fn empty_readout_omits_optional_fields() {
        let empty = Readout {
            count: 0,
            p50_s: None,
            p85_s: None,
            p90_s: None,
            on_time_rate: None,
            mean_s: None,
        };
        let cell = ReliabilityCell::from_readout("night", "saturday", &empty);
        let v = to_value(&cell).unwrap();
        // Only the three always-present keys remain.
        assert_eq!(
            v,
            json!({ "todBucket": "night", "dayType": "saturday", "sampleCount": 0 })
        );
    }

    #[test]
    fn response_serializes_camel_case() {
        let resp = ReliabilityResponse {
            route: "MEA".to_string(),
            direction: "0".to_string(),
            stop: "70001".to_string(),
            cells: vec![ReliabilityCell::from_readout(
                "am-peak",
                "weekday",
                &readout(),
            )],
        };
        let v = to_value(&resp).unwrap();
        assert_eq!(v["route"], "MEA");
        assert_eq!(v["direction"], "0");
        assert_eq!(v["stop"], "70001");
        assert_eq!(v["cells"][0]["todBucket"], "am-peak");
        assert_eq!(v["cells"][0]["sampleCount"], 12);
    }

    #[test]
    fn absent_history_is_an_empty_cell_list() {
        let resp = ReliabilityResponse {
            route: "MEA".to_string(),
            direction: "1".to_string(),
            stop: "missing".to_string(),
            cells: Vec::new(),
        };
        let v = to_value(&resp).unwrap();
        assert_eq!(v["cells"], json!([]));
    }
}
