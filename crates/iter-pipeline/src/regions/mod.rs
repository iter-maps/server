//! Region/country-specific drivers (ADR 0017). When a build step needs genuinely
//! custom code — not just config — the generic pipeline dispatches through a
//! trait to the implementation for the deployment's region, selected from the
//! resolved region (the country = first segment of the region target, the city =
//! `region.id`). Adding a city = a new `regions::<country>::<city>` module
//! implementing the trait; the generic step is untouched.

pub mod italy;

use std::sync::Arc;

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
/// through (ADR 0017). The geometry (way-union, hulls, platform offsets, GeoJSON
/// emission) is generic; everything an operator decides — which operator's
/// relations to keep, which refs are metro lines, their contract colours, branch
/// splits, the gtfs lookup keys, the route-id prefix, the feed filename, and the
/// projection origin — lives behind this trait.
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

/// Select the transit-overlay driver for a region's `(country, city)`. Returns
/// `None` when no network driver exists for the region — the overlay step then
/// logs and skips, so a region without a driver simply produces no overlays.
pub fn overlay_driver(country: &str, city: &str) -> Option<Arc<dyn TransitOverlayDriver>> {
    match (country, city) {
        ("italy", "rome") => Some(Arc::new(italy::rome::RomeOverlayDriver)),
        _ => None,
    }
}
