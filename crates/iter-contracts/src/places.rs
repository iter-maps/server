//! Place enrichment & discovery DTOs. The enrichment layer lives in the
//! gateway/BFF *above* geocoding: it fuses open sources (OSM facets, Wikidata,
//! Wikipedia, Wikimedia Commons, Overture Places) into one normalized
//! [`Place`], every displayed field carrying its source + license in
//! `provenance`, plus the [`Related`] places sharing its address. This is a
//! separate surface the client calls for a tapped result; geocoding's own
//! GeoJSON shape is unchanged.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A WGS84 point, serialized as named fields so the enrichment surface is
/// self-describing (geocoding keeps GeoJSON `[lon, lat]` arrays).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LonLat {
    pub lon: f64,
    pub lat: f64,
}

/// A normalized, enriched place fused from open sources.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Place {
    /// Canonical id, scheme-prefixed: `wd:Q…` (Wikidata, preferred),
    /// `osm:N|W|R<id>`, or `ov:<gers>`.
    pub id: String,
    pub name: String,
    /// Localized names, e.g. `{"it":"Colosseo","en":"Colosseum"}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub names: BTreeMap<String, String>,
    pub category: Category,
    pub location: LonLat,
    /// One-liner (Wikidata CC0 → Wikipedia `description`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Prose summary (Wikipedia `extract`, carries CC-BY-SA credit).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<Image>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<Address>,
    #[serde(default, skip_serializing_if = "Facets::is_empty")]
    pub facets: Facets,
    /// Places correlated to this one — chiefly those at the same address +
    /// civico (the restaurant at the searched number). Correlations are
    /// recorded, not deduped away. Empty when none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<Related>,
    /// Per-field source + license so attribution is mechanically satisfiable
    /// downstream (CC-BY-SA share-alike, ODbL, CC0, Commons per-file).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<Provenance>,
}

/// Owned, stable category taxonomy plus the raw source tags: `primary`
/// survives source changes; `tags` keep the long tail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Category {
    pub primary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// A displayable image with the attribution its license requires. `proxied`
/// is `true` for open-layer images (Commons) served through the gateway;
/// `false` for sources whose ToS forbids proxying (client fetches directly).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Image {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
    pub proxied: bool,
}

/// A structured postal address (Italian civico = `housenumber`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Address {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub street: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub housenumber: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postcode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
}

/// Open-data facets straight from OSM tags — the keyless filter set. All
/// optional; absent fields are omitted from the wire.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Facets {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub website: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opening_hours: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wheelchair: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cuisine: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diet: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outdoor_seating: Option<bool>,
}

impl Facets {
    pub fn is_empty(&self) -> bool {
        self.website.is_none()
            && self.phone.is_none()
            && self.opening_hours.is_none()
            && self.wheelchair.is_none()
            && self.cuisine.is_none()
            && self.diet.is_empty()
            && self.outdoor_seating.is_none()
    }
}

/// A place correlated to the primary result. `relation` names *why* they're
/// linked; `sameAddress` is the flagship (the venue at the searched civico).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Related {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub relation: Relation,
    pub location: LonLat,
    /// Metres from the primary place, when both are positioned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_m: Option<f64>,
}

/// Why two places are correlated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Relation {
    /// Same normalized street + house number + comune (the flagship link).
    SameAddress,
    /// Same brand/chain (shared Wikidata QID).
    SameBrand,
    /// Same OSM building / addressed feature.
    SameBuilding,
    /// Same category within a small radius.
    NearbyCategory,
}

/// One field's provenance: which source supplied it under which license.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    pub field: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json, to_value};

    use super::*;

    fn sample() -> Place {
        Place {
            id: "wd:Q10285".into(),
            name: "Colosseo".into(),
            names: BTreeMap::from([
                ("it".to_string(), "Colosseo".to_string()),
                ("en".to_string(), "Colosseum".to_string()),
            ]),
            category: Category {
                primary: "tourism.attraction".into(),
                tags: vec!["historic".into()],
            },
            location: LonLat {
                lon: 12.4924,
                lat: 41.8902,
            },
            description: Some("Anfiteatro romano".into()),
            summary: None,
            image: Some(Image {
                url: "https://example/commons/colosseo.jpg".into(),
                license: Some("CC-BY-SA-4.0".into()),
                author: Some("Some Author".into()),
                attribution: Some("Some Author, CC BY-SA 4.0".into()),
                proxied: true,
            }),
            address: Some(Address {
                street: Some("Piazza del Colosseo".into()),
                housenumber: Some("1".into()),
                postcode: Some("00184".into()),
                city: Some("Roma".into()),
            }),
            facets: Facets {
                website: Some("https://example".into()),
                ..Default::default()
            },
            related: vec![Related {
                id: "osm:N123".into(),
                name: "Bar del Colosseo".into(),
                category: Some("catering.cafe".into()),
                relation: Relation::SameAddress,
                location: LonLat {
                    lon: 12.4925,
                    lat: 41.8903,
                },
                distance_m: Some(14.0),
            }],
            provenance: vec![Provenance {
                field: "summary".into(),
                source: "wikipedia".into(),
                license: Some("CC-BY-SA".into()),
                url: Some("https://it.wikipedia.org/wiki/Colosseo".into()),
            }],
        }
    }

    #[test]
    fn place_serializes_with_camel_case_keys() {
        let v = to_value(sample()).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(v["id"], "wd:Q10285");
        assert_eq!(v["location"]["lon"], 12.4924);
        assert_eq!(v["image"]["proxied"], true);
        assert_eq!(v["address"]["housenumber"], "1");
        assert_eq!(v["related"][0]["relation"], "sameAddress");
        assert_eq!(v["provenance"][0]["field"], "summary");
        // names is a localized map.
        assert_eq!(v["names"]["en"], "Colosseum");
        // optional summary omitted when None.
        assert!(!obj.contains_key("summary"));
    }

    #[test]
    fn empty_collections_are_omitted() {
        let p = Place {
            id: "osm:N1".into(),
            name: "Nowhere".into(),
            names: BTreeMap::new(),
            category: Category {
                primary: "place".into(),
                tags: vec![],
            },
            location: LonLat { lon: 0.0, lat: 0.0 },
            description: None,
            summary: None,
            image: None,
            address: None,
            facets: Facets::default(),
            related: vec![],
            provenance: vec![],
        };
        let v = to_value(&p).unwrap();
        let obj = v.as_object().unwrap();
        for absent in [
            "names",
            "description",
            "image",
            "address",
            "facets",
            "related",
            "provenance",
        ] {
            assert!(!obj.contains_key(absent), "{absent} should be omitted");
        }
        assert_eq!(v["category"]["primary"], "place");
    }

    #[test]
    fn relation_wire_forms_are_camel_case() {
        assert_eq!(
            to_value(Relation::SameAddress).unwrap(),
            json!("sameAddress")
        );
        assert_eq!(to_value(Relation::SameBrand).unwrap(), json!("sameBrand"));
        assert_eq!(
            to_value(Relation::SameBuilding).unwrap(),
            json!("sameBuilding")
        );
        assert_eq!(
            to_value(Relation::NearbyCategory).unwrap(),
            json!("nearbyCategory")
        );
    }

    #[test]
    fn place_round_trips() {
        let v = to_value(sample()).unwrap();
        let back: Place = serde_json::from_value(v).unwrap();
        assert_eq!(back.id, "wd:Q10285");
        assert_eq!(back.related[0].relation, Relation::SameAddress);
        assert_eq!(
            back.location,
            LonLat {
                lon: 12.4924,
                lat: 41.8902
            }
        );
    }

    #[test]
    fn facets_is_empty_detects_blank() {
        assert!(Facets::default().is_empty());
        assert!(
            !Facets {
                website: Some("x".into()),
                ..Default::default()
            }
            .is_empty()
        );
        let v: Value = to_value(Place {
            id: "osm:N1".into(),
            name: "x".into(),
            names: BTreeMap::new(),
            category: Category {
                primary: "p".into(),
                tags: vec![],
            },
            location: LonLat { lon: 0.0, lat: 0.0 },
            description: None,
            summary: None,
            image: None,
            address: None,
            facets: Facets {
                diet: vec!["vegan".into()],
                ..Default::default()
            },
            related: vec![],
            provenance: vec![],
        })
        .unwrap();
        // a non-empty facet set is present and carries the diet array.
        assert_eq!(v["facets"]["diet"][0], "vegan");
    }
}
