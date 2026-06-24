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
