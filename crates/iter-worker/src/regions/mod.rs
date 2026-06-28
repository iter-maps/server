//! Region/country-specific drivers (ADR 0017). The NeTEx→GTFS converter
//! (`netex.rs`) is generic EU-standard NeTEx + GTFS; the bits that are tied to a
//! country's NeTEx profile — the id codespace scheme and the synthesized agency
//! block — are dispatched through a trait to the implementation for the feed's
//! country. Adding a country = a new `regions::<country>` module implementing the
//! trait; the generic converter is untouched.

pub mod italy;

use std::sync::Arc;

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

/// The default profile id: Italian NeTEx-IT (`IT:ITI4`), Trenitalia-FL.
pub const DEFAULT_NETEX_PROFILE: &str = "it-iti4";

/// Select the NeTEx profile by id. The default (`it-iti4`) is the Italian
/// NeTEx-IT profile; unknown ids fall back to it.
pub fn netex_profile(id: &str) -> Arc<dyn NetexProfile> {
    match id {
        "it-iti4" => Arc::new(italy::netex::ItalyNetex),
        _ => Arc::new(italy::netex::ItalyNetex),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_selects_italy() {
        let p = netex_profile(DEFAULT_NETEX_PROFILE);
        assert_eq!(
            p.strip_id("IT:ITI4:ScheduledStopPoint:830008328_pass_0083"),
            "830008328_pass_0083"
        );
        assert_eq!(p.agency().id, "FL");
    }

    #[test]
    fn unknown_falls_back_to_italy() {
        let p = netex_profile("narnia");
        assert_eq!(p.agency().name, "Trenitalia");
    }
}
