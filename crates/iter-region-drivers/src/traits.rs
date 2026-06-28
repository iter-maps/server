//! The four region-driver traits and their value types, decoupled from any tier
//! crate (ADR 0018). Provider errors are the neutral `anyhow::Error` so the
//! traits never reference tier types like the gateway's `ApiError`; each tier
//! maps `anyhow` into its own error. The generic fallbacks
//! ([`GenericNormalizer`], [`NoLiveTrains`]) and `default_split_freeform` live
//! here too.

use iter_contracts::live_trains::{BoardEntry, Station};

// ---------------------------------------------------------------------------
// Live trains
// ---------------------------------------------------------------------------

/// Which board a live-trains query asks for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BoardKind {
    Departures,
    Arrivals,
}

/// Country-specific live-train provider: endpoint shapes, field names, station-id
/// format and date param are all country/operator specific, so the generic
/// gateway handlers dispatch through this trait to the deployment's driver. The
/// driver owns its own upstream base URL and region fallback so those literals
/// never leak into the generic config.
#[async_trait::async_trait]
pub trait LiveTrainsProvider: Send + Sync {
    /// Autocomplete stations by free-text `query`. Drivers validate `query`
    /// themselves; the handler only enforces the generic min-length guard.
    async fn search(
        &self,
        http: &reqwest::Client,
        query: &str,
    ) -> Result<Vec<Station>, anyhow::Error>;

    /// The full station list for a region. `region_code` is the caller-supplied
    /// override; `None` means "use the driver's default region".
    async fn list(
        &self,
        http: &reqwest::Client,
        region_code: Option<i64>,
    ) -> Result<Vec<Station>, anyhow::Error>;

    /// A departures/arrivals board for `station`. Returns an error when `station`
    /// is not a valid id for this provider (the gateway maps it to a 400).
    async fn board(
        &self,
        http: &reqwest::Client,
        station: &str,
        kind: BoardKind,
    ) -> Result<Vec<BoardEntry>, anyhow::Error>;
}

/// Fallback live-trains provider for countries with no driver: empty boards and
/// station lists, never an upstream call.
pub struct NoLiveTrains;

#[async_trait::async_trait]
impl LiveTrainsProvider for NoLiveTrains {
    async fn search(
        &self,
        _http: &reqwest::Client,
        _query: &str,
    ) -> Result<Vec<Station>, anyhow::Error> {
        Ok(Vec::new())
    }

    async fn list(
        &self,
        _http: &reqwest::Client,
        _region_code: Option<i64>,
    ) -> Result<Vec<Station>, anyhow::Error> {
        Ok(Vec::new())
    }

    async fn board(
        &self,
        _http: &reqwest::Client,
        _station: &str,
        _kind: BoardKind,
    ) -> Result<Vec<BoardEntry>, anyhow::Error> {
        Ok(Vec::new())
    }
}

// ---------------------------------------------------------------------------
// Address normalization
// ---------------------------------------------------------------------------

/// Country-specific address normalization for the place-correlation bucket key
/// (street-type expansion, house-number/esponente rules, …). Two records share
/// an address iff their bucket keys match.
pub trait AddressNormalizer: Send + Sync {
    /// The correlation bucket for `(street, housenumber, city)`.
    fn bucket_key(&self, street: &str, housenumber: &str, city: &str) -> String;

    /// Split a freeform address ("Via X 12") into `(street, number)`. The
    /// default takes the trailing numeric token (number-after-street, as in most
    /// of Europe); override for number-first locales.
    fn split_freeform(&self, freeform: &str) -> (String, Option<String>) {
        default_split_freeform(freeform)
    }
}

/// Fallback normalizer: lowercase + keep alphanumerics, no country-specific
/// street-type or house-number rules.
pub struct GenericNormalizer;

impl AddressNormalizer for GenericNormalizer {
    fn bucket_key(&self, street: &str, housenumber: &str, city: &str) -> String {
        format!(
            "{}|{}|{}",
            squash(city),
            squash(street),
            squash(housenumber)
        )
    }
}

fn squash(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Trailing-number freeform split (the shared default).
pub(crate) fn default_split_freeform(s: &str) -> (String, Option<String>) {
    let trimmed = s.trim().trim_end_matches([',', ' ']);
    if let Some(pos) = trimmed.rfind([' ', ',']) {
        let (head, tail) = trimmed.split_at(pos);
        let tail = tail.trim_start_matches([',', ' ']);
        if tail.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return (
                head.trim_end_matches([',', ' ']).to_string(),
                Some(tail.to_string()),
            );
        }
    }
    (trimmed.to_string(), None)
}

// ---------------------------------------------------------------------------
// Transit overlay
// ---------------------------------------------------------------------------

/// The OSM-relation kind a route line belongs to. The generic geometry treats
/// metro and tram differently (colour, gtfs-key convention); the driver owns the
/// per-operator rules.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum LineKind {
    Metro,
    Tram,
}

/// A local-planar projection origin for the metre-space geometry: the WGS84
/// origin plus metres-per-degree longitude at that latitude (latitude is the
/// generic ~111_320 m/deg and lives in the step). City-specific because the
/// longitude scale is `cos(origin_lat) · 111_320`.
#[derive(Clone, Copy, Debug)]
pub struct Projection {
    pub origin_lon: f64,
    pub origin_lat: f64,
    pub m_per_deg_lon: f64,
}

/// The operator/network rules the generic transit-overlay algorithm dispatches
/// through. The geometry (way-union, hulls, platform offsets, GeoJSON emission)
/// is generic; everything an operator decides — which relations to keep, which
/// refs are metro lines, their colours, branch splits, the gtfs lookup keys, the
/// route-id prefix, the feed filename, and the projection origin — lives here.
pub trait TransitOverlayDriver: Send + Sync {
    /// The OSM `operator` tag value whose route relations this network owns.
    fn operator(&self) -> &str;

    /// Whether a route `ref` is one of this network's metro lines (the allow-set
    /// that promotes a `route=subway` relation to a metro line).
    fn is_metro_line(&self, line: &str) -> bool;

    /// The contract colour (`#RRGGBB`) for a metro line, used when the GTFS feed
    /// carries no `route_color`.
    fn metro_color(&self, line: &str) -> &str;

    /// Relabel a metro line to a branch when its terminus slugs identify a branch
    /// (e.g. Rome's B → B1 for the Jonio/Conca d'Oro spur). `None` keeps the line
    /// as declared.
    fn relabel_branch(&self, line: &str, terminus_slugs: &[String]) -> Option<String>;

    /// The `route_short_name` key under which to look this line up in the GTFS
    /// `routes.txt` (e.g. metro `A` → `MEA`, tram keeps its number).
    fn gtfs_key(&self, kind: LineKind, line: &str) -> String;

    /// The prefix the emitted GeoJSON route id carries before the GTFS route id
    /// (e.g. `ATAC:`).
    fn route_id_prefix(&self) -> &str;

    /// The GTFS feed filename in the graph dir whose `routes.txt` supplies route
    /// ids + colours (e.g. `ATAC.gtfs.zip`).
    fn gtfs_filename(&self) -> &str;

    /// The local-planar projection origin for the metre-space geometry.
    fn projection(&self) -> Projection;
}

// ---------------------------------------------------------------------------
// NeTEx profile
// ---------------------------------------------------------------------------

/// The synthesized GTFS `agency.txt` row for a NeTEx feed (NeTEx carries no GTFS
/// agency, so the profile supplies it).
pub struct AgencyInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub timezone: &'static str,
    pub lang: &'static str,
}

/// A country's NeTEx profile: the parts of NeTEx→GTFS conversion that aren't
/// EU-standard — the id codespace scheme and the agency to synthesize.
pub trait NetexProfile: Send + Sync {
    /// Strip the country's NeTEx id codespace prefix to a clean GTFS local id.
    fn strip_id(&self, raw: &str) -> String;

    /// The agency block to synthesize into the GTFS feed.
    fn agency(&self) -> AgencyInfo;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_split_takes_trailing_number() {
        assert_eq!(
            default_split_freeform("Main Road 12"),
            ("Main Road".to_string(), Some("12".to_string()))
        );
        assert_eq!(
            default_split_freeform("Some Park"),
            ("Some Park".to_string(), None)
        );
    }

    #[test]
    fn generic_normalizer_buckets_case_insensitively() {
        let n = GenericNormalizer;
        assert_eq!(
            n.bucket_key("Main Rd", "12", "Town"),
            n.bucket_key("MAIN  RD", "12", "town")
        );
    }
}
