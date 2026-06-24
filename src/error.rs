//! Crate-wide error type.
//!
//! Note the [`Error::NotImplemented`] variant: hardware- and network-backed boundaries that
//! cannot run in the current build/environment return this explicitly. They never silently
//! fabricate data. Mocks that *do* return data live only in `#[cfg(test)]`.

use thiserror::Error;

/// The result type used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// All errors surfaced by `spyair-sdr`.
#[derive(Debug, Error)]
pub enum Error {
    /// A value was outside its valid range (e.g. latitude not in [-90, 90]).
    #[error("invalid {field}: {value} (expected {expected})")]
    OutOfRange {
        /// The name of the offending field.
        field: &'static str,
        /// The rejected value, formatted.
        value: String,
        /// A human description of the acceptable range.
        expected: &'static str,
    },

    /// A capability is intentionally not implemented in this build/environment.
    ///
    /// Used for boundaries that require physical hardware or external services
    /// (RTL-SDR I/O, GPS, audio out, IP geolocation, …). Production code returns
    /// this rather than a fabricated/mocked value.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}
