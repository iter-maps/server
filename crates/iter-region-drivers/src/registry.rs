//! The four region-driver selectors (ADR 0018). Each maps a primitive (country,
//! city, profile id) to an `Arc<dyn …>` driver, dispatching the known regions to
//! their `italy::…` impls and everything else to the generic/stub/`None`
//! fallbacks. The tiers resolve the region (`iter-region`) and call these with
//! primitives; the registry never sees `region.toml`.

use std::sync::Arc;

use crate::italy;
use crate::traits::{
    AddressNormalizer, GenericNormalizer, LiveTrainsProvider, NetexProfile, NoLiveTrains,
    TransitOverlayDriver,
};

/// Select the address normalizer for a region's country. Unknown countries get a
/// minimal generic normalizer (no country rules).
pub fn address_normalizer(country: &str) -> Arc<dyn AddressNormalizer> {
    match country {
        "italy" => Arc::new(italy::address::ItalyNormalizer),
        _ => Arc::new(GenericNormalizer),
    }
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

/// Select the transit-overlay driver for a region's `(country, city)`. Returns
/// `None` when no network driver exists for the region — the overlay step then
/// logs and skips, so a region without a driver simply produces no overlays.
pub fn overlay_driver(country: &str, city: &str) -> Option<Arc<dyn TransitOverlayDriver>> {
    match (country, city) {
        ("italy", "rome") => Some(Arc::new(italy::rome::RomeOverlayDriver)),
        _ => None,
    }
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
