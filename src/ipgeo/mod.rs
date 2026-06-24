//! IP-based geolocation: parse an IP-geo service JSON response into a [`GeoPosition`].
//!
//! This mirrors the structure used elsewhere in the crate (e.g. [`crate::location`]): the pure
//! *parsing* logic is fully implemented and unit-tested, while the *transport* (the actual
//! network request) is a trait boundary. The real transport, [`HttpIpGeoProvider`], returns
//! [`Error::NotImplemented`] because a live HTTP lookup cannot run in this build/environment —
//! it never fabricates a body. A `#[cfg(test)]`-only mock drives the resolution path in tests.
//!
//! # Schema
//! The primary target is the [`ip-api.com`](https://ip-api.com) JSON schema:
//! ```json
//! { "status": "success", "lat": 35.6895, "lon": 139.6917,
//!   "city": "Tokyo", "country": "Japan", "query": "1.2.3.4" }
//! ```
//! On `{"status":"fail", ...}` we return an error rather than a fabricated location.
//!
//! # Altitude
//! IP geolocation has no notion of altitude, so the resulting [`GeoPosition`] is built with
//! `alt_m = 0.0`. Downstream the radio-horizon model will therefore treat the receiver as being
//! at ground / sea level. Supply a manual altitude (see [`crate::location::ManualLocationSource`])
//! if a more accurate height is needed.

use crate::error::{Error, Result};
use crate::geo::GeoPosition;
use serde::Deserialize;

/// Errors specific to parsing an IP-geo response.
///
/// Kept module-local (rather than added to the crate-wide [`Error`]) so the IP-geo slice does
/// not perturb the shared error enum. Converted into the crate [`Error`] at the resolution edge.
#[derive(Debug)]
pub enum IpGeoParseError {
    /// The JSON body could not be deserialized into the expected schema.
    Json(serde_json::Error),
    /// The service reported a failure (`{"status":"fail", "message": "..."}`).
    ServiceFailed {
        /// The provider's failure message, if any.
        message: Option<String>,
    },
    /// The response parsed but did not carry usable latitude/longitude fields.
    MissingCoordinates,
}

impl std::fmt::Display for IpGeoParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IpGeoParseError::Json(e) => write!(f, "malformed IP-geo JSON: {e}"),
            IpGeoParseError::ServiceFailed { message } => match message {
                Some(m) => write!(f, "IP-geo service reported failure: {m}"),
                None => write!(f, "IP-geo service reported failure"),
            },
            IpGeoParseError::MissingCoordinates => {
                write!(f, "IP-geo response carried no latitude/longitude")
            }
        }
    }
}

impl std::error::Error for IpGeoParseError {}

impl From<serde_json::Error> for IpGeoParseError {
    fn from(e: serde_json::Error) -> Self {
        IpGeoParseError::Json(e)
    }
}

/// The raw, deserialized shape of an IP-geo response.
///
/// Fields are optional/defaulted so that missing or extra keys are tolerated; the *meaning* is
/// derived in [`parse_position`]. Both the `ip-api.com` schema (`lat`/`lon`) and the
/// `ipinfo.io` schema (`loc: "lat,lon"`) are accepted.
#[derive(Debug, Default, Deserialize)]
struct IpGeoResponse {
    /// `ip-api.com` status field: `"success"` or `"fail"`. Absent on services that omit it
    /// (e.g. `ipinfo.io`), in which case success is inferred from the presence of coordinates.
    #[serde(default)]
    status: Option<String>,
    /// `ip-api.com` failure message.
    #[serde(default)]
    message: Option<String>,
    /// Latitude in decimal degrees (`ip-api.com`).
    #[serde(default)]
    lat: Option<f64>,
    /// Longitude in decimal degrees (`ip-api.com`).
    #[serde(default)]
    lon: Option<f64>,
    /// `ipinfo.io` combined `"lat,lon"` coordinate string.
    #[serde(default)]
    loc: Option<String>,
}

/// Parse an IP-geo service JSON body into a validated [`GeoPosition`] (with `alt_m = 0.0`).
///
/// Supports the `ip-api.com` schema (`lat`/`lon`) and, as a convenience, the `ipinfo.io`
/// `"loc": "lat,lon"` string.
///
/// # Errors
/// - [`IpGeoParseError::Json`] if `body` is not valid JSON for the expected shape.
/// - [`IpGeoParseError::ServiceFailed`] if `status` is `"fail"` — no location is fabricated.
/// - [`IpGeoParseError::MissingCoordinates`] if no usable coordinates are present, or if the
///   parsed coordinates fail [`GeoPosition::new`]'s range validation (so a corrupt body cannot
///   yield a bogus position).
pub fn parse_position(body: &str) -> std::result::Result<GeoPosition, IpGeoParseError> {
    let resp: IpGeoResponse = serde_json::from_str(body)?;

    // Explicit failure status from ip-api.com: never fabricate a location.
    if let Some(status) = resp.status.as_deref() {
        if status.eq_ignore_ascii_case("fail") {
            return Err(IpGeoParseError::ServiceFailed {
                message: resp.message,
            });
        }
    }

    // Prefer explicit lat/lon (ip-api.com); fall back to ipinfo.io's "loc" string.
    let (lat, lon) = match (resp.lat, resp.lon) {
        (Some(lat), Some(lon)) => (lat, lon),
        _ => match resp.loc.as_deref().and_then(parse_loc) {
            Some(pair) => pair,
            None => return Err(IpGeoParseError::MissingCoordinates),
        },
    };

    // IP geo has no altitude → ground level. `GeoPosition::new` validates the lat/lon ranges;
    // surface any range violation as a parse-level error rather than a fabricated coordinate.
    GeoPosition::new(lat, lon, 0.0).map_err(|_| IpGeoParseError::MissingCoordinates)
}

/// Parse an `ipinfo.io`-style `"lat,lon"` string into a coordinate pair.
fn parse_loc(loc: &str) -> Option<(f64, f64)> {
    let (lat, lon) = loc.split_once(',')?;
    let lat = lat.trim().parse::<f64>().ok()?;
    let lon = lon.trim().parse::<f64>().ok()?;
    Some((lat, lon))
}

/// The transport boundary: fetch the raw IP-geo JSON body.
///
/// This is the seam between pure parsing and the network. Production code uses a real
/// implementation ([`HttpIpGeoProvider`]); tests use a `#[cfg(test)]`-only mock.
pub trait IpGeoProvider {
    /// Fetch the raw JSON response body from the IP-geo service.
    ///
    /// # Errors
    /// Returns [`Error::NotImplemented`] for the real transport in this build, or a transport
    /// error from a concrete network implementation once one lands.
    fn fetch(&self) -> Result<String>;
}

/// The real HTTP transport for an IP-geo service. **Not yet implemented.**
///
/// A live HTTP request requires a runtime network lookup and an HTTP client, neither of which is
/// available in this build. [`fetch`](IpGeoProvider::fetch) therefore returns
/// [`Error::NotImplemented`] — it never fabricates a response body. Wire a real HTTP client here
/// (tracked as a follow-up issue) to enable it.
#[derive(Debug, Clone)]
pub struct HttpIpGeoProvider {
    /// The service endpoint URL (e.g. `http://ip-api.com/json`).
    pub endpoint: String,
}

impl HttpIpGeoProvider {
    /// Construct a provider targeting `endpoint`.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }
}

impl Default for HttpIpGeoProvider {
    /// Defaults to the `ip-api.com` JSON endpoint.
    fn default() -> Self {
        Self::new("http://ip-api.com/json")
    }
}

impl IpGeoProvider for HttpIpGeoProvider {
    fn fetch(&self) -> Result<String> {
        Err(Error::NotImplemented(
            "IP geolocation requires a runtime network lookup",
        ))
    }
}

/// Resolve the receiver's position via an IP-geo provider: fetch the body, then parse it.
///
/// # Errors
/// - Whatever [`IpGeoProvider::fetch`] returns (e.g. [`Error::NotImplemented`] for the real
///   transport in this build).
/// - A crate [`Error`] wrapping any [`IpGeoParseError`] (malformed JSON, a `"fail"` status, or
///   out-of-range / missing coordinates). This function returns an error and never a fabricated
///   position.
pub fn resolve_position<P: IpGeoProvider>(provider: &P) -> Result<GeoPosition> {
    let body = provider.fetch()?;
    parse_position(&body).map_err(parse_error_to_crate_error)
}

/// Map a module-local [`IpGeoParseError`] into the crate-wide [`Error`].
///
/// The crate [`Error`] enum is intentionally left untouched for this slice, so parse failures are
/// reported through the existing [`Error::OutOfRange`] variant with descriptive context.
fn parse_error_to_crate_error(e: IpGeoParseError) -> Error {
    Error::OutOfRange {
        field: "ip-geo response",
        value: e.to_string(),
        expected: "a successful IP-geo response with valid coordinates",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test-only mock transport that returns canned JSON. Lives under `#[cfg(test)]` so it can
    /// never be wired into production.
    struct MockIpGeoProvider {
        body: String,
    }

    impl IpGeoProvider for MockIpGeoProvider {
        fn fetch(&self) -> Result<String> {
            Ok(self.body.clone())
        }
    }

    /// A mock transport whose fetch itself fails (e.g. a network error).
    struct FailingProvider;
    impl IpGeoProvider for FailingProvider {
        fn fetch(&self) -> Result<String> {
            Err(Error::NotImplemented("mock transport failure"))
        }
    }

    const IP_API_SUCCESS: &str = r#"{
        "status": "success",
        "lat": 35.6895,
        "lon": 139.6917,
        "city": "Tokyo",
        "country": "Japan",
        "query": "1.2.3.4"
    }"#;

    const IP_API_FAIL: &str = r#"{
        "status": "fail",
        "message": "reserved range",
        "query": "127.0.0.1"
    }"#;

    #[test]
    fn parses_ip_api_success_to_position_with_zero_altitude() {
        let pos = parse_position(IP_API_SUCCESS).unwrap();
        assert_eq!(pos.lat, 35.6895);
        assert_eq!(pos.lon, 139.6917);
        assert_eq!(pos.alt_m, 0.0, "IP geo has no altitude → ground level");
    }

    #[test]
    fn status_fail_returns_error_not_a_location() {
        let err = parse_position(IP_API_FAIL).unwrap_err();
        match err {
            IpGeoParseError::ServiceFailed { message } => {
                assert_eq!(message.as_deref(), Some("reserved range"));
            }
            other => panic!("expected ServiceFailed, got {other:?}"),
        }
    }

    #[test]
    fn malformed_json_is_error_not_panic() {
        let err = parse_position("{ not valid json ]").unwrap_err();
        assert!(matches!(err, IpGeoParseError::Json(_)));
    }

    #[test]
    fn empty_body_is_error() {
        assert!(parse_position("").is_err());
    }

    #[test]
    fn out_of_range_coordinates_are_rejected() {
        // ip-api.com would never return this, but a corrupt/hostile body must not produce a
        // bogus GeoPosition — GeoPosition::new validation rejects it.
        let body = r#"{ "status": "success", "lat": 999.0, "lon": 0.0 }"#;
        assert!(parse_position(body).is_err());
    }

    #[test]
    fn missing_coordinates_is_error() {
        // No lat/lon and no loc string.
        let body = r#"{ "status": "success", "city": "Nowhere" }"#;
        assert!(matches!(
            parse_position(body),
            Err(IpGeoParseError::MissingCoordinates)
        ));
    }

    #[test]
    fn tolerates_missing_and_extra_fields() {
        // No "status" field (ipinfo.io style is statusless) but explicit lat/lon, plus an
        // unrelated extra field.
        let body = r#"{ "lat": 51.5074, "lon": -0.1278, "isp": "Acme", "extra": 42 }"#;
        let pos = parse_position(body).unwrap();
        assert_eq!(pos.lat, 51.5074);
        assert_eq!(pos.lon, -0.1278);
    }

    #[test]
    fn accepts_ipinfo_loc_string() {
        let body = r#"{ "loc": "35.6895,139.6917", "city": "Tokyo" }"#;
        let pos = parse_position(body).unwrap();
        assert_eq!(pos.lat, 35.6895);
        assert_eq!(pos.lon, 139.6917);
    }

    #[test]
    fn malformed_loc_string_is_missing_coordinates() {
        let body = r#"{ "loc": "not-a-pair" }"#;
        assert!(matches!(
            parse_position(body),
            Err(IpGeoParseError::MissingCoordinates)
        ));
    }

    #[test]
    fn resolve_position_with_mock_returns_position() {
        let provider = MockIpGeoProvider {
            body: IP_API_SUCCESS.to_string(),
        };
        let pos = resolve_position(&provider).unwrap();
        assert_eq!(pos, GeoPosition::new(35.6895, 139.6917, 0.0).unwrap());
    }

    #[test]
    fn resolve_position_propagates_service_failure() {
        let provider = MockIpGeoProvider {
            body: IP_API_FAIL.to_string(),
        };
        assert!(resolve_position(&provider).is_err());
    }

    #[test]
    fn resolve_position_propagates_fetch_error() {
        assert!(matches!(
            resolve_position(&FailingProvider),
            Err(Error::NotImplemented(_))
        ));
    }

    #[test]
    fn real_http_provider_is_not_implemented() {
        let provider = HttpIpGeoProvider::default();
        assert!(matches!(provider.fetch(), Err(Error::NotImplemented(_))));
    }

    #[test]
    fn real_http_provider_resolve_is_not_implemented() {
        let provider = HttpIpGeoProvider::new("http://ip-api.com/json");
        assert!(matches!(
            resolve_position(&provider),
            Err(Error::NotImplemented(_))
        ));
        assert_eq!(provider.endpoint, "http://ip-api.com/json");
    }
}
