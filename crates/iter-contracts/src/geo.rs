//! A WGS84 bounding box in the wire format `minLon,minLat,maxLon,maxLat`,
//! shared by the offline, tiles, and overlay surfaces.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BBox {
    pub min_lon: f64,
    pub min_lat: f64,
    pub max_lon: f64,
    pub max_lat: f64,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum BBoxError {
    #[error("bbox must be 'minLon,minLat,maxLon,maxLat'")]
    Invalid,
    #[error("bbox coordinates out of WGS84 range")]
    OutOfRange,
    #[error("bbox is degenerate (min must be strictly less than max)")]
    Degenerate,
}

impl BBox {
    /// Parse the `minLon,minLat,maxLon,maxLat` query format and validate it.
    pub fn parse(s: &str) -> Result<BBox, BBoxError> {
        let parts: Vec<f64> = s
            .split(',')
            .map(|p| p.trim().parse::<f64>())
            .collect::<Result<_, _>>()
            .map_err(|_| BBoxError::Invalid)?;
        let [min_lon, min_lat, max_lon, max_lat] = parts[..] else {
            return Err(BBoxError::Invalid);
        };
        let b = BBox { min_lon, min_lat, max_lon, max_lat };
        b.validate()?;
        Ok(b)
    }

    fn validate(&self) -> Result<(), BBoxError> {
        let in_range = (-180.0..=180.0).contains(&self.min_lon)
            && (-180.0..=180.0).contains(&self.max_lon)
            && (-90.0..=90.0).contains(&self.min_lat)
            && (-90.0..=90.0).contains(&self.max_lat);
        if !in_range {
            return Err(BBoxError::OutOfRange);
        }
        if self.min_lon >= self.max_lon || self.min_lat >= self.max_lat {
            return Err(BBoxError::Degenerate);
        }
        Ok(())
    }

    /// Planar area in square degrees — the cheap guard the offline cap uses.
    pub fn area_deg2(&self) -> f64 {
        (self.max_lon - self.min_lon) * (self.max_lat - self.min_lat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lazio_bbox() {
        let b = BBox::parse("11.3,41.1,14.05,43.35").unwrap();
        assert!((b.area_deg2() - 2.75 * 2.25).abs() < 1e-9);
    }

    #[test]
    fn rejects_degenerate_and_out_of_range() {
        assert!(matches!(BBox::parse("12,42,12,43"), Err(BBoxError::Degenerate)));
        assert!(matches!(BBox::parse("12,42,200,43"), Err(BBoxError::OutOfRange)));
        assert!(matches!(BBox::parse("12,42,13"), Err(BBoxError::Invalid)));
    }
}
