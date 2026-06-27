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
        let b = BBox {
            min_lon,
            min_lat,
            max_lon,
            max_lat,
        };
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
    fn parses_with_surrounding_whitespace() {
        let b = BBox::parse(" 11.3 , 41.1 , 14.05 , 43.35 ").unwrap();
        assert_eq!(b.min_lon, 11.3);
        assert_eq!(b.min_lat, 41.1);
        assert_eq!(b.max_lon, 14.05);
        assert_eq!(b.max_lat, 43.35);
    }

    #[test]
    fn parses_negative_coords() {
        let b = BBox::parse("-10,-20,-1,-2").unwrap();
        assert_eq!(b.min_lon, -10.0);
        assert_eq!(b.min_lat, -20.0);
        assert_eq!(b.max_lon, -1.0);
        assert_eq!(b.max_lat, -2.0);
    }

    #[test]
    fn parses_at_range_boundaries() {
        let b = BBox::parse("-180,-90,180,90").unwrap();
        assert_eq!(b.min_lon, -180.0);
        assert_eq!(b.min_lat, -90.0);
        assert_eq!(b.max_lon, 180.0);
        assert_eq!(b.max_lat, 90.0);
    }

    #[test]
    fn invalid_too_few_parts() {
        assert!(matches!(BBox::parse("12,42,13"), Err(BBoxError::Invalid)));
    }

    #[test]
    fn invalid_too_many_parts() {
        assert!(matches!(
            BBox::parse("12,42,13,43,99"),
            Err(BBoxError::Invalid)
        ));
    }

    #[test]
    fn invalid_non_numeric() {
        assert!(matches!(
            BBox::parse("12,42,abc,43"),
            Err(BBoxError::Invalid)
        ));
    }

    #[test]
    fn invalid_empty_string() {
        assert!(matches!(BBox::parse(""), Err(BBoxError::Invalid)));
    }

    #[test]
    fn out_of_range_lon_too_high() {
        assert!(matches!(
            BBox::parse("12,42,200,43"),
            Err(BBoxError::OutOfRange)
        ));
    }

    #[test]
    fn out_of_range_lat_too_high() {
        assert!(matches!(
            BBox::parse("12,42,13,91"),
            Err(BBoxError::OutOfRange)
        ));
    }

    #[test]
    fn out_of_range_negative_lon() {
        assert!(matches!(
            BBox::parse("-181,42,13,43"),
            Err(BBoxError::OutOfRange)
        ));
    }

    #[test]
    fn out_of_range_negative_lat() {
        assert!(matches!(
            BBox::parse("12,-91,13,43"),
            Err(BBoxError::OutOfRange)
        ));
    }

    #[test]
    fn degenerate_lon_equal() {
        assert!(matches!(
            BBox::parse("12,42,12,43"),
            Err(BBoxError::Degenerate)
        ));
    }

    #[test]
    fn degenerate_lat_equal() {
        assert!(matches!(
            BBox::parse("12,42,13,42"),
            Err(BBoxError::Degenerate)
        ));
    }

    #[test]
    fn degenerate_lon_inverted() {
        assert!(matches!(
            BBox::parse("13,42,12,43"),
            Err(BBoxError::Degenerate)
        ));
    }

    #[test]
    fn degenerate_lat_inverted() {
        assert!(matches!(
            BBox::parse("12,43,13,42"),
            Err(BBoxError::Degenerate)
        ));
    }

    #[test]
    fn area_deg2_computes_planar_product() {
        let b = BBox::parse("0,0,2,3").unwrap();
        assert!((b.area_deg2() - 6.0).abs() < 1e-9);
    }

    #[test]
    fn serde_round_trip() {
        let b = BBox::parse("11.3,41.1,14.05,43.35").unwrap();
        let json = serde_json::to_string(&b).unwrap();
        let back: BBox = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
    }

    #[test]
    fn error_display_messages() {
        assert_eq!(
            BBoxError::Invalid.to_string(),
            "bbox must be 'minLon,minLat,maxLon,maxLat'"
        );
        assert_eq!(
            BBoxError::OutOfRange.to_string(),
            "bbox coordinates out of WGS84 range"
        );
        assert_eq!(
            BBoxError::Degenerate.to_string(),
            "bbox is degenerate (min must be strictly less than max)"
        );
    }
}
