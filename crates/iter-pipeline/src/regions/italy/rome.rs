//! Rome / ATAC transit-overlay driver — the operator/network specifics behind the
//! generic [`TransitOverlayDriver`] trait (ADR 0017). The overlay *geometry* (the
//! osmpbf harvest, way-union, concave-hull concourse, platform offset math,
//! GeoJSON emission) is generic and lives in `steps/overlay.rs`; what's specific
//! to Rome's metro/tram network lives here:
//!
//! - the `ATAC` operator filter and the `A|B|C` metro line allow-set;
//! - the contract colours (A orange, B/B1 blue, C green);
//! - the B1 branch split (a line-B direction that serves Jonio / Conca d'Oro is
//!   the B1 spur);
//! - the GTFS conventions (metro short-names `ME{line}`, the `ATAC:` route-id
//!   prefix, the `ATAC.gtfs.zip` feed filename);
//! - the central-Rome projection origin (`12.5, 41.9`; the longitude scale is
//!   `cos(41.9°) · 111_320 ≈ 82_800` m/deg).
//!
//! Moving these to `region.toml` data is a separate future step (ADR 0017 tier
//! 1); this driver only *isolates* them out of the generic core.

use crate::regions::{LineKind, Projection, TransitOverlayDriver};

/// The OSM `operator` tag value identifying Rome's metro/tram network.
const OPERATOR: &str = "ATAC";

/// Prefix on the emitted GeoJSON route id (`ATAC:<gtfs route id>`).
const ROUTE_ID_PREFIX: &str = "ATAC:";

/// The GTFS feed in the graph dir whose `routes.txt` supplies route ids/colours.
const GTFS_FILENAME: &str = "ATAC.gtfs.zip";

// Local planar projection around central Rome (metres per degree at ~41.9°N).
const ORIGIN_LON: f64 = 12.5;
const ORIGIN_LAT: f64 = 41.9;
// cos(41.9°) · 111_320 — metres per degree longitude at the origin latitude.
const M_LON: f64 = 82_800.0;

/// Rome's ATAC metro/tram overlay rules.
pub struct RomeOverlayDriver;

impl TransitOverlayDriver for RomeOverlayDriver {
    fn operator(&self) -> &str {
        OPERATOR
    }

    fn is_metro_line(&self, line: &str) -> bool {
        matches!(line, "A" | "B" | "C")
    }

    fn metro_color(&self, line: &str) -> &str {
        contract_metro_color(line)
    }

    fn relabel_branch(&self, line: &str, terminus_slugs: &[String]) -> Option<String> {
        // B1 split: a line-B direction serving Jonio / Conca d'Oro is the B1 branch.
        if line == "B" && terminus_slugs.iter().any(|s| is_b1_terminus_slug(s)) {
            Some("B1".to_string())
        } else {
            None
        }
    }

    fn gtfs_key(&self, kind: LineKind, line: &str) -> String {
        match kind {
            LineKind::Metro => format!("ME{line}"),
            LineKind::Tram => line.to_string(),
        }
    }

    fn route_id_prefix(&self) -> &str {
        ROUTE_ID_PREFIX
    }

    fn gtfs_filename(&self) -> &str {
        GTFS_FILENAME
    }

    fn projection(&self) -> Projection {
        Projection {
            origin_lon: ORIGIN_LON,
            origin_lat: ORIGIN_LAT,
            m_per_deg_lon: M_LON,
        }
    }
}

/// Contract metro-line colours, used when the GTFS feed carries no `route_color`.
fn contract_metro_color(line: &str) -> &'static str {
    match line {
        "A" => "#E27439",
        "B" | "B1" => "#0570B5",
        "C" => "#008456",
        _ => "#666666",
    }
}

/// Whether a station slug is a B1-branch terminus (Jonio / Conca d'Oro). The
/// generic step slugs the terminus name; the branch rule is matched on the slug.
fn is_b1_terminus_slug(slug: &str) -> bool {
    slug.contains("jonio") || slug.contains("conca")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_and_metro_lines() {
        let d = RomeOverlayDriver;
        assert_eq!(d.operator(), "ATAC");
        assert!(d.is_metro_line("A"));
        assert!(d.is_metro_line("B"));
        assert!(d.is_metro_line("C"));
        assert!(!d.is_metro_line("8")); // a tram line
        assert!(!d.is_metro_line("B1")); // the branch is derived, not a base line
    }

    #[test]
    fn contract_colors() {
        let d = RomeOverlayDriver;
        assert_eq!(d.metro_color("A"), "#E27439");
        assert_eq!(d.metro_color("B"), "#0570B5");
        assert_eq!(d.metro_color("B1"), "#0570B5");
        assert_eq!(d.metro_color("C"), "#008456");
        assert_eq!(d.metro_color("X"), "#666666");
    }

    #[test]
    fn b1_branch_relabel() {
        let d = RomeOverlayDriver;
        // a line-B direction terminating at Jonio / Conca d'Oro becomes B1.
        assert_eq!(
            d.relabel_branch("B", &["jonio".to_string()]),
            Some("B1".to_string())
        );
        assert_eq!(
            d.relabel_branch("B", &["conca-d-oro".to_string()]),
            Some("B1".to_string())
        );
        // the main B branch (Laurentina) stays B.
        assert_eq!(d.relabel_branch("B", &["laurentina".to_string()]), None);
        // other lines are never relabelled.
        assert_eq!(d.relabel_branch("A", &["jonio".to_string()]), None);
    }

    #[test]
    fn b1_terminus_slug_detected() {
        assert!(is_b1_terminus_slug("jonio"));
        assert!(is_b1_terminus_slug("conca-d-oro"));
        assert!(!is_b1_terminus_slug("laurentina"));
    }

    #[test]
    fn gtfs_keys_and_prefix() {
        let d = RomeOverlayDriver;
        assert_eq!(d.gtfs_key(LineKind::Metro, "A"), "MEA");
        assert_eq!(d.gtfs_key(LineKind::Tram, "8"), "8");
        assert_eq!(d.route_id_prefix(), "ATAC:");
        assert_eq!(d.gtfs_filename(), "ATAC.gtfs.zip");
    }

    #[test]
    fn projection_origin_is_central_rome() {
        let p = RomeOverlayDriver.projection();
        assert_eq!(p.origin_lon, 12.5);
        assert_eq!(p.origin_lat, 41.9);
        assert_eq!(p.m_per_deg_lon, 82_800.0);
    }
}
