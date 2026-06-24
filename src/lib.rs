//! `spyair-sdr` — a location-aware RTL-SDR scanner.
//!
//! This crate is being built test-first. Hardware- and network-backed boundaries are modelled
//! as traits; production code uses real implementations, and any backend that cannot run in the
//! current environment returns [`error::Error::NotImplemented`] rather than fabricating data.
//! Mocks exist **only** under `#[cfg(test)]`.
//!
//! Modules landed so far (see issue #2):
//! - [`error`] — crate-wide error type.
//! - [`geo`] — positions, great-circle distance, VHF radio-horizon model.
//! - [`location`] — [`location::LocationSource`] trait + manual / stubbed sources.

pub mod adsb;
pub mod audio;
pub mod dsp;
pub mod error;
pub mod freqdb;
pub mod geo;
pub mod gps;
pub mod ipgeo;
pub mod location;
pub mod planner;
pub mod scanner;
pub mod sdr;
pub mod tui;

pub use error::{Error, Result};
pub use geo::{antenna_horizon_km, radio_horizon_km, GeoPosition};
pub use location::{GpsLocationSource, IpLocationSource, LocationSource, ManualLocationSource};
