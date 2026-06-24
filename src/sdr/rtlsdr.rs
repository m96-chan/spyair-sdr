//! RTL-SDR raw-byte decoding and the RTL-SDR [`SdrSource`] backend.
//!
//! See issue #10. This module splits cleanly into a **testable pure core** and an **honest
//! hardware stub**:
//!
//! - [`decode_rtl_iq`] converts the RTL2832U's native byte stream into normalised [`Iq`] samples.
//!   It is pure (no I/O) and fully unit-tested offline.
//! - [`RtlSdrSource`] is the production [`SdrSource`] for a real dongle. Because no librtlsdr /
//!   hardware is available in this environment, both [`SdrSource::tune`] and
//!   [`SdrSource::read_block`] return [`Error::NotImplemented`] — they never fabricate samples.
//!   The real librtlsdr-backed implementation (behind a future `rtlsdr` Cargo feature) is deferred
//!   to a follow-up issue; the byte→IQ decode above is ready to wire into it.
//!
//! # RTL-SDR sample format
//! The RTL2832U streams **unsigned 8-bit interleaved I/Q** bytes — `[I0, Q0, I1, Q1, …]` — where
//! every component is in `0..=255` and centred on the DC offset **127.5**. Each byte is normalised
//! to roughly `[-1.0, 1.0]` via `(byte - 127.5) / 127.5`.

use crate::dsp::Iq;
use crate::error::{Error, Result};
use crate::scanner::SdrSource;

/// DC offset of the RTL2832U's unsigned-8-bit samples (mid-point of `0..=255`).
const RTL_DC_OFFSET: f32 = 127.5;

/// Decode a raw RTL-SDR byte stream into normalised [`Iq`] samples (pure, no I/O).
///
/// `bytes` is the RTL2832U's native **unsigned 8-bit interleaved I/Q** stream
/// (`[I0, Q0, I1, Q1, …]`). Each byte is mapped to `f32` via `(byte - 127.5) / 127.5`, placing it
/// in roughly `[-1.0, 1.0]`: byte `0` → ≈ `-1.0`, byte `255` → ≈ `+1.0`, and `127`/`128` straddle
/// `0`.
///
/// Only complete I/Q pairs are decoded: the output length is `bytes.len() / 2`. An empty input
/// yields an empty vector, and a trailing odd leftover byte (an incomplete pair) is dropped rather
/// than causing a panic.
#[must_use]
pub fn decode_rtl_iq(bytes: &[u8]) -> Vec<Iq> {
    bytes
        .chunks_exact(2)
        .map(|pair| {
            let i = (f32::from(pair[0]) - RTL_DC_OFFSET) / RTL_DC_OFFSET;
            let q = (f32::from(pair[1]) - RTL_DC_OFFSET) / RTL_DC_OFFSET;
            Iq::new(i, q)
        })
        .collect()
}

/// Production [`SdrSource`] for a physical RTL-SDR dongle. **Stub pending hardware (issue #10).**
///
/// In the default build there is no librtlsdr binding and no dongle, so both [`SdrSource::tune`]
/// and [`SdrSource::read_block`] return [`Error::NotImplemented`] — this type never fabricates
/// samples. The pure byte→IQ decoder [`decode_rtl_iq`] is implemented and ready to feed the real
/// backend once it exists. The real librtlsdr-backed implementation (behind a future `rtlsdr`
/// Cargo feature) is deferred to a follow-up issue.
#[derive(Debug, Clone, Copy, Default)]
pub struct RtlSdrSource;

impl SdrSource for RtlSdrSource {
    fn tune(&mut self, _freq_hz: i64) -> Result<()> {
        Err(Error::NotImplemented(
            "tuning an RTL-SDR requires librtlsdr + a physical dongle (issue #10); the byte→IQ \
             decoder decode_rtl_iq is implemented and ready for the real backend",
        ))
    }

    fn read_block(&mut self) -> Result<Vec<Iq>> {
        Err(Error::NotImplemented(
            "reading from an RTL-SDR requires librtlsdr + a physical dongle (issue #10); the \
             byte→IQ decoder decode_rtl_iq is implemented and ready for the real backend",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tolerance for normalised-sample float comparisons.
    const EPS: f32 = 1e-6;

    #[test]
    fn empty_input_yields_empty_vec() {
        assert!(decode_rtl_iq(&[]).is_empty());
    }

    #[test]
    fn decodes_one_pair() {
        let out = decode_rtl_iq(&[255, 0]);
        assert_eq!(out.len(), 1);
        assert!((out[0].i - 1.0).abs() < EPS);
        assert!((out[0].q - (-1.0)).abs() < EPS);
    }

    #[test]
    fn decodes_multiple_pairs_interleaved() {
        // [I0, Q0, I1, Q1]
        let out = decode_rtl_iq(&[255, 0, 0, 255]);
        assert_eq!(out.len(), 2);
        assert!((out[0].i - 1.0).abs() < EPS);
        assert!((out[0].q + 1.0).abs() < EPS);
        assert!((out[1].i + 1.0).abs() < EPS);
        assert!((out[1].q - 1.0).abs() < EPS);
    }

    #[test]
    fn trailing_odd_byte_is_dropped() {
        // 3 bytes → only 1 complete I/Q pair, leftover byte ignored (no panic).
        let out = decode_rtl_iq(&[255, 0, 200]);
        assert_eq!(out.len(), 1);
        assert!((out[0].i - 1.0).abs() < EPS);
        assert!((out[0].q + 1.0).abs() < EPS);
    }

    #[test]
    fn single_odd_byte_yields_empty() {
        assert!(decode_rtl_iq(&[42]).is_empty());
    }

    #[test]
    fn min_byte_maps_to_negative_one() {
        let out = decode_rtl_iq(&[0, 0]);
        assert!((out[0].i + 1.0).abs() < EPS);
        assert!((out[0].q + 1.0).abs() < EPS);
    }

    #[test]
    fn max_byte_maps_to_positive_one() {
        let out = decode_rtl_iq(&[255, 255]);
        assert!((out[0].i - 1.0).abs() < EPS);
        assert!((out[0].q - 1.0).abs() < EPS);
    }

    #[test]
    fn center_bytes_straddle_zero() {
        // 127 and 128 are the two bytes nearest the 127.5 DC offset; they bracket 0.0.
        let out = decode_rtl_iq(&[127, 128]);
        let below = out[0].i; // (127 - 127.5)/127.5 < 0
        let above = out[0].q; // (128 - 127.5)/127.5 > 0
        assert!(below < 0.0, "byte 127 should map below zero, got {below}");
        assert!(above > 0.0, "byte 128 should map above zero, got {above}");
        // symmetric about zero
        assert!((below + above).abs() < EPS);
    }

    #[test]
    fn output_length_is_half_input() {
        let bytes: Vec<u8> = (0u8..=200).collect();
        let out = decode_rtl_iq(&bytes);
        assert_eq!(out.len(), bytes.len() / 2);
    }

    #[test]
    fn rtlsdr_source_tune_is_not_implemented() {
        let mut src = RtlSdrSource;
        assert!(matches!(
            src.tune(118_000_000),
            Err(Error::NotImplemented(_))
        ));
    }

    #[test]
    fn rtlsdr_source_read_block_is_not_implemented() {
        let mut src = RtlSdrSource;
        assert!(matches!(src.read_block(), Err(Error::NotImplemented(_))));
    }
}
