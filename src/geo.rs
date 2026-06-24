//! Geographic primitives: positions, great-circle distance, and the VHF radio-horizon model.
//!
//! All pure math — no I/O, fully unit-testable.

use crate::error::{Error, Result};

/// Mean Earth radius in kilometres (used for haversine distance).
const EARTH_RADIUS_KM: f64 = 6371.0088;

/// Radio-horizon coefficient for the 4/3-earth (standard atmosphere) approximation,
/// when heights are in metres and the result is in kilometres:
/// `d_km ≈ 4.12 · √h_m`.
const RADIO_HORIZON_K: f64 = 4.12;

/// A point on (or above) the Earth's surface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoPosition {
    /// Latitude in decimal degrees, north positive. Range [-90, 90].
    pub lat: f64,
    /// Longitude in decimal degrees, east positive. Range [-180, 180].
    pub lon: f64,
    /// Height above mean sea level in metres. May be negative (e.g. below sea level).
    pub alt_m: f64,
}

impl GeoPosition {
    /// Construct a validated position.
    ///
    /// # Errors
    /// Returns [`Error::OutOfRange`] if `lat` ∉ [-90, 90] or `lon` ∉ [-180, 180],
    /// or if any coordinate is non-finite (NaN/inf).
    pub fn new(lat: f64, lon: f64, alt_m: f64) -> Result<Self> {
        if !lat.is_finite() || !(-90.0..=90.0).contains(&lat) {
            return Err(Error::OutOfRange {
                field: "lat",
                value: lat.to_string(),
                expected: "-90..=90",
            });
        }
        if !lon.is_finite() || !(-180.0..=180.0).contains(&lon) {
            return Err(Error::OutOfRange {
                field: "lon",
                value: lon.to_string(),
                expected: "-180..=180",
            });
        }
        if !alt_m.is_finite() {
            return Err(Error::OutOfRange {
                field: "alt_m",
                value: alt_m.to_string(),
                expected: "a finite number of metres",
            });
        }
        Ok(Self { lat, lon, alt_m })
    }

    /// Great-circle (haversine) distance to `other`, in kilometres.
    ///
    /// Ignores altitude — this is surface distance along the sphere.
    pub fn distance_km(&self, other: &GeoPosition) -> f64 {
        let (lat1, lat2) = (self.lat.to_radians(), other.lat.to_radians());
        let dlat = (other.lat - self.lat).to_radians();
        let dlon = (other.lon - self.lon).to_radians();

        let a = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
        let c = 2.0 * a.sqrt().asin();
        EARTH_RADIUS_KM * c
    }

    /// Is `other` within the shared VHF radio horizon of `self`?
    ///
    /// Uses both stations' altitudes (see [`radio_horizon_km`]).
    pub fn within_radio_horizon(&self, other: &GeoPosition) -> bool {
        self.distance_km(other) <= radio_horizon_km(self.alt_m, other.alt_m)
    }
}

/// Distance to the radio horizon for a single antenna of height `h_m` metres, in kilometres,
/// under the standard 4/3-earth refraction model: `d ≈ 4.12·√h`.
///
/// Negative or zero heights clamp to 0 km of horizon contribution.
pub fn antenna_horizon_km(h_m: f64) -> f64 {
    if h_m <= 0.0 {
        0.0
    } else {
        RADIO_HORIZON_K * h_m.sqrt()
    }
}

/// Maximum line-of-sight distance in kilometres between two antennas at heights
/// `h_rx_m` and `h_tx_m` (metres): the sum of each antenna's horizon distance.
///
/// `d_km ≈ 4.12·(√h_rx + √h_tx)`.
pub fn radio_horizon_km(h_rx_m: f64, h_tx_m: f64) -> f64 {
    antenna_horizon_km(h_rx_m) + antenna_horizon_km(h_tx_m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn rejects_out_of_range_latitude() {
        assert!(matches!(
            GeoPosition::new(90.1, 0.0, 0.0),
            Err(Error::OutOfRange { field: "lat", .. })
        ));
        assert!(matches!(
            GeoPosition::new(-91.0, 0.0, 0.0),
            Err(Error::OutOfRange { field: "lat", .. })
        ));
    }

    #[test]
    fn rejects_out_of_range_longitude() {
        assert!(matches!(
            GeoPosition::new(0.0, 180.5, 0.0),
            Err(Error::OutOfRange { field: "lon", .. })
        ));
    }

    #[test]
    fn rejects_non_finite() {
        assert!(GeoPosition::new(f64::NAN, 0.0, 0.0).is_err());
        assert!(GeoPosition::new(0.0, 0.0, f64::INFINITY).is_err());
    }

    #[test]
    fn accepts_valid_extremes() {
        assert!(GeoPosition::new(90.0, 180.0, -10.5).is_ok());
        assert!(GeoPosition::new(-90.0, -180.0, 8848.0).is_ok());
    }

    #[test]
    fn haversine_haneda_to_narita() {
        // RJTT Haneda and RJAA Narita are ~60 km apart.
        let haneda = GeoPosition::new(35.5494, 139.7798, 5.0).unwrap();
        let narita = GeoPosition::new(35.7720, 140.3929, 43.0).unwrap();
        let d = haneda.distance_km(&narita);
        assert!(approx(d, 60.0, 5.0), "expected ~60 km, got {d}");
    }

    #[test]
    fn haversine_zero_for_same_point() {
        let p = GeoPosition::new(35.0, 139.0, 0.0).unwrap();
        assert!(approx(p.distance_km(&p), 0.0, 1e-9));
    }

    #[test]
    fn antenna_horizon_clamps_non_positive() {
        assert_eq!(antenna_horizon_km(0.0), 0.0);
        assert_eq!(antenna_horizon_km(-5.0), 0.0);
    }

    #[test]
    fn antenna_horizon_known_value() {
        // √100 = 10, 4.12·10 = 41.2 km
        assert!(approx(antenna_horizon_km(100.0), 41.2, 1e-6));
    }

    #[test]
    fn aircraft_at_altitude_has_large_horizon() {
        // 5 m ground receiver to an airliner at 11 km (FL360-ish).
        let d = radio_horizon_km(5.0, 11_000.0);
        // 4.12·(√5 + √11000) ≈ 4.12·(2.236 + 104.88) ≈ 441 km
        assert!(d > 400.0 && d < 480.0, "expected ~441 km, got {d}");
    }

    #[test]
    fn sea_level_pair_is_short_range() {
        // Two 5 m antennas: 4.12·(√5+√5) ≈ 18.4 km
        let d = radio_horizon_km(5.0, 5.0);
        assert!(d < 25.0, "expected short range, got {d}");
    }

    #[test]
    fn within_radio_horizon_boundary() {
        let rx = GeoPosition::new(35.0, 139.0, 5.0).unwrap();
        // An aircraft directly overhead at 11 km altitude is well within horizon.
        let near = GeoPosition::new(35.1, 139.0, 11_000.0).unwrap();
        assert!(rx.within_radio_horizon(&near));

        // A sea-level station 100 km away is beyond the ~18 km horizon.
        let far = GeoPosition::new(36.0, 139.0, 5.0).unwrap();
        assert!(!rx.within_radio_horizon(&far));
    }
}
