//! Digital signal processing for the scanner: AM/NFM demodulation, signal-power
//! estimation, and a hysteresis squelch gate.
//!
//! Everything here is **pure math** over in-memory buffers — no device or audio I/O.
//! Hardware boundaries (RTL-SDR capture, audio playback) live elsewhere as traits and
//! are intentionally out of scope for this slice.
//!
//! # Signal model
//! Complex baseband (IQ) samples are modelled by the lightweight [`Iq`] type — a pair of
//! `f32` in-phase / quadrature components. We deliberately avoid pulling in an external
//! complex-number crate: this slice only needs add/subtract/conjugate-multiply and
//! magnitude, all of which [`Iq`] provides.
//!
//! # Units & normalisation
//! - Sample rate is **not** baked into these functions; callers work in samples. Where a
//!   result is "per sample" (e.g. the FM discriminator's phase step) that is stated.
//! - Audio outputs are raw, **unnormalised** `f32` (no automatic gain control). AM output
//!   is the envelope with its DC (carrier) component removed; FM output is the
//!   per-sample phase advance in **radians**, which is proportional to instantaneous
//!   frequency deviation (`Δf = phase_step · sample_rate / (2π)`).

/// A single complex baseband (IQ) sample: in-phase (`i`) and quadrature (`q`).
///
/// This is a minimal, dependency-free stand-in for a generic complex type. It carries
/// exactly the operations the demodulators need.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Iq {
    /// In-phase component.
    pub i: f32,
    /// Quadrature component.
    pub q: f32,
}

impl Iq {
    /// Construct an IQ sample from its in-phase and quadrature components.
    #[must_use]
    pub const fn new(i: f32, q: f32) -> Self {
        Self { i, q }
    }

    /// Squared magnitude `i² + q²` (instantaneous power, linear units).
    #[must_use]
    pub fn norm_sqr(self) -> f32 {
        self.i * self.i + self.q * self.q
    }

    /// Magnitude `√(i² + q²)` (instantaneous amplitude / envelope).
    #[must_use]
    pub fn magnitude(self) -> f32 {
        self.norm_sqr().sqrt()
    }

    /// Complex conjugate `(i, -q)`.
    #[must_use]
    pub fn conj(self) -> Self {
        Self {
            i: self.i,
            q: -self.q,
        }
    }

    /// Complex multiplication `self * other`.
    #[must_use]
    pub fn mul_cplx(self, other: Self) -> Self {
        Self {
            i: self.i * other.i - self.q * other.q,
            q: self.i * other.q + self.q * other.i,
        }
    }
}

/// Estimate signal power (a linear RSSI proxy) as the mean instantaneous power
/// `mean(i² + q²)` over the buffer.
///
/// The estimate is strictly monotonic in input amplitude: scaling every sample by `k`
/// scales the result by `k²`. Returns `0.0` for an empty buffer (no signal observed).
#[must_use]
pub fn signal_power(samples: &[Iq]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s.norm_sqr()).sum();
    sum / samples.len() as f32
}

/// Estimate signal power expressed in **decibels** relative to unit power
/// (`10·log10(power)`).
///
/// Returns [`f32::NEG_INFINITY`] when the linear power is zero. Useful for squelch
/// thresholds expressed in dB; the linear [`signal_power`] is preferred where a finite,
/// monotonic value is needed.
#[must_use]
pub fn signal_power_db(samples: &[Iq]) -> f32 {
    let p = signal_power(samples);
    if p <= 0.0 {
        f32::NEG_INFINITY
    } else {
        10.0 * p.log10()
    }
}

/// AM-demodulate a baseband IQ buffer by envelope detection.
///
/// Each output sample is the envelope `|s|` with the buffer's mean envelope (the DC /
/// carrier term) subtracted, leaving the audio-band modulation centred on zero. Output
/// length equals the input length. An empty input yields an empty output.
///
/// The result is unnormalised; apply gain downstream if required.
#[must_use]
pub fn demodulate_am(samples: &[Iq]) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }
    let envelopes: Vec<f32> = samples.iter().map(|s| s.magnitude()).collect();
    let mean: f32 = envelopes.iter().sum::<f32>() / envelopes.len() as f32;
    envelopes.into_iter().map(|e| e - mean).collect()
}

/// NFM-demodulate a baseband IQ buffer with a quadrature (phase-difference)
/// discriminator.
///
/// For consecutive samples `s[n-1]`, `s[n]` the output is the phase of
/// `s[n] · conj(s[n-1])`, i.e. the per-sample phase advance in **radians**. This is
/// proportional to the instantaneous frequency deviation:
/// `Δf(n) = output[n] · sample_rate / (2π)`.
///
/// The first sample has no predecessor, so the output has `len - 1` samples. Inputs of
/// length 0 or 1 yield an empty output. Samples with (near-)zero magnitude contribute a
/// `0.0` phase step (no defined direction), avoiding spurious spikes.
#[must_use]
pub fn demodulate_nfm(samples: &[Iq]) -> Vec<f32> {
    if samples.len() < 2 {
        return Vec::new();
    }
    samples
        .windows(2)
        .map(|w| {
            let prev = w[0];
            let cur = w[1];
            let product = cur.mul_cplx(prev.conj());
            // `atan2(0, 0)` is defined as 0; guard explicitly so a dead sample reads as
            // "no frequency change" rather than an arbitrary angle.
            if product.norm_sqr() == 0.0 {
                0.0
            } else {
                product.q.atan2(product.i)
            }
        })
        .collect()
}

/// Maximum FFT length used by [`power_spectrum`].
///
/// The window is the largest power of two **≤** the input length, capped here. The cap
/// bounds per-frame work and latency: 4096 bins at typical SDR rates (≈2.4 MS/s) already
/// gives sub-kHz resolution, which is finer than a terminal waterfall can display, so
/// going larger only costs CPU. Documented and enforced in [`power_spectrum`].
const MAX_FFT_LEN: usize = 4096;

/// Noise floor, in dB below the per-frame peak bin, that maps to `0.0` in the normalised
/// spectrum returned by [`power_spectrum`].
///
/// Bins at the peak map to `1.0`; bins at or below `peak - FLOOR_DB` map to `0.0`; the
/// range in between is linear in dB. A value of −80 dB keeps a flat-noise frame visually
/// distinct from a strong-carrier frame (the carrier saturates near `1.0` while noise sits
/// low) without crushing all dynamic range.
const FLOOR_DB: f32 = -80.0;

/// Largest power of two that is `≤ n`, or `0` when `n == 0`.
fn largest_pow2_le(n: usize) -> usize {
    if n == 0 {
        0
    } else {
        // Highest set bit of `n`.
        1usize << (usize::BITS - 1 - n.leading_zeros())
    }
}

/// In-place iterative radix-2 Cooley–Tukey FFT over interleaved complex samples held as
/// `(re, im)` pairs.
///
/// `data.len()` **must** be a power of two (callers in this module guarantee this; lengths
/// `0` and `1` are returned unchanged). The transform is unnormalised:
/// `X[k] = Σ_n x[n]·exp(-j2π k n / N)`. Pure in-memory math — no allocation beyond the
/// twiddle recurrence, no I/O.
fn fft_pow2(data: &mut [(f32, f32)]) {
    let n = data.len();
    if n < 2 {
        return;
    }

    // Bit-reversal permutation.
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            data.swap(i, j);
        }
    }

    // Danielson–Lanczos butterflies, doubling the sub-transform length each stage.
    let mut len = 2usize;
    while len <= n {
        let half = len / 2;
        // Principal twiddle for this stage: exp(-j2π/len).
        let theta = -2.0 * std::f32::consts::PI / len as f32;
        let (wstep_re, wstep_im) = (theta.cos(), theta.sin());
        let mut start = 0usize;
        while start < n {
            // Recurrent twiddle w = exp(-j2π k / len), k = 0..half.
            let (mut w_re, mut w_im) = (1.0f32, 0.0f32);
            for k in 0..half {
                let a = data[start + k];
                let b = data[start + k + half];
                // t = w * b
                let t_re = w_re * b.0 - w_im * b.1;
                let t_im = w_re * b.1 + w_im * b.0;
                data[start + k] = (a.0 + t_re, a.1 + t_im);
                data[start + k + half] = (a.0 - t_re, a.1 - t_im);
                // Advance twiddle: w *= wstep.
                let nw_re = w_re * wstep_re - w_im * wstep_im;
                let nw_im = w_re * wstep_im + w_im * wstep_re;
                w_re = nw_re;
                w_im = nw_im;
            }
            start += len;
        }
        len <<= 1;
    }
}

/// Compute a normalised, display-ready power spectrum of a complex baseband buffer.
///
/// The result has exactly `display_bins` values in `0.0..=1.0`, ordered low→high frequency
/// with **DC (0 Hz) centred** (negative frequencies on the left, positive on the right),
/// suitable for driving a waterfall / EQ display. Returns an empty `Vec` when `iq` is empty
/// or `display_bins == 0`. The mapping is deterministic and NaN/inf-free.
///
/// # Pipeline
/// 1. Pick the FFT length as the largest power of two `≤ iq.len()`, capped at
///    [`MAX_FFT_LEN`]. Only the first `fft_len` samples are used.
/// 2. Apply a **Hann window** before the transform. Tapering the frame edges to zero
///    suppresses spectral leakage (the sidelobes a rectangular window would smear across
///    bins), so a single carrier shows up as one clean peak rather than a broad skirt.
/// 3. Take per-bin magnitude² (linear power) and **fftshift** so DC lands in the middle.
/// 4. Down-bin to `display_bins` columns by averaging contiguous groups of FFT bins. When
///    `display_bins > fft_len` there are fewer source bins than columns, so each FFT bin is
///    **replicated** across the columns that map back to it (nearest-source mapping); the
///    output still has exactly `display_bins` values.
/// 5. Convert each column to dB relative to the peak column and map linearly to `0.0..=1.0`
///    with [`FLOOR_DB`] as the `0.0` point (`peak → 1.0`).
///
/// # Units
/// Frequency ordering is in FFT-bin units; this function is sample-rate agnostic. The
/// caller maps bin index to Hz using the capture sample rate at the display edge.
#[must_use]
pub fn power_spectrum(iq: &[Iq], display_bins: usize) -> Vec<f32> {
    if iq.is_empty() || display_bins == 0 {
        return Vec::new();
    }

    let fft_len = largest_pow2_le(iq.len()).min(MAX_FFT_LEN);
    if fft_len == 0 {
        return Vec::new();
    }

    // Hann-windowed copy of the first `fft_len` samples into (re, im) pairs.
    let mut buf: Vec<(f32, f32)> = Vec::with_capacity(fft_len);
    if fft_len == 1 {
        // Degenerate single-sample window: Hann would zero it; pass it through so a
        // one-sample frame still yields a finite spectrum rather than all-zero.
        buf.push((iq[0].i, iq[0].q));
    } else {
        let denom = (fft_len - 1) as f32;
        for (n, sample) in iq.iter().take(fft_len).enumerate() {
            // Hann: w[n] = 0.5 · (1 − cos(2π n / (N−1))).
            let w = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * n as f32 / denom).cos());
            buf.push((sample.i * w, sample.q * w));
        }
    }

    fft_pow2(&mut buf);

    // Per-bin linear power.
    let power: Vec<f32> = buf.iter().map(|&(re, im)| re * re + im * im).collect();

    // fftshift: rotate so index 0 (DC) moves to the centre. For length N the new order is
    // [N/2 .. N) ++ [0 .. N/2), placing negative freqs left and positive right.
    let mut shifted = vec![0.0f32; fft_len];
    let half = fft_len / 2;
    for (k, &p) in power.iter().enumerate() {
        let dst = (k + (fft_len - half)) % fft_len;
        shifted[dst] = p;
    }

    // Down-bin (or replicate) to exactly `display_bins` columns of mean power.
    let mut columns = vec![0.0f32; display_bins];
    if display_bins <= fft_len {
        for (col, slot) in columns.iter_mut().enumerate() {
            let start = col * fft_len / display_bins;
            let end = ((col + 1) * fft_len / display_bins).max(start + 1);
            let slice = &shifted[start..end.min(fft_len)];
            let mean = slice.iter().sum::<f32>() / slice.len() as f32;
            *slot = mean;
        }
    } else {
        // Fewer source bins than columns: map each column to its nearest source bin.
        for (col, slot) in columns.iter_mut().enumerate() {
            let src = col * fft_len / display_bins;
            *slot = shifted[src.min(fft_len - 1)];
        }
    }

    // dB-relative-to-peak normalisation into 0.0..=1.0, NaN/inf-safe.
    let peak = columns.iter().copied().fold(0.0f32, f32::max);
    if peak <= 0.0 || !peak.is_finite() {
        // All-zero (or non-finite) frame: nothing to show.
        return vec![0.0f32; display_bins];
    }
    let span = -FLOOR_DB; // positive dB span from floor to peak.
    for slot in &mut columns {
        let v = *slot;
        let norm = if v > 0.0 {
            let db = 10.0 * (v / peak).log10(); // ≤ 0, with peak → 0 dB.
            ((db - FLOOR_DB) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        *slot = if norm.is_finite() { norm } else { 0.0 };
    }
    columns
}

/// State of a [`Squelch`] gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SquelchState {
    /// Signal is below threshold; audio is muted.
    Closed,
    /// Signal is above threshold; audio passes.
    Open,
}

/// A squelch (noise gate) with hysteresis.
///
/// The gate **opens** once power rises to or above `open_threshold` and **closes** once
/// power falls to or below `close_threshold`. With `close_threshold < open_threshold`,
/// power hovering between the two thresholds leaves the state unchanged, preventing the
/// rapid open/close *chatter* a single-threshold gate would produce.
///
/// Thresholds are in the same linear units as [`signal_power`]. The gate starts
/// [`SquelchState::Closed`].
#[derive(Debug, Clone, Copy)]
pub struct Squelch {
    open_threshold: f32,
    close_threshold: f32,
    state: SquelchState,
}

impl Squelch {
    /// Construct a squelch from its open and close thresholds (linear power units).
    ///
    /// `close_threshold` should be **strictly less than** `open_threshold` for genuine
    /// hysteresis; if it is not, the thresholds are swapped so the lower value always
    /// governs closing. The gate starts closed. Returns `None` if either threshold is
    /// not finite.
    #[must_use]
    pub fn new(open_threshold: f32, close_threshold: f32) -> Option<Self> {
        if !open_threshold.is_finite() || !close_threshold.is_finite() {
            return None;
        }
        let (open_threshold, close_threshold) = if close_threshold <= open_threshold {
            (open_threshold, close_threshold)
        } else {
            (close_threshold, open_threshold)
        };
        Some(Self {
            open_threshold,
            close_threshold,
            state: SquelchState::Closed,
        })
    }

    /// Feed a new power estimate and return whether the gate is now open.
    ///
    /// Transitions use hysteresis: open at `power >= open_threshold`, close at
    /// `power <= close_threshold`, otherwise hold the current state.
    pub fn update(&mut self, power: f32) -> bool {
        match self.state {
            SquelchState::Closed if power >= self.open_threshold => {
                self.state = SquelchState::Open;
            }
            SquelchState::Open if power <= self.close_threshold => {
                self.state = SquelchState::Closed;
            }
            _ => {}
        }
        self.is_open()
    }

    /// The current gate state.
    #[must_use]
    pub fn state(&self) -> SquelchState {
        self.state
    }

    /// Whether the gate is currently open (audio passing).
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.state == SquelchState::Open
    }

    /// The open threshold (linear power units).
    #[must_use]
    pub fn open_threshold(&self) -> f32 {
        self.open_threshold
    }

    /// The close threshold (linear power units).
    #[must_use]
    pub fn close_threshold(&self) -> f32 {
        self.close_threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// Pearson correlation coefficient between two equal-length signals.
    /// Returns 0.0 if either signal has no variance.
    fn correlation(a: &[f32], b: &[f32]) -> f32 {
        assert_eq!(a.len(), b.len());
        let n = a.len() as f32;
        let mean_a = a.iter().sum::<f32>() / n;
        let mean_b = b.iter().sum::<f32>() / n;
        let mut cov = 0.0;
        let mut var_a = 0.0;
        let mut var_b = 0.0;
        for (&x, &y) in a.iter().zip(b.iter()) {
            let dx = x - mean_a;
            let dy = y - mean_b;
            cov += dx * dy;
            var_a += dx * dx;
            var_b += dy * dy;
        }
        if var_a == 0.0 || var_b == 0.0 {
            return 0.0;
        }
        cov / (var_a.sqrt() * var_b.sqrt())
    }

    // --- Iq primitive -------------------------------------------------------

    #[test]
    fn iq_magnitude_and_norm() {
        let s = Iq::new(3.0, 4.0);
        assert!((s.norm_sqr() - 25.0).abs() < 1e-6);
        assert!((s.magnitude() - 5.0).abs() < 1e-6);
    }

    #[test]
    fn iq_conj_mul_self_is_real_power() {
        let s = Iq::new(0.5, -0.8);
        let p = s.mul_cplx(s.conj());
        assert!((p.i - s.norm_sqr()).abs() < 1e-6);
        assert!(p.q.abs() < 1e-6);
    }

    // --- Power / RSSI -------------------------------------------------------

    #[test]
    fn signal_power_empty_is_zero() {
        assert_eq!(signal_power(&[]), 0.0);
        assert_eq!(signal_power_db(&[]), f32::NEG_INFINITY);
    }

    #[test]
    fn signal_power_matches_mean_norm_sqr() {
        let samples = [Iq::new(1.0, 0.0), Iq::new(0.0, 2.0)];
        // (1 + 4) / 2 = 2.5
        assert!((signal_power(&samples) - 2.5).abs() < 1e-6);
    }

    #[test]
    fn power_estimate_is_monotonic_in_amplitude() {
        // Build a tone at increasing amplitudes; power must increase strictly.
        let n = 256;
        let make = |amp: f32| -> Vec<Iq> {
            (0..n)
                .map(|k| {
                    let ph = 2.0 * PI * 0.05 * k as f32;
                    Iq::new(amp * ph.cos(), amp * ph.sin())
                })
                .collect()
        };
        let amps = [0.1f32, 0.5, 1.0, 2.0, 5.0];
        let mut prev = -1.0;
        for &a in &amps {
            let p = signal_power(&make(a));
            assert!(p > prev, "power not monotonic at amp {a}: {p} <= {prev}");
            prev = p;
        }
    }

    // --- AM demodulation ----------------------------------------------------

    #[test]
    fn am_demod_recovers_modulating_tone() {
        // Baseband AM: a real envelope = carrier(1.0) + m * cos(2π f_m k). Because we
        // synthesise at baseband (carrier already at DC), the IQ magnitude *is* the
        // envelope, so the recovered audio should track cos(2π f_m k).
        let n = 1024;
        let f_m = 0.01; // cycles/sample, low audio tone
        let m = 0.5; // modulation depth
        let mut iq = Vec::with_capacity(n);
        let mut reference = Vec::with_capacity(n);
        for k in 0..n {
            let tone = (2.0 * PI * f_m * k as f32).cos();
            let envelope = 1.0 + m * tone;
            // Put the whole real envelope on I; magnitude == |envelope| == envelope (>0).
            iq.push(Iq::new(envelope, 0.0));
            reference.push(tone);
        }
        let audio = demodulate_am(&iq);
        assert_eq!(audio.len(), n);
        // DC (carrier) is removed → mean near zero.
        let mean: f32 = audio.iter().sum::<f32>() / n as f32;
        assert!(mean.abs() < 1e-3, "carrier DC not removed: {mean}");
        // Recovered audio strongly correlates with the modulating tone.
        let corr = correlation(&audio, &reference);
        assert!(corr > 0.99, "AM correlation too low: {corr}");
    }

    #[test]
    fn am_demod_empty_is_empty() {
        assert!(demodulate_am(&[]).is_empty());
    }

    // --- FM demodulation ----------------------------------------------------

    #[test]
    fn fm_demod_tracks_frequency_deviation() {
        // FM: instantaneous phase = ∫ω dt. With deviation following a tone,
        // dφ/dn = ω_c + k_f·cos(2π f_m n). The discriminator returns dφ/dn, so the
        // (carrier-subtracted) output must track cos(2π f_m n).
        let n = 2048;
        let f_m = 0.005; // modulating tone (cycles/sample)
        let omega_c = 0.3; // carrier offset (rad/sample), well inside ±π
        let k_f = 0.2; // peak deviation (rad/sample)
        let mut iq = Vec::with_capacity(n);
        let mut reference = Vec::with_capacity(n);
        let mut phase = 0.0f32;
        for k in 0..n {
            iq.push(Iq::new(phase.cos(), phase.sin()));
            let tone = (2.0 * PI * f_m * k as f32).cos();
            reference.push(tone);
            // advance instantaneous phase by the current angular frequency
            phase += omega_c + k_f * tone;
        }
        let audio = demodulate_nfm(&iq);
        assert_eq!(audio.len(), n - 1);

        // Mean phase step ≈ carrier offset.
        let mean: f32 = audio.iter().sum::<f32>() / audio.len() as f32;
        assert!(
            (mean - omega_c).abs() < 0.02,
            "FM carrier offset wrong: {mean} vs {omega_c}"
        );

        // Carrier-subtracted output tracks the modulating tone. Align reference to the
        // discriminator output (which corresponds to samples 1..n).
        let ac: Vec<f32> = audio.iter().map(|&x| x - mean).collect();
        let corr = correlation(&ac, &reference[1..]);
        assert!(corr > 0.99, "FM correlation too low: {corr}");

        // Amplitude is proportional to deviation: peak AC ≈ k_f.
        let peak = ac.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
        assert!(
            (peak - k_f).abs() < 0.02,
            "FM deviation amplitude wrong: {peak} vs {k_f}"
        );
    }

    #[test]
    fn fm_demod_short_input_is_empty() {
        assert!(demodulate_nfm(&[]).is_empty());
        assert!(demodulate_nfm(&[Iq::new(1.0, 0.0)]).is_empty());
    }

    #[test]
    fn fm_demod_constant_signal_is_silent() {
        // A pure tone with constant frequency offset → constant phase step, zero AC.
        let n = 64;
        let step = 0.25f32;
        let mut iq = Vec::with_capacity(n);
        let mut phase = 0.0f32;
        for _ in 0..n {
            iq.push(Iq::new(phase.cos(), phase.sin()));
            phase += step;
        }
        let audio = demodulate_nfm(&iq);
        for &x in &audio {
            assert!(
                (x - step).abs() < 1e-3,
                "expected constant step {step}, got {x}"
            );
        }
    }

    // --- Squelch with hysteresis -------------------------------------------

    #[test]
    fn squelch_starts_closed() {
        let sq = Squelch::new(1.0, 0.5).unwrap();
        assert_eq!(sq.state(), SquelchState::Closed);
        assert!(!sq.is_open());
    }

    #[test]
    fn squelch_rejects_non_finite_thresholds() {
        assert!(Squelch::new(f32::NAN, 0.5).is_none());
        assert!(Squelch::new(1.0, f32::INFINITY).is_none());
    }

    #[test]
    fn squelch_swaps_inverted_thresholds() {
        // Passed low-as-open / high-as-close: the lower value must govern closing.
        let sq = Squelch::new(0.5, 1.0).unwrap();
        assert!((sq.open_threshold() - 1.0).abs() < 1e-6);
        assert!((sq.close_threshold() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn squelch_opens_above_open_threshold() {
        let mut sq = Squelch::new(1.0, 0.5).unwrap();
        assert!(!sq.update(0.9)); // below open
        assert!(sq.update(1.0)); // at open threshold → opens
        assert_eq!(sq.state(), SquelchState::Open);
    }

    #[test]
    fn squelch_closes_below_close_threshold() {
        let mut sq = Squelch::new(1.0, 0.5).unwrap();
        sq.update(2.0); // open
        assert!(sq.is_open());
        assert!(!sq.update(0.5)); // at close threshold → closes
        assert_eq!(sq.state(), SquelchState::Closed);
    }

    #[test]
    fn squelch_does_not_chatter_between_thresholds() {
        let mut sq = Squelch::new(1.0, 0.5).unwrap();
        // Start closed; power in the dead-band must NOT open it.
        for &p in &[0.6f32, 0.7, 0.8, 0.9, 0.55, 0.75] {
            assert!(!sq.update(p), "opened while closed in dead-band at {p}");
        }
        // Open it, then hover in the dead-band: it must stay open (no chatter).
        assert!(sq.update(1.5));
        for &p in &[0.9f32, 0.8, 0.7, 0.6, 0.55, 0.95] {
            assert!(sq.update(p), "closed while open in dead-band at {p}");
        }
        // Only crossing the close threshold actually closes it.
        assert!(!sq.update(0.4));
    }

    // --- FFT primitive ------------------------------------------------------

    /// Magnitude of an interleaved (re, im) bin.
    fn bin_mag((re, im): (f32, f32)) -> f32 {
        (re * re + im * im).sqrt()
    }

    #[test]
    fn largest_pow2_le_basics() {
        assert_eq!(largest_pow2_le(0), 0);
        assert_eq!(largest_pow2_le(1), 1);
        assert_eq!(largest_pow2_le(2), 2);
        assert_eq!(largest_pow2_le(3), 2);
        assert_eq!(largest_pow2_le(5), 4);
        assert_eq!(largest_pow2_le(4096), 4096);
        assert_eq!(largest_pow2_le(5000), 4096);
    }

    #[test]
    fn fft_len_one_and_two_are_trivial() {
        // Length 1: unchanged.
        let mut a = [(3.0f32, -2.0f32)];
        fft_pow2(&mut a);
        assert!((a[0].0 - 3.0).abs() < 1e-6 && (a[0].1 + 2.0).abs() < 1e-6);

        // Length 2: X[0] = x0 + x1, X[1] = x0 - x1.
        let mut b = [(1.0f32, 0.0f32), (2.0f32, 0.0f32)];
        fft_pow2(&mut b);
        assert!((b[0].0 - 3.0).abs() < 1e-6 && b[0].1.abs() < 1e-6);
        assert!((b[1].0 + 1.0).abs() < 1e-6 && b[1].1.abs() < 1e-6);
    }

    #[test]
    fn fft_of_impulse_is_flat() {
        // FFT of [1, 0, 0, ...] → every bin has magnitude 1.
        let n = 16;
        let mut data = vec![(0.0f32, 0.0f32); n];
        data[0] = (1.0, 0.0);
        fft_pow2(&mut data);
        for (k, &bin) in data.iter().enumerate() {
            assert!(
                (bin_mag(bin) - 1.0).abs() < 1e-5,
                "bin {k} not unit magnitude: {bin:?}"
            );
        }
    }

    #[test]
    fn fft_of_single_sinusoid_concentrates_in_one_bin() {
        // x[n] = exp(j2π·n/N) is a full-period tone → all energy in bin 1.
        let n = 32;
        let mut data: Vec<(f32, f32)> = (0..n)
            .map(|k| {
                let ph = 2.0 * PI * k as f32 / n as f32;
                (ph.cos(), ph.sin())
            })
            .collect();
        fft_pow2(&mut data);
        let mags: Vec<f32> = data.iter().map(|&b| bin_mag(b)).collect();
        // Bin 1 should hold ~N; all others ~0.
        assert!((mags[1] - n as f32).abs() < 1e-3, "bin 1 = {}", mags[1]);
        for (k, &m) in mags.iter().enumerate() {
            if k != 1 {
                assert!(m < 1e-3, "stray energy in bin {k}: {m}");
            }
        }
    }

    // --- Power spectrum -----------------------------------------------------

    #[test]
    fn power_spectrum_empty_or_zero_bins_is_empty() {
        assert!(power_spectrum(&[], 64).is_empty());
        assert!(power_spectrum(&[Iq::new(1.0, 0.0); 8], 0).is_empty());
    }

    #[test]
    fn power_spectrum_length_matches_display_bins() {
        let iq = vec![Iq::new(0.3, -0.1); 1000];
        for &bins in &[1usize, 7, 64, 128, 333] {
            assert_eq!(power_spectrum(&iq, bins).len(), bins);
        }
    }

    #[test]
    fn power_spectrum_values_are_finite_and_in_unit_range() {
        // Mix of tone + offset; every output must be a real number in [0, 1].
        let n = 1024;
        let iq: Vec<Iq> = (0..n)
            .map(|k| {
                let ph = 2.0 * PI * 0.12 * k as f32;
                Iq::new(ph.cos() + 0.01, ph.sin())
            })
            .collect();
        let spec = power_spectrum(&iq, 100);
        assert_eq!(spec.len(), 100);
        for (k, &v) in spec.iter().enumerate() {
            assert!(v.is_finite(), "non-finite at {k}: {v}");
            assert!((0.0..=1.0).contains(&v), "out of range at {k}: {v}");
        }
    }

    #[test]
    fn power_spectrum_dc_input_peaks_in_centre() {
        // Constant (DC) input → all energy at 0 Hz, which fftshift puts in the centre.
        let n = 512;
        let iq = vec![Iq::new(1.0, 0.0); n];
        let bins = 64;
        let spec = power_spectrum(&iq, bins);
        let (peak_idx, &peak_val) = spec
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        // fftshift centres DC at index fft_len/2 → display bin near the middle.
        assert!(
            (peak_idx as i64 - (bins as i64 / 2)).abs() <= 1,
            "DC peak not centred: idx {peak_idx} of {bins}"
        );
        assert!(
            (peak_val - 1.0).abs() < 1e-3,
            "DC peak not ~1.0: {peak_val}"
        );
    }

    #[test]
    fn power_spectrum_tone_peaks_at_expected_shifted_bin() {
        // A positive-frequency complex tone exp(j2π·f·n). With fftshift, a positive
        // frequency must land to the RIGHT of centre, and its bin should dominate.
        let fft_len = 512usize;
        let bins = 128usize;
        let f = 0.125f32; // cycles/sample → FFT bin = f·fft_len = 64.
        let iq: Vec<Iq> = (0..fft_len)
            .map(|k| {
                let ph = 2.0 * PI * f * k as f32;
                Iq::new(ph.cos(), ph.sin())
            })
            .collect();
        let spec = power_spectrum(&iq, bins);

        // Expected location: FFT bin 64, fftshifted to 64 + fft_len/2 = 320 (mod 512),
        // then down-binned to display bin 320 * bins / fft_len = 80.
        let raw_bin = (f * fft_len as f32) as usize; // 64
        let shifted = (raw_bin + fft_len / 2) % fft_len; // 320
        let expected_col = shifted * bins / fft_len; // 80

        let (peak_idx, &peak_val) = spec
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        assert!(
            (peak_idx as i64 - expected_col as i64).abs() <= 1,
            "tone peak at {peak_idx}, expected ~{expected_col}"
        );
        // Peak is to the right of centre (positive frequency).
        assert!(peak_idx > bins / 2, "positive tone not right of centre");
        // Peak saturates near 1.0; far-away bins are much lower.
        assert!(
            (peak_val - 1.0).abs() < 1e-3,
            "tone peak not ~1.0: {peak_val}"
        );
        let far = spec[(expected_col + bins / 2) % bins];
        assert!(
            far < 0.5,
            "far bin not suppressed: {far} vs peak {peak_val}"
        );
    }

    #[test]
    fn power_spectrum_is_deterministic() {
        let n = 600;
        let iq: Vec<Iq> = (0..n)
            .map(|k| {
                let ph = 2.0 * PI * 0.07 * k as f32;
                Iq::new(ph.cos(), 0.5 * ph.sin())
            })
            .collect();
        let a = power_spectrum(&iq, 80);
        let b = power_spectrum(&iq, 80);
        assert_eq!(a, b);
    }

    #[test]
    fn power_spectrum_more_bins_than_fft_len_replicates() {
        // 4 samples → fft_len 4; ask for 16 columns. Output length must still be 16 and
        // every value finite & in range (replication path).
        let iq = vec![
            Iq::new(1.0, 0.0),
            Iq::new(0.0, 1.0),
            Iq::new(-1.0, 0.0),
            Iq::new(0.0, -1.0),
        ];
        let spec = power_spectrum(&iq, 16);
        assert_eq!(spec.len(), 16);
        for &v in &spec {
            assert!(v.is_finite() && (0.0..=1.0).contains(&v));
        }
    }
}
