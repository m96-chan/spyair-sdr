//! The local frequency database: schema, the bilingual service mapping, OurAirports ingestion,
//! and a SQLite store. The Planner queries this DB at runtime; building it is a deterministic,
//! offline transform over public CSV inputs.

pub mod ourairports;
pub mod service;
pub mod store;

pub use service::{describe, ServiceDescription};
pub use store::ChannelStore;

/// Demodulation mode for a channel.
///
/// Stored as text in the DB (`AM`, `NFM`, `WFM`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Amplitude modulation (airband ATC).
    Am,
    /// Narrowband FM (ham, marine, weather radio).
    Nfm,
    /// Wideband FM (broadcast).
    Wfm,
}

impl Mode {
    /// The canonical text representation stored in the DB.
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Am => "AM",
            Mode::Nfm => "NFM",
            Mode::Wfm => "WFM",
        }
    }
}

/// Where a [`Channel`] originated, for attribution and refresh logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// OurAirports (public domain).
    OurAirports,
    /// RepeaterBook (fetched at runtime with the user's API key).
    RepeaterBook,
    /// Built-in static tables (marine VHF, weather radio).
    Builtin,
    /// User-supplied `extra-frequencies.csv`.
    User,
}

impl Source {
    /// The canonical text representation stored in the DB.
    pub fn as_str(self) -> &'static str {
        match self {
            Source::OurAirports => "ourairports",
            Source::RepeaterBook => "repeaterbook",
            Source::Builtin => "builtin",
            Source::User => "user",
        }
    }
}

/// A single tunable channel, matching the `channels` table in the README schema.
#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    /// Centre frequency in hertz.
    pub freq_hz: i64,
    /// Demodulation mode.
    pub mode: Mode,
    /// Service code (e.g. `TWR`, `GND`, `REPEATER`, `MARINE`).
    pub service: String,
    /// Facility/station identifier (e.g. `RJTT`), if known.
    pub ident: Option<String>,
    /// English description.
    pub desc_en: Option<String>,
    /// Japanese description.
    pub desc_jp: Option<String>,
    /// Transmitter/facility latitude, if known.
    pub lat: Option<f64>,
    /// Transmitter/facility longitude, if known.
    pub lon: Option<f64>,
    /// Transmitter/facility elevation in metres, if known.
    pub elev_m: Option<f64>,
    /// Scanner priority (higher = scanned/held with precedence).
    pub priority: i64,
    /// Originating data source.
    pub source: Source,
}

/// Lower bound of the VHF voice airband (118.000 MHz), in hertz.
pub const AIRBAND_MIN_HZ: i64 = 118_000_000;
/// Upper bound of the VHF voice airband (137.000 MHz), in hertz.
pub const AIRBAND_MAX_HZ: i64 = 137_000_000;

impl Channel {
    /// Whether this channel's frequency falls within the inclusive band `[min_hz, max_hz]`.
    pub fn in_band(&self, min_hz: i64, max_hz: i64) -> bool {
        (min_hz..=max_hz).contains(&self.freq_hz)
    }

    /// Whether this channel is in the VHF voice airband (118–137 MHz).
    pub fn is_airband(&self) -> bool {
        self.in_band(AIRBAND_MIN_HZ, AIRBAND_MAX_HZ)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chan(freq_hz: i64) -> Channel {
        Channel {
            freq_hz,
            mode: Mode::Am,
            service: "TWR".into(),
            ident: None,
            desc_en: None,
            desc_jp: None,
            lat: None,
            lon: None,
            elev_m: None,
            priority: 0,
            source: Source::OurAirports,
        }
    }

    #[test]
    fn airband_bounds_are_inclusive() {
        assert!(chan(AIRBAND_MIN_HZ).is_airband());
        assert!(chan(AIRBAND_MAX_HZ).is_airband());
        assert!(chan(118_100_000).is_airband()); // Haneda Tower
    }

    #[test]
    fn out_of_airband_rejected() {
        assert!(!chan(117_999_000).is_airband()); // just below
        assert!(!chan(137_000_001).is_airband()); // just above
        assert!(!chan(243_000_000).is_airband()); // UHF military guard
        assert!(!chan(500_000).is_airband()); // HF
    }

    #[test]
    fn in_band_custom_range() {
        let c = chan(156_800_000); // marine ch16
        assert!(c.in_band(156_000_000, 162_000_000));
        assert!(!c.in_band(AIRBAND_MIN_HZ, AIRBAND_MAX_HZ));
    }
}
