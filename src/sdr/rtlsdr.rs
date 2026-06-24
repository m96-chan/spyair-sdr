//! RTL-SDR raw-byte decoding and the RTL-SDR [`SdrSource`] backend.
//!
//! See issue #10. This module splits cleanly into a **testable pure core** and an **honest
//! hardware stub**:
//!
//! - [`decode_rtl_iq`] converts the RTL2832U's native byte stream into normalised [`Iq`] samples.
//!   It is pure (no I/O) and fully unit-tested offline.
//! - [`RtlSdrSource`] is the **default-build** production [`SdrSource`] for a real dongle. Without
//!   the `rtlsdr` Cargo feature there is no librtlsdr binding linked in, so both
//!   [`SdrSource::tune`] and [`SdrSource::read_block`] return [`Error::NotImplemented`] — they
//!   never fabricate samples.
//! - [`RtlSdrDevice`] (only compiled under `--features rtlsdr`) is the **real** librtlsdr-backed
//!   [`SdrSource`]. It opens a physical dongle, sets the sample rate, tunes, and reads raw bytes
//!   synchronously, decoding them through the same pure [`decode_rtl_iq`] above. All samples come
//!   from the hardware; nothing is fabricated.
//!
//! # Binding crate
//! The real path uses the [`rtlsdr`](https://crates.io/crates/rtlsdr) crate (v0.1.x), which links
//! the system `librtlsdr` via `#[link(name = "rtlsdr")]` (no `build.rs`, so it relies on the
//! library being discoverable by the linker — true on this host, where `pkg-config --exists
//! librtlsdr` reports the rtl-sdr-blog fork 2.0.1). It was chosen over `rtlsdr_mt` (spawns a
//! reader thread, heavier than needed for synchronous block reads) and `soapysdr` (an extra
//! abstraction layer + the SoapySDR runtime, which is not required for a single RTL-SDR). The
//! binding builds and links cleanly here; no fallback was needed.
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

/// Default sample rate in samples/second (2.048 MS/s — a standard, widely-supported RTL-SDR rate).
pub const DEFAULT_SAMPLE_RATE_HZ: u32 = 2_048_000;

/// Default read block size in **I/Q pairs** (16384 pairs → 32768 raw bytes). A power-of-two block
/// that aligns with librtlsdr's internal USB transfer granularity (multiples of 16384 bytes).
pub const DEFAULT_BLOCK_IQ_PAIRS: usize = 16_384;

/// Convert a number of I/Q **pairs** into the number of raw RTL-SDR **bytes** to request.
///
/// The RTL2832U streams one byte per component and two components (I and Q) per sample, so the
/// byte count is exactly `2 * pairs`. Pure (no I/O); TDD'd so the real backend can rely on it.
#[must_use]
pub const fn iq_pairs_to_bytes(pairs: usize) -> usize {
    pairs * 2
}

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

/// Real librtlsdr-backed [`SdrSource`] for a physical RTL-SDR dongle.
///
/// Only compiled under `--features rtlsdr`. It owns an open `librtlsdr` device handle, configured
/// for synchronous block reads. [`SdrSource::read_block`] reads raw unsigned-8-bit interleaved I/Q
/// bytes from the hardware and decodes them via the pure [`decode_rtl_iq`] — **no sample is ever
/// fabricated**. Gain defaults to **automatic** (the tuner AGC); to drive gain manually call
/// [`RtlSdrDevice::set_manual_gain_tenths_db`], which switches to manual mode and applies a gain
/// expressed in tenths of a dB.
///
/// The FFI path requires hardware and therefore cannot be unit-tested here; it is exercised by a
/// manual hardware smoke test. The byte→IQ math it depends on ([`decode_rtl_iq`],
/// [`iq_pairs_to_bytes`]) is fully unit-tested.
#[cfg(feature = "rtlsdr")]
pub struct RtlSdrDevice {
    device: rtlsdr::RTLSDRDevice,
    block_pairs: usize,
}

#[cfg(feature = "rtlsdr")]
impl RtlSdrDevice {
    /// Open device `index` and configure it for synchronous reads at [`DEFAULT_SAMPLE_RATE_HZ`]
    /// with a [`DEFAULT_BLOCK_IQ_PAIRS`] block and **automatic** gain.
    ///
    /// Returns [`Error::Device`] if the dongle is missing, busy, or rejects configuration. Never
    /// fabricates a device.
    pub fn open(index: u32) -> Result<Self> {
        let index_i32 = i32::try_from(index)
            .map_err(|_| Error::Device(format!("device index {index} out of range")))?;
        let mut device = rtlsdr::open(index_i32)
            .map_err(|e| Error::Device(format!("opening RTL-SDR device {index}: {e}")))?;
        device
            .set_sample_rate(DEFAULT_SAMPLE_RATE_HZ)
            .map_err(|e| Error::Device(format!("setting sample rate: {e}")))?;
        // Automatic gain: hand control to the tuner AGC. Manual gain is opt-in via
        // `set_manual_gain_tenths_db`.
        device
            .set_tuner_gain_mode(false)
            .map_err(|e| Error::Device(format!("enabling automatic gain: {e}")))?;
        // Clear any stale samples buffered by the driver before the first read.
        device
            .reset_buffer()
            .map_err(|e| Error::Device(format!("resetting sample buffer: {e}")))?;
        Ok(Self {
            device,
            block_pairs: DEFAULT_BLOCK_IQ_PAIRS,
        })
    }

    /// Switch to manual gain and set the tuner gain in **tenths of a dB** (e.g. `496` = 49.6 dB).
    ///
    /// The valid values are tuner-specific; an out-of-range value is rejected by the driver and
    /// surfaced as [`Error::Device`].
    pub fn set_manual_gain_tenths_db(&mut self, gain_tenths_db: i32) -> Result<()> {
        self.device
            .set_tuner_gain_mode(true)
            .map_err(|e| Error::Device(format!("enabling manual gain: {e}")))?;
        self.device
            .set_tuner_gain(gain_tenths_db)
            .map_err(|e| Error::Device(format!("setting tuner gain: {e}")))
    }
}

#[cfg(feature = "rtlsdr")]
impl SdrSource for RtlSdrDevice {
    fn tune(&mut self, freq_hz: i64) -> Result<()> {
        let freq = u32::try_from(freq_hz)
            .map_err(|_| Error::Device(format!("centre frequency {freq_hz} Hz out of range")))?;
        self.device
            .set_center_freq(freq)
            .map_err(|e| Error::Device(format!("tuning to {freq_hz} Hz: {e}")))
    }

    fn read_block(&mut self) -> Result<Vec<Iq>> {
        let want_bytes = iq_pairs_to_bytes(self.block_pairs);
        let bytes = self
            .device
            .read_sync(want_bytes)
            .map_err(|e| Error::Device(format!("reading {want_bytes} bytes: {e}")))?;
        Ok(decode_rtl_iq(&bytes))
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

    #[test]
    fn iq_pairs_to_bytes_is_double() {
        assert_eq!(iq_pairs_to_bytes(0), 0);
        assert_eq!(iq_pairs_to_bytes(1), 2);
        assert_eq!(iq_pairs_to_bytes(16_384), 32_768);
    }

    #[test]
    fn default_block_matches_default_byte_count() {
        // The documented default of 16384 pairs must be 32768 raw bytes.
        assert_eq!(iq_pairs_to_bytes(DEFAULT_BLOCK_IQ_PAIRS), 32_768);
    }

    #[test]
    fn default_sample_rate_is_2048k() {
        assert_eq!(DEFAULT_SAMPLE_RATE_HZ, 2_048_000);
    }
}
