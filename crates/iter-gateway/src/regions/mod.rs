//! Region/country-specific drivers (ADR 0017). When a surface needs genuinely
//! custom code — not just config — the generic gateway dispatches through a
//! trait to the implementation for the deployment's country, selected from the
//! resolved region (the first segment of the region path, e.g. `italy`). Adding
//! a country = a new `regions::<country>` module implementing the trait; the
//! generic core is untouched.

pub mod italy;

use std::sync::Arc;

use iter_contracts::live_trains::{BoardEntry, Station};
use iter_core::ApiError;

/// Which board a live-trains query asks for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BoardKind {
    Departures,
    Arrivals,
}

/// Country-specific live-train provider (ADR 0017): the upstream API's endpoint
/// shapes, field names, station-id format and date param are all country/operator
/// specific, so the generic handlers dispatch through this trait to the driver
/// for the deployment's country. The TTL cache + single-flight + axum handlers
/// stay generic in the core; only the upstream client lives here.
///
/// The driver owns its own upstream base URL and any region fallback — those
/// literals never leak into the generic config or handlers.
#[async_trait::async_trait]
pub trait LiveTrainsProvider: Send + Sync {
    /// Autocomplete stations by free-text `query`. Drivers validate `query`
    /// themselves; the handler only enforces the generic min-length guard.
    async fn search(&self, http: &reqwest::Client, query: &str) -> Result<Vec<Station>, ApiError>;

    /// The full station list for a region. `region_code` is the caller-supplied
    /// override; `None` means "use the driver's default region".
    async fn list(
        &self,
        http: &reqwest::Client,
        region_code: Option<i64>,
    ) -> Result<Vec<Station>, ApiError>;

    /// A departures/arrivals board for `station`. Returns a validation
    /// [`ApiError`] (400) when `station` is not a valid id for this provider.
    async fn board(
        &self,
        http: &reqwest::Client,
        station: &str,
        kind: BoardKind,
    ) -> Result<Vec<BoardEntry>, ApiError>;
}

/// Select the live-trains provider for a region's country. The optional
/// `base_url`/`region_code` are passed to the chosen driver (the upstream
/// endpoint override and the default station-list region); each driver owns its
/// own fallbacks for them. Unknown countries get a stub that returns empty
/// results — the surface stays wired but inert, exactly like
/// [`GenericNormalizer`] for address correlation.
pub fn live_trains_provider(
    country: &str,
    base_url: Option<String>,
    region_code: Option<i64>,
) -> Arc<dyn LiveTrainsProvider> {
    match country {
        "italy" => Arc::new(italy::live_trains::ViaggiaTreno::new(base_url, region_code)),
        _ => Arc::new(NoLiveTrains),
    }
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
    ) -> Result<Vec<Station>, ApiError> {
        Ok(Vec::new())
    }

    async fn list(
        &self,
        _http: &reqwest::Client,
        _region_code: Option<i64>,
    ) -> Result<Vec<Station>, ApiError> {
        Ok(Vec::new())
    }

    async fn board(
        &self,
        _http: &reqwest::Client,
        _station: &str,
        _kind: BoardKind,
    ) -> Result<Vec<BoardEntry>, ApiError> {
        Ok(Vec::new())
    }
}

/// Country-specific address normalization for the place-correlation bucket key
/// (street-type expansion, house-number/esponente rules, …). The bucket key is
/// the join: two records share an address iff their keys match (ADR 0012).
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

/// Select the address normalizer for a region's country. Unknown countries get a
/// minimal generic normalizer (no country rules).
pub fn address_normalizer(country: &str) -> Arc<dyn AddressNormalizer> {
    match country {
        "italy" => Arc::new(italy::address::ItalyNormalizer),
        _ => Arc::new(GenericNormalizer),
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

    #[test]
    fn selector_picks_italy() {
        // italy gets the DUG-aware normalizer (V.le == Viale); generic does not.
        let it = address_normalizer("italy");
        let generic = address_normalizer("narnia");
        assert_eq!(
            it.bucket_key("V.le Roma", "1", "X"),
            it.bucket_key("Viale Roma", "1", "X")
        );
        assert_ne!(
            generic.bucket_key("V.le Roma", "1", "X"),
            generic.bucket_key("Viale Roma", "1", "X")
        );
    }
}
