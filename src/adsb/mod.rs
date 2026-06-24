//! ADS-B correlation: parse a dump1090 `aircraft.json` snapshot and, given the active ATC
//! frequency, produce a best-effort ranked list of which nearby aircraft is most likely the
//! one transmitting.
//!
//! ## Scope (issue #8)
//! This module is **pure**: it parses an already-fetched JSON document and runs the correlation
//! heuristic over it. The live HTTP fetch from a dump1090 instance is **out of scope** (tracked
//! separately as issue #16) — production wires this module behind a fetch boundary that, until
//! implemented, returns [`crate::error::Error::NotImplemented`]. Everything here operates over a
//! caller-supplied `&str`, so it is fully offline and unit-testable.
//!
//! ## Error handling
//! Parsing returns [`Result<Snapshot, serde_json::Error>`](parse_snapshot). Malformed JSON
//! surfaces the underlying [`serde_json::Error`] — it never panics. We deliberately do **not**
//! add a variant to the crate-wide [`crate::error::Error`] for this (per the issue's file-
//! ownership contract, `src/error.rs` is off-limits); a module-local alias [`ParseError`] is
//! provided for readability.
//!
//! ## Correlation is a heuristic
//! There is no field in `aircraft.json` that says which aircraft keyed the mic. We rank nearby
//! aircraft by plausibility (proximity to the ATC facility, presence of a callsign, freshness)
//! and return **ranked candidates with a score** — never a single certain answer. Ranking is
//! deterministic for a given snapshot (see [`correlate`]).

use crate::freqdb::Channel;
use crate::geo::GeoPosition;
use serde::Deserialize;

/// Alias for the error returned when a snapshot fails to parse.
///
/// Parsing failures are surfaced as the underlying [`serde_json::Error`] rather than being mapped
/// into the crate-wide error type (which this module is not permitted to extend).
pub type ParseError = serde_json::Error;

/// A decoded dump1090 `aircraft.json` document.
///
/// Top-level shape: `{ "now": <unix seconds>, "messages": <count>, "aircraft": [ … ] }`.
/// All fields are optional in practice; missing scalars default sensibly and unknown fields are
/// ignored so future dump1090 versions still parse.
#[derive(Debug, Clone, Deserialize)]
pub struct Snapshot {
    /// The server's notion of "now" as Unix time in seconds (fractional). Defaults to `0.0`.
    #[serde(default)]
    pub now: f64,
    /// Total number of Mode S messages processed by the server. Defaults to `0`.
    #[serde(default)]
    pub messages: u64,
    /// The list of currently-tracked aircraft. Defaults to empty.
    #[serde(default)]
    pub aircraft: Vec<Aircraft>,
}

/// One aircraft entry from `aircraft.json`.
///
/// Fields are frequently absent (an aircraft may be heard on Mode S without ever broadcasting a
/// position or callsign), so almost everything is [`Option`]. Unknown JSON fields are tolerated.
#[derive(Debug, Clone, Deserialize)]
pub struct Aircraft {
    /// 24-bit ICAO address as a lowercase hex string (e.g. `"86d4a1"`). Usually present.
    #[serde(default)]
    pub hex: Option<String>,
    /// Raw callsign / flight number, e.g. `"JAL515 "` — note dump1090 pads with spaces.
    /// Use [`Aircraft::callsign`] to get the trimmed form.
    #[serde(default)]
    pub flight: Option<String>,
    /// Latitude in decimal degrees, if a position has been decoded.
    #[serde(default)]
    pub lat: Option<f64>,
    /// Longitude in decimal degrees, if a position has been decoded.
    #[serde(default)]
    pub lon: Option<f64>,
    /// Barometric altitude in feet. dump1090 also emits the string `"ground"` here; that
    /// non-numeric case deserialises to [`None`].
    #[serde(default, deserialize_with = "de_alt_baro")]
    pub alt_baro: Option<f64>,
    /// True track over ground in degrees, if known.
    #[serde(default)]
    pub track: Option<f64>,
    /// Ground speed in knots, if known.
    #[serde(default)]
    pub gs: Option<f64>,
    /// Seconds since the last message of any type was received for this aircraft.
    #[serde(default)]
    pub seen: Option<f64>,
    /// Seconds since the last *position* message was received for this aircraft.
    #[serde(default)]
    pub seen_pos: Option<f64>,
}

/// Deserialise dump1090's `alt_baro`, which is normally an integer number of feet but may be the
/// string `"ground"`. The string form yields [`None`]; numbers yield `Some(feet)`.
fn de_alt_baro<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Accept any JSON value, then narrow to a finite number; anything else (string, null) -> None.
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_f64().filter(|f| f.is_finite()))
}

/// Feet-to-metres conversion factor (exact, by international definition).
const FEET_TO_METRES: f64 = 0.3048;

impl Aircraft {
    /// The trimmed callsign, if the aircraft is broadcasting one.
    ///
    /// dump1090 right-pads the `flight` field with spaces; we trim it. An all-whitespace or empty
    /// field is treated as absent and returns [`None`].
    pub fn callsign(&self) -> Option<&str> {
        self.flight
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// Barometric altitude converted to metres, if known.
    pub fn alt_m(&self) -> Option<f64> {
        self.alt_baro.map(|ft| ft * FEET_TO_METRES)
    }

    /// The aircraft's decoded position as a [`GeoPosition`], if both `lat` and `lon` are present
    /// and valid. Altitude defaults to `0` m when `alt_baro` is missing.
    ///
    /// Returns [`None`] when there is no position fix or the coordinates are out of range
    /// (so an invalid record is simply excluded rather than raising an error).
    pub fn position(&self) -> Option<GeoPosition> {
        let (lat, lon) = (self.lat?, self.lon?);
        GeoPosition::new(lat, lon, self.alt_m().unwrap_or(0.0)).ok()
    }
}

/// An aircraft that passed the in-range filter, paired with its computed distance to the receiver.
#[derive(Debug, Clone)]
pub struct NearbyAircraft<'a> {
    /// The source record.
    pub aircraft: &'a Aircraft,
    /// Decoded position (guaranteed present for a nearby aircraft).
    pub position: GeoPosition,
    /// Great-circle distance from the receiver, in kilometres.
    pub distance_km: f64,
}

/// Maximum age (`seen`, seconds) for an aircraft to count as "live" by default.
///
/// Records older than this are considered stale and excluded by [`nearby_aircraft`].
pub const DEFAULT_MAX_SEEN_SECS: f64 = 60.0;

/// Select aircraft that have a position, are within the receiver's VHF radio horizon, and are not
/// stale, returning each with its distance to the receiver.
///
/// * Aircraft without a decoded position are excluded (they cannot be range-checked).
/// * "In range" uses [`GeoPosition::within_radio_horizon`], i.e. the line-of-sight horizon between
///   the receiver and the aircraft at its altitude.
/// * Aircraft whose `seen` exceeds `max_seen_secs` are dropped. Pass [`f64::INFINITY`] to disable
///   the staleness filter. A missing `seen` is treated as fresh (`0`).
///
/// The returned vector preserves the snapshot's aircraft order (it is not yet ranked).
pub fn nearby_aircraft<'a>(
    snapshot: &'a Snapshot,
    receiver: &GeoPosition,
    max_seen_secs: f64,
) -> Vec<NearbyAircraft<'a>> {
    snapshot
        .aircraft
        .iter()
        .filter(|ac| ac.seen.unwrap_or(0.0) <= max_seen_secs)
        .filter_map(|ac| {
            let position = ac.position()?;
            if !receiver.within_radio_horizon(&position) {
                return None;
            }
            let distance_km = receiver.distance_km(&position);
            Some(NearbyAircraft {
                aircraft: ac,
                position,
                distance_km,
            })
        })
        .collect()
}

/// Find the active ATC facility for an in-use frequency by matching `freq_hz` against a channel
/// table.
///
/// Returns the first [`Channel`] whose `freq_hz` equals `freq_hz`, or [`None`] if the frequency is
/// not in the table. The borrowed channel stands in for "the facility on this frequency".
pub fn facility_for_freq(channels: &[Channel], freq_hz: i64) -> Option<&Channel> {
    channels.iter().find(|c| c.freq_hz == freq_hz)
}

/// A ranked correlation candidate: a nearby aircraft that *might* be the one transmitting on the
/// active frequency.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// The aircraft's ICAO hex address, if known (a stable identifier even when no callsign).
    pub hex: Option<String>,
    /// The trimmed callsign, if the aircraft is broadcasting one.
    pub callsign: Option<String>,
    /// Distance from the ATC facility (or the receiver, if the facility has no position), in km.
    pub distance_km: f64,
    /// Heuristic plausibility score in `[0, 1]`; higher means more likely. See [`correlate`].
    pub score: f64,
}

/// The result of correlating a snapshot against the active frequency.
#[derive(Debug, Clone, PartialEq)]
pub struct Correlation {
    /// The matched facility's identifier (`Channel::ident`), if the frequency was found and the
    /// facility has one.
    pub facility_ident: Option<String>,
    /// Ranked candidates, most plausible first. Empty if no aircraft are in range.
    pub candidates: Vec<Candidate>,
}

/// Weight given to facility proximity in the heuristic score (the rest comes from having a
/// callsign). Kept explicit so the ranking is auditable.
const PROXIMITY_WEIGHT: f64 = 0.8;
/// Bonus applied when the aircraft is broadcasting a callsign (ATC addresses aircraft by callsign,
/// so a transmitting aircraft almost always has one).
const CALLSIGN_WEIGHT: f64 = 0.2;
/// Reference distance (km) at which the proximity term decays to zero. Beyond this the proximity
/// contribution is clamped to zero rather than going negative.
const PROXIMITY_RANGE_KM: f64 = 400.0;

/// Correlate a snapshot against an active ATC frequency, producing a ranked candidate list.
///
/// Steps:
/// 1. Resolve the facility for `active_freq_hz` via [`facility_for_freq`]. If the facility has a
///    known position it is used as the reference point for proximity; otherwise the `receiver`
///    position is used.
/// 2. Filter aircraft with [`nearby_aircraft`] (`max_seen_secs`).
/// 3. Score each candidate and sort descending.
///
/// ## Scoring (heuristic, in `[0, 1]`)
/// `score = PROXIMITY_WEIGHT · max(0, 1 − d/PROXIMITY_RANGE_KM) + CALLSIGN_WEIGHT · has_callsign`
/// where `d` is the distance from the facility reference point. Closer aircraft that are
/// broadcasting a callsign rank highest.
///
/// ## Determinism
/// Sorting is by descending score, then ascending distance, then ascending `hex`, then ascending
/// callsign — a total order with no reliance on the input order, so the ranking is stable and
/// reproducible for a given snapshot.
///
/// Returns a [`Correlation`] with an empty candidate list when nothing is in range. The frequency
/// not being in the channel table is **not** an error: candidates are still ranked by distance to
/// the receiver, and `facility_ident` is [`None`].
pub fn correlate(
    snapshot: &Snapshot,
    channels: &[Channel],
    active_freq_hz: i64,
    receiver: &GeoPosition,
    max_seen_secs: f64,
) -> Correlation {
    let facility = facility_for_freq(channels, active_freq_hz);

    // Reference point for proximity: the facility's position if known, else the receiver.
    let reference = facility.and_then(facility_position).unwrap_or(*receiver);

    let mut candidates: Vec<Candidate> = nearby_aircraft(snapshot, receiver, max_seen_secs)
        .into_iter()
        .map(|near| {
            let distance_km = reference.distance_km(&near.position);
            let proximity = (1.0 - distance_km / PROXIMITY_RANGE_KM).max(0.0);
            let has_callsign = near.aircraft.callsign().is_some();
            let score =
                PROXIMITY_WEIGHT * proximity + CALLSIGN_WEIGHT * f64::from(u8::from(has_callsign));
            Candidate {
                hex: near.aircraft.hex.clone(),
                callsign: near.aircraft.callsign().map(str::to_owned),
                distance_km,
                score,
            }
        })
        .collect();

    candidates.sort_by(cmp_candidates);

    Correlation {
        facility_ident: facility.and_then(|c| c.ident.clone()),
        candidates,
    }
}

/// The facility's position as a [`GeoPosition`], if it has lat/lon. Elevation defaults to `0` m.
fn facility_position(channel: &Channel) -> Option<GeoPosition> {
    let (lat, lon) = (channel.lat?, channel.lon?);
    GeoPosition::new(lat, lon, channel.elev_m.unwrap_or(0.0)).ok()
}

/// Total ordering used to rank candidates: descending score, then ascending distance, then
/// ascending `hex`, then ascending callsign. The tie-breakers make the order deterministic even
/// when scores collide.
fn cmp_candidates(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    // Descending score. NaN should not occur (inputs are finite) but order it last defensively.
    b.score
        .partial_cmp(&a.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| {
            a.distance_km
                .partial_cmp(&b.distance_km)
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| a.hex.cmp(&b.hex))
        .then_with(|| a.callsign.cmp(&b.callsign))
}

/// Parse a dump1090 `aircraft.json` document.
///
/// # Errors
/// Returns the underlying [`serde_json::Error`] (aliased as [`ParseError`]) if `json` is not valid
/// JSON or does not match the expected top-level shape. Never panics. Missing aircraft fields are
/// tolerated (see [`Aircraft`]); unknown fields are ignored.
pub fn parse_snapshot(json: &str) -> Result<Snapshot, ParseError> {
    serde_json::from_str(json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::freqdb::{Mode, Source};

    /// A realistic-but-trimmed dump1090 `aircraft.json` over the Tokyo area.
    /// Mix of: full record, no-callsign record, no-position record, `"ground"` altitude,
    /// stale record, and unknown extra fields.
    const FIXTURE: &str = r#"{
        "now": 1718900000.0,
        "messages": 123456,
        "aircraft": [
            {
                "hex": "86d4a1",
                "flight": "JAL515 ",
                "lat": 35.560,
                "lon": 139.790,
                "alt_baro": 3000,
                "track": 90.0,
                "gs": 250.0,
                "seen": 1.2,
                "seen_pos": 1.0,
                "rssi": -12.3,
                "category": "A3"
            },
            {
                "hex": "86c2b9",
                "flight": "ANA12  ",
                "lat": 35.700,
                "lon": 139.900,
                "alt_baro": 8000,
                "track": 270.0,
                "gs": 300.0,
                "seen": 0.8
            },
            {
                "hex": "abcdef",
                "lat": 35.553,
                "lon": 139.781,
                "alt_baro": "ground",
                "seen": 0.5
            },
            {
                "hex": "111111",
                "flight": "NOPOS1 ",
                "alt_baro": 12000,
                "seen": 2.0
            },
            {
                "hex": "222222",
                "flight": "FARAWAY",
                "lat": 0.0,
                "lon": 0.0,
                "alt_baro": 35000,
                "seen": 1.0
            },
            {
                "hex": "333333",
                "flight": "STALE99",
                "lat": 35.561,
                "lon": 139.791,
                "alt_baro": 3000,
                "seen": 600.0
            }
        ]
    }"#;

    /// dump1090 over Haneda (RJTT), receiver = a rooftop near the airport.
    fn rjtt_receiver() -> GeoPosition {
        GeoPosition::new(35.5494, 139.7798, 15.0).unwrap()
    }

    /// A channel table with the Haneda Tower frequency and an unrelated one.
    fn channels() -> Vec<Channel> {
        vec![
            Channel {
                freq_hz: 118_100_000,
                mode: Mode::Am,
                service: "TWR".into(),
                ident: Some("RJTT".into()),
                desc_en: Some("Haneda Tower".into()),
                desc_jp: Some("東京タワー".into()),
                lat: Some(35.5494),
                lon: Some(139.7798),
                elev_m: Some(6.0),
                priority: 10,
                source: Source::OurAirports,
            },
            Channel {
                freq_hz: 121_500_000,
                mode: Mode::Am,
                service: "EMERG".into(),
                ident: None,
                desc_en: None,
                desc_jp: None,
                lat: None,
                lon: None,
                elev_m: None,
                priority: 1,
                source: Source::Builtin,
            },
        ]
    }

    #[test]
    fn parses_top_level_and_records() {
        let snap = parse_snapshot(FIXTURE).expect("fixture should parse");
        assert_eq!(snap.messages, 123456);
        assert!((snap.now - 1718900000.0).abs() < 1e-6);
        assert_eq!(snap.aircraft.len(), 6);

        let jal = &snap.aircraft[0];
        assert_eq!(jal.hex.as_deref(), Some("86d4a1"));
        assert_eq!(jal.callsign(), Some("JAL515"), "flight must be trimmed");
        assert_eq!(jal.lat, Some(35.560));
        assert_eq!(jal.alt_baro, Some(3000.0));
        // 3000 ft -> ~914.4 m
        assert!((jal.alt_m().unwrap() - 914.4).abs() < 0.1);
    }

    #[test]
    fn handles_missing_callsign_and_position() {
        let snap = parse_snapshot(FIXTURE).unwrap();

        // "abcdef" has a position but no flight -> no callsign, but does have a position.
        let no_cs = &snap.aircraft[2];
        assert_eq!(no_cs.callsign(), None);
        assert!(no_cs.position().is_some());
        // alt_baro was the string "ground" -> None, position altitude defaults to 0.
        assert_eq!(no_cs.alt_baro, None);
        assert_eq!(no_cs.position().unwrap().alt_m, 0.0);

        // "111111" has a callsign but no lat/lon -> no position.
        let no_pos = &snap.aircraft[3];
        assert_eq!(no_pos.callsign(), Some("NOPOS1"));
        assert!(no_pos.position().is_none());
    }

    #[test]
    fn malformed_json_is_error_not_panic() {
        assert!(parse_snapshot("{ this is not json").is_err());
        assert!(parse_snapshot("").is_err());
        // Wrong type for `aircraft` (object instead of array) is also an error.
        assert!(parse_snapshot(r#"{"aircraft": {}}"#).is_err());
    }

    #[test]
    fn empty_and_partial_documents_parse() {
        // A bare object: all defaults kick in.
        let snap = parse_snapshot("{}").unwrap();
        assert_eq!(snap.now, 0.0);
        assert_eq!(snap.messages, 0);
        assert!(snap.aircraft.is_empty());
    }

    #[test]
    fn out_of_range_aircraft_are_excluded() {
        let snap = parse_snapshot(FIXTURE).unwrap();
        let rx = rjtt_receiver();
        let near = nearby_aircraft(&snap, &rx, DEFAULT_MAX_SEEN_SECS);

        let hexes: Vec<&str> = near
            .iter()
            .filter_map(|n| n.aircraft.hex.as_deref())
            .collect();

        // JAL515, ANA12, the no-callsign one are all over Tokyo and high enough to be in horizon.
        assert!(hexes.contains(&"86d4a1"));
        assert!(hexes.contains(&"86c2b9"));
        assert!(hexes.contains(&"abcdef"));
        // FARAWAY is off the coast of Africa (0,0) -> excluded.
        assert!(
            !hexes.contains(&"222222"),
            "out-of-range aircraft must be excluded"
        );
        // NOPOS1 has no position -> excluded.
        assert!(
            !hexes.contains(&"111111"),
            "positionless aircraft must be excluded"
        );
    }

    #[test]
    fn stale_aircraft_dropped_by_default_but_kept_when_disabled() {
        let snap = parse_snapshot(FIXTURE).unwrap();
        let rx = rjtt_receiver();

        let fresh = nearby_aircraft(&snap, &rx, DEFAULT_MAX_SEEN_SECS);
        assert!(!fresh
            .iter()
            .any(|n| n.aircraft.hex.as_deref() == Some("333333")));

        let all = nearby_aircraft(&snap, &rx, f64::INFINITY);
        assert!(all
            .iter()
            .any(|n| n.aircraft.hex.as_deref() == Some("333333")));
    }

    #[test]
    fn frequency_maps_to_facility() {
        let chans = channels();
        let twr = facility_for_freq(&chans, 118_100_000).expect("tower freq present");
        assert_eq!(twr.ident.as_deref(), Some("RJTT"));
        assert_eq!(twr.service, "TWR");

        assert!(facility_for_freq(&chans, 999_000_000).is_none());
    }

    #[test]
    fn correlation_ranking_is_deterministic() {
        let snap = parse_snapshot(FIXTURE).unwrap();
        let chans = channels();
        let rx = rjtt_receiver();

        let result = correlate(&snap, &chans, 118_100_000, &rx, DEFAULT_MAX_SEEN_SECS);
        assert_eq!(result.facility_ident.as_deref(), Some("RJTT"));

        // Three in-range, fresh aircraft: 86d4a1 (JAL515), 86c2b9 (ANA12), abcdef (no callsign).
        let order: Vec<Option<&str>> = result.candidates.iter().map(|c| c.hex.as_deref()).collect();
        assert_eq!(order.len(), 3);

        // Deterministic expected order:
        // - abcdef sits ~0.05 km from the tower but has NO callsign (score ~0.8).
        // - JAL515 is ~1.2 km away WITH a callsign (score ~0.8*~0.997 + 0.2 ≈ 0.997).
        // - ANA12 is ~20 km away WITH a callsign (score ~0.8*0.95 + 0.2 ≈ 0.96).
        // So callsign-bearing close aircraft win.
        assert_eq!(order, vec![Some("86d4a1"), Some("86c2b9"), Some("abcdef")]);

        // Scores are sorted descending.
        for pair in result.candidates.windows(2) {
            assert!(pair[0].score >= pair[1].score, "scores must be descending");
        }
        // Top candidate carries its callsign through.
        assert_eq!(result.candidates[0].callsign.as_deref(), Some("JAL515"));
    }

    #[test]
    fn unknown_frequency_still_ranks_by_receiver_distance() {
        let snap = parse_snapshot(FIXTURE).unwrap();
        let chans = channels();
        let rx = rjtt_receiver();

        // 999 MHz isn't in the table: facility_ident is None, but we still rank nearby aircraft.
        let result = correlate(&snap, &chans, 999_000_000, &rx, DEFAULT_MAX_SEEN_SECS);
        assert!(result.facility_ident.is_none());
        assert_eq!(result.candidates.len(), 3);
        // Still deterministic / non-empty.
        assert!(result.candidates[0].score >= result.candidates[1].score);
    }

    #[test]
    fn correlation_empty_when_nothing_in_range() {
        // Receiver in the middle of the Pacific: nothing from the Tokyo fixture is in range.
        let snap = parse_snapshot(FIXTURE).unwrap();
        let rx = GeoPosition::new(0.0, -150.0, 5.0).unwrap();
        let result = correlate(&snap, &channels(), 118_100_000, &rx, DEFAULT_MAX_SEEN_SECS);
        assert!(result.candidates.is_empty());
    }
}
