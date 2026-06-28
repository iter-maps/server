//! Italian NeTEx-IT (`IT:ITI4`) profile for Trenitalia-FL (concept doc 11, ADR
//! 0004, ADR 0017). This is the Italy driver behind the generic
//! [`crate::regions::NetexProfile`] trait: the `IT:ITI4` id
//! codespace scheme and the FL/`Europe/Rome`/`it` agency Trenitalia's NeTEx
//! carries no GTFS agency for. The streaming parser and GTFS structure stay
//! generic in `netex.rs`.

use crate::regions::{AgencyInfo, NetexProfile};

/// The Italian NeTEx-IT (`IT:ITI4`) profile, Trenitalia-FL.
pub struct ItalyNetex;

impl NetexProfile for ItalyNetex {
    fn strip_id(&self, raw: &str) -> String {
        gid(raw)
    }

    fn agency(&self) -> AgencyInfo {
        AgencyInfo {
            id: "FL",
            name: "Trenitalia",
            url: "https://www.trenitalia.com",
            timezone: "Europe/Rome",
            lang: "it",
        }
    }
}

/// NeTEx id → a clean GTFS local id: the part after the `IT:ITI4:<Type>:` prefix
/// (`IT:ITI4:ScheduledStopPoint:830008328_pass_0083` → `830008328_pass_0083`).
/// Other countries' NeTEx use a different prefix shape — this is the Italian
/// NeTEx-IT id scheme.
fn gid(s: &str) -> String {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() > 3 {
        parts[3..].join("_")
    } else {
        s.replace(':', "_")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_it_iti4_prefix() {
        assert_eq!(
            gid("IT:ITI4:ScheduledStopPoint:830008328_pass_0083"),
            "830008328_pass_0083"
        );
        // a profile call goes through the same scheme.
        assert_eq!(
            ItalyNetex.strip_id("IT:ITI4:Line:10083_pass_0083"),
            "10083_pass_0083"
        );
    }

    #[test]
    fn synthesizes_the_trenitalia_agency() {
        let a = ItalyNetex.agency();
        assert_eq!(a.id, "FL");
        assert_eq!(a.name, "Trenitalia");
        assert_eq!(a.url, "https://www.trenitalia.com");
        assert_eq!(a.timezone, "Europe/Rome");
        assert_eq!(a.lang, "it");
    }
}
