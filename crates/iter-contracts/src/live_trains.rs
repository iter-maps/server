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
