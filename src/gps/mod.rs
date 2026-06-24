//! GPS positioning via NMEA 0183.
//!
//! See issue #12. The NMEA *parsing* and the fix-resolution pipeline are pure and fully
//! unit-testable here. The actual byte transport — a serial port or a `gpsd` socket — is hardware
//! gated: [`SerialNmeaSource`] returns [`crate::error::Error::NotImplemented`] until a receiver is
//! present, and never fabricates a fix. Tests drive [`resolve_position`] with a `#[cfg(test)]` mock
//! transport.
//!
//! Supported sentences: `GGA` (position + altitude) and `RMC` (position, no altitude), for any
//! talker id (`GP`, `GL`, `GN`, `GA`, …). Sentences must carry a valid `*HH` checksum.

use crate::error::{Error, Result};
use crate::geo::GeoPosition;

/// A decoded position fix from a single NMEA sentence.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NmeaFix {
    /// Latitude in decimal degrees (north positive).
    pub lat: f64,
    /// Longitude in decimal degrees (east positive).
    pub lon: f64,
    /// Altitude above mean sea level in metres, when the sentence carries it (`GGA` only).
    pub alt_m: Option<f64>,
}

impl NmeaFix {
    /// Convert to a validated [`GeoPosition`], defaulting altitude to 0 m when the sentence had
    /// none (e.g. `RMC`).
    pub fn to_position(self) -> Result<GeoPosition> {
        GeoPosition::new(self.lat, self.lon, self.alt_m.unwrap_or(0.0))
    }
}

/// Validate an NMEA `*HH` checksum: XOR of every byte between `$` and `*` must equal the trailing
/// hex value. Returns `false` if the framing or checksum is missing/malformed.
pub fn checksum_valid(sentence: &str) -> bool {
    let body = match sentence.strip_prefix('$') {
        Some(b) => b,
        None => return false,
    };
    let (payload, hex) = match body.split_once('*') {
        Some(parts) => parts,
        None => return false,
    };
    let expected = match u8::from_str_radix(hex.trim(), 16) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let actual = payload.bytes().fold(0u8, |acc, b| acc ^ b);
    actual == expected
}

/// Convert an NMEA `ddmm.mmmm` / `dddmm.mmmm` magnitude plus a hemisphere flag to decimal degrees.
fn dm_to_decimal(value: &str, hemi: &str) -> Option<f64> {
    let raw: f64 = value.parse().ok()?;
    let degrees = (raw / 100.0).trunc();
    let minutes = raw - degrees * 100.0;
    let mut deg = degrees + minutes / 60.0;
    match hemi {
        "N" | "E" => {}
        "S" | "W" => deg = -deg,
        _ => return None,
    }
    Some(deg)
}

/// Parse a single NMEA sentence into a [`NmeaFix`].
///
/// Returns `None` unless the sentence is a checksum-valid `GGA`/`RMC` carrying a *valid* fix
/// (`GGA` quality ≠ 0, `RMC` status `A`).
pub fn parse_sentence(line: &str) -> Option<NmeaFix> {
    let line = line.trim();
    if !checksum_valid(line) {
        return None;
    }
    // Strip the `$` and the `*HH` checksum, then split into fields.
    let body = line.strip_prefix('$')?.split('*').next()?;
    let fields: Vec<&str> = body.split(',').collect();
    let kind = fields.first()?;
    if kind.len() < 3 {
        return None;
    }
    match &kind[kind.len() - 3..] {
        "GGA" => parse_gga(&fields),
        "RMC" => parse_rmc(&fields),
        _ => None,
    }
}

fn parse_gga(f: &[&str]) -> Option<NmeaFix> {
    // 0:type 1:time 2:lat 3:N/S 4:lon 5:E/W 6:quality 7:sats 8:hdop 9:alt 10:M ...
    if f.len() < 10 {
        return None;
    }
    if f[6].trim() == "0" || f[6].trim().is_empty() {
        return None; // no fix
    }
    let lat = dm_to_decimal(f[2], f[3])?;
    let lon = dm_to_decimal(f[4], f[5])?;
    let alt_m = f[9].parse::<f64>().ok();
    Some(NmeaFix { lat, lon, alt_m })
}

fn parse_rmc(f: &[&str]) -> Option<NmeaFix> {
    // 0:type 1:time 2:status 3:lat 4:N/S 5:lon 6:E/W ...
    if f.len() < 7 {
        return None;
    }
    if f[2].trim() != "A" {
        return None; // void / no valid fix
    }
    let lat = dm_to_decimal(f[3], f[4])?;
    let lon = dm_to_decimal(f[5], f[6])?;
    Some(NmeaFix {
        lat,
        lon,
        alt_m: None,
    })
}

/// Resolve a [`GeoPosition`] from a block of NMEA text (one sentence per line).
///
/// Uses the **last** valid fix in the stream (the most recent), filling altitude with 0 m if that
/// fix lacked it (e.g. an `RMC`). Returns `None` if no valid fix is present.
pub fn position_from_nmea(text: &str) -> Option<GeoPosition> {
    let mut last: Option<NmeaFix> = None;
    for line in text.lines() {
        if let Some(fix) = parse_sentence(line) {
            last = Some(fix);
        }
    }
    last.and_then(|f| f.to_position().ok())
}

/// A transport that yields raw NMEA sentences. The production implementation is hardware-backed
/// (serial / `gpsd`); see [`SerialNmeaSource`]. Tests use a `#[cfg(test)]`-only mock.
pub trait NmeaSource {
    /// Read the next sentence, or `None` at end of stream.
    fn next_sentence(&mut self) -> Result<Option<String>>;
}

/// Resolve the first valid [`GeoPosition`] from an [`NmeaSource`], reading sentences until one
/// yields a fix or the stream ends.
pub fn resolve_position<S: NmeaSource>(source: &mut S) -> Result<Option<GeoPosition>> {
    while let Some(line) = source.next_sentence()? {
        if let Some(fix) = parse_sentence(&line) {
            return Ok(Some(fix.to_position()?));
        }
    }
    Ok(None)
}

/// Live serial / `gpsd` NMEA transport. **Not implemented** — requires a GPS receiver (tracked in
/// #12). Returns [`Error::NotImplemented`] rather than a fabricated sentence.
#[derive(Debug, Clone, Default)]
pub struct SerialNmeaSource {
    /// Serial device path / `gpsd` endpoint (e.g. `/dev/ttyUSB0`), for the future real impl.
    pub endpoint: String,
}

impl NmeaSource for SerialNmeaSource {
    fn next_sentence(&mut self) -> Result<Option<String>> {
        Err(Error::NotImplemented(
            "GPS serial/gpsd transport requires a receiver; feed NMEA via parse_sentence/position_from_nmea instead",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real-world style sentences (valid checksums).
    const GGA: &str = "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47";
    const RMC: &str = "$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*6A";

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn checksum_validation() {
        assert!(checksum_valid(GGA));
        assert!(checksum_valid(RMC));
        // corrupt a byte → checksum fails
        assert!(!checksum_valid(
            "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*48"
        ));
        assert!(!checksum_valid("no dollar or star"));
    }

    #[test]
    fn parses_gga_with_altitude() {
        let fix = parse_sentence(GGA).expect("valid GGA");
        assert!(approx(fix.lat, 48.1173), "lat {}", fix.lat);
        assert!(approx(fix.lon, 11.5167), "lon {}", fix.lon);
        assert_eq!(fix.alt_m, Some(545.4));
    }

    #[test]
    fn parses_rmc_without_altitude() {
        let fix = parse_sentence(RMC).expect("valid RMC");
        assert!(approx(fix.lat, 48.1173));
        assert!(approx(fix.lon, 11.5167));
        assert_eq!(fix.alt_m, None);
        // to_position defaults altitude to 0
        assert_eq!(fix.to_position().unwrap().alt_m, 0.0);
    }

    #[test]
    fn southern_western_hemispheres_are_negative() {
        // Sydney-ish: 33°S, 151°E
        let s = "$GPGGA,000000,3352.000,S,15112.000,E,1,08,0.9,10.0,M,0.0,M,,*5E";
        let fix = parse_sentence(s).expect("valid");
        assert!(fix.lat < 0.0, "S latitude should be negative: {}", fix.lat);
        assert!(fix.lon > 0.0);
    }

    #[test]
    fn rejects_invalid_checksum_and_no_fix() {
        // bad checksum
        assert!(parse_sentence(
            "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*00"
        )
        .is_none());
        // GGA fix quality 0 → no fix
        assert!(parse_sentence(&with_checksum(
            "GPGGA,123519,4807.038,N,01131.000,E,0,00,99.9,545.4,M,46.9,M,,"
        ))
        .is_none());
        // RMC void
        assert!(parse_sentence(&with_checksum(
            "GPRMC,123519,V,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W"
        ))
        .is_none());
        // unrelated sentence type
        assert!(parse_sentence(&with_checksum("GPGSV,3,1,11,03,03,111,00")).is_none());
    }

    #[test]
    fn position_from_nmea_takes_last_valid_fix() {
        let stream = format!(
            "garbage line\n{}\n{}\n",
            with_checksum("GPGGA,000000,3352.000,S,15112.000,E,1,08,0.9,10.0,M,0.0,M,,"),
            GGA, // Munich last → wins
        );
        let pos = position_from_nmea(&stream).expect("a fix");
        assert!(approx(pos.lat, 48.1173));
        assert_eq!(pos.alt_m, 545.4);

        assert!(position_from_nmea("nothing valid here\n$GPGSV,1,1,00*79").is_none());
    }

    /// A `#[cfg(test)]`-only transport scripting NMEA lines. Never compiled into a release build.
    struct MockNmeaSource {
        lines: std::collections::VecDeque<String>,
    }
    impl MockNmeaSource {
        fn new(lines: &[&str]) -> Self {
            Self {
                lines: lines.iter().map(|s| s.to_string()).collect(),
            }
        }
    }
    impl NmeaSource for MockNmeaSource {
        fn next_sentence(&mut self) -> Result<Option<String>> {
            Ok(self.lines.pop_front())
        }
    }

    #[test]
    fn resolve_position_reads_until_first_fix() {
        let mut src = MockNmeaSource::new(&["junk", RMC, GGA]);
        let pos = resolve_position(&mut src).unwrap().expect("fix");
        assert!(approx(pos.lat, 48.1173));
        // stopped at RMC (first valid) → altitude defaults to 0
        assert_eq!(pos.alt_m, 0.0);
    }

    #[test]
    fn resolve_position_none_when_no_fix() {
        let mut src = MockNmeaSource::new(&["junk", "more junk"]);
        assert_eq!(resolve_position(&mut src).unwrap(), None);
    }

    #[test]
    fn serial_transport_is_not_implemented() {
        let mut src = SerialNmeaSource::default();
        assert!(matches!(src.next_sentence(), Err(Error::NotImplemented(_))));
    }

    /// Helper: append the correct `*HH` checksum to a bare sentence body (sans `$`).
    fn with_checksum(body: &str) -> String {
        let sum = body.bytes().fold(0u8, |acc, b| acc ^ b);
        format!("${body}*{sum:02X}")
    }
}
