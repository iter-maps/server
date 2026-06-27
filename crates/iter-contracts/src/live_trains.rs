//! Normalized live-train boards from ViaggiaTreno (`/trenitalia/*`). The
//! gateway maps the upstream's messy payloads onto these stable shapes; the
//! client never sees ViaggiaTreno directly.

use serde::Serialize;

/// One row of a departures/arrivals board.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardEntry {
    /// Category + number, e.g. "REG 22815".
    pub train_number: String,
    /// REG | RV | IC | FR | ...
    pub category: String,
    /// Origin (arrivals) — null when upstream omits it.
    pub origin: Option<String>,
    pub destination: String,
    /// Scheduled wall-clock "HH:MM".
    pub scheduled_time: String,
    /// Signed delay in minutes (negative = early).
    pub delay_minutes: i32,
    /// Track/platform — omitted when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

/// A station in the full list or autocomplete results. Station ids match
/// `^S\d+$` (ViaggiaTreno's `S` + number, e.g. Roma Termini = `S08409`); a
/// `type:"station"` geocoding result is the client's entry point here.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Station {
    pub id: String,
    pub name: String,
    /// Upstream's literal 0 means "no coordinate" → omitted, not 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json, to_value};

    #[test]
    fn board_entry_camel_case_with_platform() {
        let e = BoardEntry {
            train_number: "REG 22815".to_string(),
            category: "REG".to_string(),
            origin: Some("Roma Termini".to_string()),
            destination: "Napoli Centrale".to_string(),
            scheduled_time: "14:35".to_string(),
            delay_minutes: 5,
            platform: Some("3".to_string()),
        };
        let v = to_value(&e).unwrap();
        assert_eq!(
            v,
            json!({
                "trainNumber": "REG 22815",
                "category": "REG",
                "origin": "Roma Termini",
                "destination": "Napoli Centrale",
                "scheduledTime": "14:35",
                "delayMinutes": 5,
                "platform": "3",
            })
        );
    }

    #[test]
    fn board_entry_origin_none_is_null_present_key() {
        let e = BoardEntry {
            train_number: "RV 2".to_string(),
            category: "RV".to_string(),
            origin: None,
            destination: "Firenze".to_string(),
            scheduled_time: "09:00".to_string(),
            delay_minutes: 0,
            platform: None,
        };
        let v = to_value(&e).unwrap();
        assert_eq!(v["origin"], Value::Null);
        assert!(v.as_object().unwrap().contains_key("origin"));
    }

    #[test]
    fn board_entry_platform_none_is_omitted() {
        let e = BoardEntry {
            train_number: "IC 1".to_string(),
            category: "IC".to_string(),
            origin: None,
            destination: "Bari".to_string(),
            scheduled_time: "10:00".to_string(),
            delay_minutes: 0,
            platform: None,
        };
        let v = to_value(&e).unwrap();
        assert!(!v.as_object().unwrap().contains_key("platform"));
    }

    #[test]
    fn board_entry_negative_delay() {
        let e = BoardEntry {
            train_number: "FR 9".to_string(),
            category: "FR".to_string(),
            origin: None,
            destination: "Milano".to_string(),
            scheduled_time: "12:00".to_string(),
            delay_minutes: -3,
            platform: None,
        };
        let v = to_value(&e).unwrap();
        assert_eq!(v["delayMinutes"], -3);
    }

    #[test]
    fn station_coords_omitted_when_none() {
        let s = Station {
            id: "S08409".to_string(),
            name: "Roma Termini".to_string(),
            lat: None,
            lon: None,
        };
        let v = to_value(&s).unwrap();
        assert_eq!(
            v,
            json!({
                "id": "S08409",
                "name": "Roma Termini",
            })
        );
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("lat"));
        assert!(!obj.contains_key("lon"));
    }

    #[test]
    fn station_coords_present_when_some() {
        let s = Station {
            id: "S08409".to_string(),
            name: "Roma Termini".to_string(),
            lat: Some(41.9009),
            lon: Some(12.5021),
        };
        let v = to_value(&s).unwrap();
        assert_eq!(v["lat"], 41.9009);
        assert_eq!(v["lon"], 12.5021);
    }
}
