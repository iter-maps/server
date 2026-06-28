//! Italy live-trains driver: a client over RFI's unofficial ViaggiaTreno API,
//! behind the generic [`LiveTrainsProvider`] trait. Everything Italy/RFI-specific
//! lives here — the `cercaStazione`/`elencoStazioni`/`partenze`/`arrivi` endpoint
//! segments, the Italian JSON field names, the `S\d+` station-id format, the
//! `Date.toString()` CET/CEST date param, and the viaggiatreno.it base URL +
//! referer. Field names and the date-param were verified against the live API
//! (2026-06-28).

use anyhow::{Context, anyhow};
use iter_contracts::live_trains::{BoardEntry, Station};
use serde_json::Value;

use crate::traits::{BoardKind, LiveTrainsProvider};

/// RFI's unofficial ViaggiaTreno API (cleartext, flaky — the generic TTL cache +
/// single-flight in the core shield it). The base URL is overridable for tests;
/// the default is the public endpoint. The default region (Lazio = `5`) is used
/// for the station list when the caller supplies none.
pub struct ViaggiaTreno {
    base_url: String,
    default_region: i64,
}

/// ViaggiaTreno's public base and Lazio's region code — the Italy-specific
/// defaults that used to sit in the generic config.
const DEFAULT_BASE_URL: &str = "http://www.viaggiatreno.it/infomobilita/resteasy/viaggiatreno";
const DEFAULT_REGION: i64 = 5;

impl Default for ViaggiaTreno {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            default_region: DEFAULT_REGION,
        }
    }
}

impl ViaggiaTreno {
    /// Build a provider against a specific base URL (env override / tests),
    /// falling back to the public endpoint when `base_url` is empty, and to
    /// Lazio when `region_code` is `None`.
    pub fn new(base_url: Option<String>, region_code: Option<i64>) -> Self {
        Self {
            base_url: base_url
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            default_region: region_code.unwrap_or(DEFAULT_REGION),
        }
    }
}

#[async_trait::async_trait]
impl LiveTrainsProvider for ViaggiaTreno {
    async fn search(
        &self,
        http: &reqwest::Client,
        query: &str,
    ) -> Result<Vec<Station>, anyhow::Error> {
        let url = format!("{}/cercaStazione/{}", self.base_url, pct(query));
        let v = self.fetch_json(http, &url).await?;
        Ok(normalize_search(&v))
    }

    async fn list(
        &self,
        http: &reqwest::Client,
        region_code: Option<i64>,
    ) -> Result<Vec<Station>, anyhow::Error> {
        let region = region_code.unwrap_or(self.default_region);
        let url = format!("{}/elencoStazioni/{}", self.base_url, region);
        let v = self.fetch_json(http, &url).await?;
        Ok(normalize_list(&v))
    }

    async fn board(
        &self,
        http: &reqwest::Client,
        station: &str,
        kind: BoardKind,
    ) -> Result<Vec<BoardEntry>, anyhow::Error> {
        if !is_station_id(station) {
            return Err(anyhow!("station must match ^S\\d+$"));
        }
        let zoned = rome_now()?;
        let date = date_param(&zoned);
        let segment = match kind {
            BoardKind::Departures => "partenze",
            BoardKind::Arrivals => "arrivi",
        };
        let url = format!("{}/{}/{}/{}", self.base_url, segment, station, pct(&date));
        let v = self.fetch_json(http, &url).await?;
        Ok(normalize_board(&v, kind))
    }
}

impl ViaggiaTreno {
    async fn fetch_json(&self, http: &reqwest::Client, url: &str) -> Result<Value, anyhow::Error> {
        let resp = http
            .get(url)
            .header("referer", "http://www.viaggiatreno.it/")
            .send()
            .await
            .map_err(|_| anyhow!("ViaggiaTreno is unavailable"))?;
        if !resp.status().is_success() {
            return Err(anyhow!("ViaggiaTreno is unavailable"));
        }
        // ViaggiaTreno legitimately returns empty bodies (e.g. boards at night).
        let text = resp
            .text()
            .await
            .map_err(|_| anyhow!("ViaggiaTreno is unavailable"))?;
        if text.trim().is_empty() {
            return Ok(Value::Array(Vec::new()));
        }
        serde_json::from_str(&text).context("ViaggiaTreno bad payload")
    }
}

fn rome_now() -> Result<jiff::Zoned, anyhow::Error> {
    let tz = jiff::tz::TimeZone::get("Europe/Rome")
        .map_err(|_| anyhow!("Europe/Rome timezone unavailable"))?;
    Ok(jiff::Timestamp::now().to_zoned(tz))
}

/// JS `Date.toString()`-style stamp in Europe/Rome wall-clock, which
/// ViaggiaTreno's board endpoints expect, e.g.
/// `Fri Jun 27 2025 14:30:00 GMT+0200 (Central European Summer Time)`.
fn date_param(zoned: &jiff::Zoned) -> String {
    let secs = zoned.offset().seconds();
    let sign = if secs < 0 { '-' } else { '+' };
    let hours = secs.abs() / 3600;
    let mins = (secs.abs() % 3600) / 60;
    // +02:00 is CEST (summer); otherwise CET (standard).
    let tz_name = if secs == 7200 {
        "Central European Summer Time"
    } else {
        "Central European Standard Time"
    };
    format!(
        "{} GMT{}{:02}{:02} ({})",
        zoned.strftime("%a %b %d %Y %H:%M:%S"),
        sign,
        hours,
        mins,
        tz_name,
    )
}

fn is_station_id(s: &str) -> bool {
    s.len() >= 2 && s.starts_with('S') && s[1..].bytes().all(|b| b.is_ascii_digit())
}

/// Percent-encode a path segment (RFC 3986 unreserved set kept).
fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn field<'a>(o: &'a Value, key: &str) -> Option<&'a str> {
    o.get(key).and_then(Value::as_str).map(str::trim)
}

fn normalize_search(v: &Value) -> Vec<Station> {
    array(v)
        .filter_map(|o| {
            let id = field(o, "id")?.to_string();
            let name = field(o, "nomeLungo")?.to_string();
            (!id.is_empty() && !name.is_empty()).then_some(Station {
                id,
                name,
                lat: None,
                lon: None,
            })
        })
        .collect()
}

fn normalize_list(v: &Value) -> Vec<Station> {
    array(v)
        .filter_map(|o| {
            let id = field(o, "codiceStazione")?.to_string();
            let name = o
                .get("localita")
                .and_then(|l| field(l, "nomeLungo"))?
                .to_string();
            if id.is_empty() || name.is_empty() {
                return None;
            }
            Some(Station {
                id,
                name,
                lat: coord(o.get("lat")),
                lon: coord(o.get("lon")),
            })
        })
        .collect()
}

fn normalize_board(v: &Value, kind: BoardKind) -> Vec<BoardEntry> {
    let (time_key, plat_eff, plat_prog) = match kind {
        BoardKind::Departures => (
            "compOrarioPartenza",
            "binarioEffettivoPartenzaDescrizione",
            "binarioProgrammatoPartenzaDescrizione",
        ),
        BoardKind::Arrivals => (
            "compOrarioArrivo",
            "binarioEffettivoArrivoDescrizione",
            "binarioProgrammatoArrivoDescrizione",
        ),
    };

    array(v)
        .filter_map(|o| {
            let train_number = field(o, "compNumeroTreno")?.to_string();
            let category = field(o, "categoria")
                .map(str::to_string)
                .unwrap_or_else(|| {
                    train_number
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_string()
                });
            let destination = field(o, "destinazione").unwrap_or_default().to_string();
            let origin = match kind {
                BoardKind::Departures => None,
                BoardKind::Arrivals => field(o, "origine").map(str::to_string),
            };
            let scheduled_time = field(o, time_key).unwrap_or_default().to_string();
            let delay_minutes = o.get("ritardo").and_then(Value::as_i64).unwrap_or(0) as i32;
            let platform = field(o, plat_eff)
                .or_else(|| field(o, plat_prog))
                .filter(|p| !p.is_empty())
                .map(str::to_string);

            Some(BoardEntry {
                train_number,
                category,
                origin,
                destination,
                scheduled_time,
                delay_minutes,
                platform,
            })
        })
        .collect()
}

fn array(v: &Value) -> impl Iterator<Item = &Value> {
    v.as_array().map(Vec::as_slice).unwrap_or(&[]).iter()
}

fn coord(v: Option<&Value>) -> Option<f64> {
    match v.and_then(Value::as_f64) {
        Some(n) if n != 0.0 => Some(n),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn station_id_validation() {
        assert!(is_station_id("S08409"));
        assert!(is_station_id("S1"));
        assert!(!is_station_id("08409"));
        assert!(!is_station_id("S"));
        assert!(!is_station_id("SABC"));
        assert!(!is_station_id(""));
    }

    #[test]
    fn percent_encodes_path_segment() {
        assert_eq!(pct("Roma Termini"), "Roma%20Termini");
        assert_eq!(pct("a(b)+c"), "a%28b%29%2Bc");
        assert_eq!(pct("plain-1.0_x~"), "plain-1.0_x~");
    }

    #[test]
    fn date_param_summer_and_winter() {
        let tz = jiff::tz::TimeZone::get("Europe/Rome").unwrap();
        // 2025-07-01 12:00 Rome → CEST (+0200).
        let summer: jiff::Timestamp = "2025-07-01T10:00:00Z".parse().unwrap();
        let s = date_param(&summer.to_zoned(tz.clone()));
        assert!(s.contains("GMT+0200"), "{s}");
        assert!(s.contains("(Central European Summer Time)"), "{s}");
        assert!(s.starts_with("Tue Jul 01 2025 12:00:00"), "{s}");

        // 2025-01-01 12:00 Rome → CET (+0100).
        let winter: jiff::Timestamp = "2025-01-01T11:00:00Z".parse().unwrap();
        let w = date_param(&winter.to_zoned(tz));
        assert!(w.contains("GMT+0100"), "{w}");
        assert!(w.contains("(Central European Standard Time)"), "{w}");
    }

    #[test]
    fn default_provider_uses_public_base_and_lazio() {
        let p = ViaggiaTreno::default();
        assert_eq!(p.base_url, DEFAULT_BASE_URL);
        assert_eq!(p.default_region, 5);
        // Empty/None inputs fall back to the same defaults.
        let p2 = ViaggiaTreno::new(Some(String::new()), None);
        assert_eq!(p2.base_url, DEFAULT_BASE_URL);
        assert_eq!(p2.default_region, 5);
        // Explicit overrides win.
        let p3 = ViaggiaTreno::new(Some("http://example.test".to_string()), Some(8));
        assert_eq!(p3.base_url, "http://example.test");
        assert_eq!(p3.default_region, 8);
    }

    #[test]
    fn normalize_search_maps_id_and_name() {
        let v = json!([
            {"id": "S08409", "nomeLungo": "ROMA TERMINI "},
            {"id": "", "nomeLungo": "skip"},
            {"nomeLungo": "no id"},
        ]);
        let out = normalize_search(&v);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "S08409");
        assert_eq!(out[0].name, "ROMA TERMINI");
        assert!(out[0].lat.is_none());
    }

    #[test]
    fn normalize_list_drops_zero_coords_and_missing_name() {
        let v = json!([
            {"codiceStazione": "S08409", "localita": {"nomeLungo": "Roma Termini"}, "lat": 41.9, "lon": 12.5},
            {"codiceStazione": "S00001", "localita": {"nomeLungo": "No Coords"}, "lat": 0, "lon": 0},
            {"codiceStazione": "S00002", "localita": {}},
        ]);
        let out = normalize_list(&v);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].lat, Some(41.9));
        assert_eq!(out[1].lat, None, "literal 0 → no coordinate");
    }

    #[test]
    fn normalize_departures_board() {
        let v = json!([{
            "compNumeroTreno": "REG 22815",
            "categoria": "REG",
            "destinazione": "NAPOLI ",
            "compOrarioPartenza": "14:35",
            "ritardo": 5,
            "binarioEffettivoPartenzaDescrizione": "3",
        }]);
        let out = normalize_board(&v, BoardKind::Departures);
        assert_eq!(out.len(), 1);
        let e = &out[0];
        assert_eq!(e.train_number, "REG 22815");
        assert_eq!(e.category, "REG");
        assert_eq!(e.destination, "NAPOLI");
        assert_eq!(e.scheduled_time, "14:35");
        assert_eq!(e.delay_minutes, 5);
        assert_eq!(e.platform.as_deref(), Some("3"));
        assert!(e.origin.is_none());
    }

    #[test]
    fn normalize_arrivals_uses_origine_and_falls_back_platform() {
        let v = json!([{
            "compNumeroTreno": "IC 581",
            "origine": "MILANO",
            "compOrarioArrivo": "09:10",
            "ritardo": -2,
            "binarioProgrammatoArrivoDescrizione": "1",
        }]);
        let out = normalize_board(&v, BoardKind::Arrivals);
        let e = &out[0];
        assert_eq!(e.origin.as_deref(), Some("MILANO"));
        assert_eq!(
            e.category, "IC",
            "derived from compNumeroTreno when categoria absent"
        );
        assert_eq!(e.delay_minutes, -2);
        assert_eq!(
            e.platform.as_deref(),
            Some("1"),
            "falls back to programmato platform"
        );
    }
}
