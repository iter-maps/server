//! Address correlation (ADR 0012, concept doc 20 §2.2). Given an address the
//! user searched (a civico), surface the places at it — the restaurant at
//! "Via Cavour 1" — as `related[]`. This deliberately does NOT dedup the way
//! geocoders do (Pelias collapses the venue and its address; Nominatim/Photon
//! never link a POI to its house number): we *attach*.
//!
//! Backed by a build-time `places.jsonl` (the PLACES pipeline step extracts
//! addressed POIs from Overture). The gateway loads it once into an in-memory
//! bucket index keyed by the normalized address (see [`crate::address`]) and by
//! brand QID, so a replica answers from memory — the stateless + regenerable
//! artifact model.

use std::collections::HashMap;
use std::path::Path;

use axum::Json;
use axum::extract::{Query, State};
use iter_contracts::places::{LonLat, Related, Relation};
use serde::{Deserialize, Serialize};

use crate::address::{bucket_key, split_freeform};
use crate::http::ApiResult;
use crate::state::AppState;

/// One addressed place from the build-time extract.
#[derive(Debug, Clone, Deserialize)]
pub struct Poi {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub category: Option<String>,
    /// Freeform address ("Via Cavour 1") — Overture places merge street+number.
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub city: Option<String>,
    #[serde(default)]
    pub brand_wikidata: Option<String>,
    pub lon: f64,
    pub lat: f64,
}

/// In-memory correlation index: address bucket → places, brand QID → places.
#[derive(Default)]
pub struct CorrelationIndex {
    by_address: HashMap<String, Vec<Poi>>,
    by_brand: HashMap<String, Vec<Poi>>,
}

impl CorrelationIndex {
    /// Build the index from `places.jsonl`. A missing file yields an empty index
    /// (correlation simply returns nothing) — the surface degrades, never fails.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            tracing::info!(path = %path.display(), "no places index; correlation disabled");
            return Self::default();
        };
        let pois = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<Poi>(l).ok());
        Self::from_pois(pois)
    }

    fn from_pois(pois: impl IntoIterator<Item = Poi>) -> Self {
        let mut idx = Self::default();
        for poi in pois {
            if let Some(addr) = &poi.address {
                let (street, number) = split_freeform(addr);
                if let Some(number) = number {
                    let key = bucket_key(&street, &number, poi.city.as_deref().unwrap_or(""));
                    idx.by_address.entry(key).or_default().push(poi.clone());
                }
            }
            if let Some(brand) = &poi.brand_wikidata {
                idx.by_brand
                    .entry(brand.clone())
                    .or_default()
                    .push(poi.clone());
            }
        }
        idx
    }

    pub fn len(&self) -> usize {
        self.by_address.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.by_address.is_empty()
    }

    /// Places sharing the address bucket, as `sameAddress` relations.
    fn at_address(
        &self,
        street: &str,
        housenumber: &str,
        city: &str,
        anchor: Option<LonLat>,
    ) -> Vec<Related> {
        let key = bucket_key(street, housenumber, city);
        self.by_address
            .get(&key)
            .into_iter()
            .flatten()
            .map(|p| related(p, Relation::SameAddress, anchor))
            .collect()
    }

    fn same_brand(&self, brand: &str, anchor: Option<LonLat>) -> Vec<Related> {
        self.by_brand
            .get(brand)
            .into_iter()
            .flatten()
            .map(|p| related(p, Relation::SameBrand, anchor))
            .collect()
    }
}

fn related(p: &Poi, relation: Relation, anchor: Option<LonLat>) -> Related {
    let location = LonLat {
        lon: p.lon,
        lat: p.lat,
    };
    Related {
        id: p.id.clone(),
        name: p.name.clone(),
        category: p.category.clone(),
        relation,
        location,
        distance_m: anchor.map(|a| haversine_m(a, location)),
    }
}

/// Great-circle distance in metres.
fn haversine_m(a: LonLat, b: LonLat) -> f64 {
    let r = 6_371_000.0_f64;
    let (lat1, lat2) = (a.lat.to_radians(), b.lat.to_radians());
    let dlat = (b.lat - a.lat).to_radians();
    let dlon = (b.lon - a.lon).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * r * h.sqrt().asin()
}

#[derive(Debug, Deserialize)]
pub struct RelatedParams {
    pub street: Option<String>,
    pub housenumber: Option<String>,
    #[serde(default)]
    pub city: Option<String>,
    #[serde(default)]
    pub brand: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct RelatedResponse {
    pub related: Vec<Related>,
}

/// `GET /places/related?street=&housenumber=&city=` — the places at that civico
/// (and, if `brand` is given, the same chain elsewhere). Unknown address → empty
/// `related`, never an error.
pub async fn related_places(
    State(state): State<AppState>,
    Query(p): Query<RelatedParams>,
) -> ApiResult<Json<RelatedResponse>> {
    let anchor = match (p.lon, p.lat) {
        (Some(lon), Some(lat)) => Some(LonLat { lon, lat }),
        _ => None,
    };
    let mut related = Vec::new();
    if let (Some(street), Some(hn)) = (&p.street, &p.housenumber) {
        related.extend(state.correlations.at_address(
            street,
            hn,
            p.city.as_deref().unwrap_or(""),
            anchor,
        ));
    }
    if let Some(brand) = &p.brand {
        related.extend(state.correlations.same_brand(brand, anchor));
    }
    Ok(Json(RelatedResponse { related }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poi(id: &str, name: &str, addr: &str, brand: Option<&str>) -> Poi {
        Poi {
            id: id.into(),
            name: name.into(),
            category: Some("catering.restaurant".into()),
            address: Some(addr.into()),
            city: Some("Roma".into()),
            brand_wikidata: brand.map(str::to_string),
            lon: 12.49,
            lat: 41.90,
        }
    }

    #[test]
    fn correlates_places_at_the_same_civico() {
        let idx = CorrelationIndex::from_pois([
            poi("a", "Ristorante Cavour", "Via Cavour 1", None),
            poi("b", "Bar Cavour", "V. Cavour 1", None), // abbreviated → same bucket
            poi("c", "Altrove", "Via Merulana 10", None),
        ]);
        // query with a different abbreviation still hits the bucket.
        let r = idx.at_address("Via Cavour", "1", "Roma", None);
        let names: Vec<&str> = r.iter().map(|x| x.name.as_str()).collect();
        assert!(names.contains(&"Ristorante Cavour"));
        assert!(names.contains(&"Bar Cavour"));
        assert!(!names.contains(&"Altrove"));
        assert!(r.iter().all(|x| x.relation == Relation::SameAddress));
    }

    #[test]
    fn unknown_address_returns_empty() {
        let idx = CorrelationIndex::from_pois([poi("a", "X", "Via Cavour 1", None)]);
        assert!(idx.at_address("Via Nowhere", "99", "Roma", None).is_empty());
    }

    #[test]
    fn same_brand_groups_a_chain() {
        let idx = CorrelationIndex::from_pois([
            poi("a", "Caffè 1", "Via Cavour 1", Some("Q608427")),
            poi("b", "Caffè 2", "Via Nazionale 5", Some("Q608427")),
            poi("c", "Other", "Via Cavour 1", None),
        ]);
        let r = idx.same_brand("Q608427", None);
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|x| x.relation == Relation::SameBrand));
    }

    #[test]
    fn anchor_adds_distance() {
        let idx = CorrelationIndex::from_pois([poi("a", "X", "Via Cavour 1", None)]);
        let r = idx.at_address(
            "Via Cavour",
            "1",
            "Roma",
            Some(LonLat {
                lon: 12.49,
                lat: 41.91,
            }),
        );
        assert!(r[0].distance_m.unwrap() > 1000.0); // ~1.1 km north
    }

    #[test]
    fn pois_without_a_number_are_not_address_indexed() {
        let idx = CorrelationIndex::from_pois([poi("a", "Park", "Largo Argentina", None)]);
        assert!(idx.is_empty());
    }
}
