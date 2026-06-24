//! Position sources.
//!
//! [`LocationSource`] abstracts *how* we learn the receiver's position. The only fully
//! implemented production source today is [`ManualLocationSource`] (from config). GPS and
//! IP-geolocation sources are declared but return [`Error::NotImplemented`] until their
//! hardware/network backends land (tracked as follow-up issues) — they are **stubs, not
//! mocks**, and never fabricate a position.

use crate::error::{Error, Result};
use crate::geo::GeoPosition;

/// A source of the receiver's current geographic position.
pub trait LocationSource {
    /// Resolve the current position.
    ///
    /// # Errors
    /// Returns an error if the position cannot be determined (e.g. no GPS fix), or
    /// [`Error::NotImplemented`] if the backend is not available in this build.
    fn position(&self) -> Result<GeoPosition>;

    /// A short identifier for diagnostics / UI (e.g. `"manual"`, `"gps"`).
    fn kind(&self) -> &'static str;
}

/// A fixed position supplied by the user (config `lat`/`lon`/`alt_m`).
///
/// This is a real production implementation.
#[derive(Debug, Clone, Copy)]
pub struct ManualLocationSource {
    position: GeoPosition,
}

impl ManualLocationSource {
    /// Build from a validated [`GeoPosition`].
    pub fn new(position: GeoPosition) -> Self {
        Self { position }
    }

    /// Convenience constructor that validates raw coordinates.
    ///
    /// # Errors
    /// Propagates [`GeoPosition::new`] validation errors.
    pub fn from_coords(lat: f64, lon: f64, alt_m: f64) -> Result<Self> {
        Ok(Self::new(GeoPosition::new(lat, lon, alt_m)?))
    }
}

impl LocationSource for ManualLocationSource {
    fn position(&self) -> Result<GeoPosition> {
        Ok(self.position)
    }

    fn kind(&self) -> &'static str {
        "manual"
    }
}

/// GPS-backed source (serial NMEA / `gpsd`). The live byte transport is **not yet implemented** —
/// it requires hardware (issue #12) and returns [`Error::NotImplemented`] rather than a fabricated
/// fix. The NMEA decoding it will feed on *is* implemented and tested: see [`crate::gps`]
/// ([`crate::gps::parse_sentence`], [`crate::gps::position_from_nmea`],
/// [`crate::gps::resolve_position`] over a [`crate::gps::NmeaSource`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct GpsLocationSource;

impl LocationSource for GpsLocationSource {
    fn position(&self) -> Result<GeoPosition> {
        Err(Error::NotImplemented(
            "GPS location source (serial NMEA / gpsd) requires hardware",
        ))
    }

    fn kind(&self) -> &'static str {
        "gps"
    }
}

/// IP-geolocation fallback source. **Not yet implemented** — requires a runtime network lookup.
///
/// Returns [`Error::NotImplemented`] until the HTTP backend lands. The response-parsing logic and
/// transport trait for IP geolocation live in [`crate::ipgeo`]; only the live HTTP transport
/// ([`crate::ipgeo::HttpIpGeoProvider`]) remains stubbed.
#[derive(Debug, Clone, Copy, Default)]
pub struct IpLocationSource;

impl LocationSource for IpLocationSource {
    fn position(&self) -> Result<GeoPosition> {
        Err(Error::NotImplemented(
            "IP geolocation requires a runtime network lookup",
        ))
    }

    fn kind(&self) -> &'static str {
        "ip"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test-only mock that returns a canned position. Lives under `#[cfg(test)]` so it can
    /// never be wired into production. Demonstrates how downstream consumers can be tested
    /// against the `LocationSource` trait.
    struct MockLocationSource {
        pos: GeoPosition,
    }

    impl LocationSource for MockLocationSource {
        fn position(&self) -> Result<GeoPosition> {
            Ok(self.pos)
        }
        fn kind(&self) -> &'static str {
            "mock"
        }
    }

    #[test]
    fn manual_source_returns_configured_position() {
        let src = ManualLocationSource::from_coords(35.5494, 139.7798, 5.0).unwrap();
        let p = src.position().unwrap();
        assert_eq!(p, GeoPosition::new(35.5494, 139.7798, 5.0).unwrap());
        assert_eq!(src.kind(), "manual");
    }

    #[test]
    fn manual_source_validates_coords() {
        assert!(ManualLocationSource::from_coords(999.0, 0.0, 0.0).is_err());
    }

    #[test]
    fn gps_source_is_not_implemented() {
        assert!(matches!(
            GpsLocationSource.position(),
            Err(Error::NotImplemented(_))
        ));
    }

    #[test]
    fn ip_source_is_not_implemented() {
        assert!(matches!(
            IpLocationSource.position(),
            Err(Error::NotImplemented(_))
        ));
    }

    #[test]
    fn mock_source_usable_only_in_tests() {
        let mock = MockLocationSource {
            pos: GeoPosition::new(0.0, 0.0, 0.0).unwrap(),
        };
        assert_eq!(mock.kind(), "mock");
        assert!(mock.position().is_ok());
    }
}
