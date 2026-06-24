//! Audio output: the [`AudioSink`] boundary, a real **WAV recorder**, and a hardware-gated live
//! playback stub.
//!
//! See issue #11. Recording demodulated PCM to a WAV file is real, pure I/O and fully testable
//! here. Live playback to a sound card (`cpal`/`rodio`) requires an audio device and is therefore
//! a [`NotImplemented`](crate::error::Error::NotImplemented) stub ([`PlaybackSink`]) until that
//! device is available. A capturing sink for testing consumers lives only under `#[cfg(test)]`.

use std::io::{Seek, SeekFrom, Write};

use crate::error::{Error, Result};

/// PCM stream format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    /// Sample rate in hertz (e.g. 48_000).
    pub sample_rate: u32,
    /// Channel count (1 = mono, 2 = stereo).
    pub channels: u16,
}

impl AudioFormat {
    /// Mono format at `sample_rate` Hz.
    pub fn mono(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            channels: 1,
        }
    }

    /// Bytes per sample frame across all channels, for 16-bit PCM.
    pub fn block_align(self) -> u16 {
        self.channels * 2
    }

    /// Bytes per second, for 16-bit PCM.
    pub fn byte_rate(self) -> u32 {
        self.sample_rate * u32::from(self.block_align())
    }
}

/// A sink for demodulated PCM audio frames (interleaved `f32` in `-1.0..=1.0`).
pub trait AudioSink {
    /// Write a block of interleaved PCM samples.
    fn write_samples(&mut self, samples: &[f32]) -> Result<()>;

    /// The format this sink consumes.
    fn format(&self) -> AudioFormat;
}

/// Convert an `f32` sample in `-1.0..=1.0` to signed 16-bit PCM (clamped).
fn to_i16(sample: f32) -> i16 {
    let clamped = sample.clamp(-1.0, 1.0);
    (clamped * i16::MAX as f32).round() as i16
}

/// Streams 16-bit PCM to a WAV (RIFF) container.
///
/// The RIFF/`data` chunk sizes are written as placeholders up front and patched in [`finalize`]
/// (hence the `Seek` bound), so recording can stream without knowing the length in advance.
///
/// [`finalize`]: WavWriter::finalize
pub struct WavWriter<W: Write + Seek> {
    inner: W,
    format: AudioFormat,
    data_bytes: u32,
}

impl<W: Write + Seek> WavWriter<W> {
    /// Begin a WAV stream, writing the 44-byte header with placeholder sizes.
    pub fn new(mut inner: W, format: AudioFormat) -> Result<Self> {
        write_header(&mut inner, format, 0)?;
        Ok(Self {
            inner,
            format,
            data_bytes: 0,
        })
    }

    /// Finish the stream: patch the RIFF and `data` chunk sizes, flush, and return the writer.
    pub fn finalize(mut self) -> Result<W> {
        // RIFF chunk size at offset 4 = 36 + data_bytes.
        self.inner.seek(SeekFrom::Start(4))?;
        self.inner
            .write_all(&(36 + self.data_bytes).to_le_bytes())?;
        // data chunk size at offset 40.
        self.inner.seek(SeekFrom::Start(40))?;
        self.inner.write_all(&self.data_bytes.to_le_bytes())?;
        self.inner.seek(SeekFrom::End(0))?;
        self.inner.flush()?;
        Ok(self.inner)
    }

    /// Total PCM bytes written so far.
    pub fn data_bytes(&self) -> u32 {
        self.data_bytes
    }
}

impl<W: Write + Seek> AudioSink for WavWriter<W> {
    fn write_samples(&mut self, samples: &[f32]) -> Result<()> {
        for &s in samples {
            self.inner.write_all(&to_i16(s).to_le_bytes())?;
        }
        self.data_bytes = self.data_bytes.saturating_add((samples.len() * 2) as u32);
        Ok(())
    }

    fn format(&self) -> AudioFormat {
        self.format
    }
}

/// Write the 44-byte canonical PCM WAV header.
fn write_header<W: Write>(w: &mut W, format: AudioFormat, data_bytes: u32) -> Result<()> {
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_bytes).to_le_bytes())?;
    w.write_all(b"WAVE")?;
    // fmt chunk
    w.write_all(b"fmt ")?;
    w.write_all(&16u32.to_le_bytes())?; // PCM fmt chunk size
    w.write_all(&1u16.to_le_bytes())?; // audio format = PCM
    w.write_all(&format.channels.to_le_bytes())?;
    w.write_all(&format.sample_rate.to_le_bytes())?;
    w.write_all(&format.byte_rate().to_le_bytes())?;
    w.write_all(&format.block_align().to_le_bytes())?;
    w.write_all(&16u16.to_le_bytes())?; // bits per sample
                                        // data chunk
    w.write_all(b"data")?;
    w.write_all(&data_bytes.to_le_bytes())?;
    Ok(())
}

/// Live playback to a sound card via `cpal`/`rodio`. **Not implemented** — requires an audio output
/// device (tracked in #11). Returns [`Error::NotImplemented`]; it never silently drops audio.
#[derive(Debug, Clone, Copy)]
pub struct PlaybackSink {
    format: AudioFormat,
}

impl PlaybackSink {
    /// Construct a playback sink for the given format (the real backend is not yet wired up).
    pub fn new(format: AudioFormat) -> Self {
        Self { format }
    }
}

impl AudioSink for PlaybackSink {
    fn write_samples(&mut self, _samples: &[f32]) -> Result<()> {
        Err(Error::NotImplemented(
            "live audio playback (cpal/rodio) requires an output device",
        ))
    }

    fn format(&self) -> AudioFormat {
        self.format
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn u32_le(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    }
    fn u16_le(b: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([b[at], b[at + 1]])
    }

    #[test]
    fn format_helpers() {
        let f = AudioFormat {
            sample_rate: 48_000,
            channels: 2,
        };
        assert_eq!(f.block_align(), 4);
        assert_eq!(f.byte_rate(), 48_000 * 4);
        assert_eq!(AudioFormat::mono(8_000).channels, 1);
    }

    #[test]
    fn wav_header_is_well_formed() {
        let buf = Cursor::new(Vec::new());
        let mut w = WavWriter::new(buf, AudioFormat::mono(16_000)).unwrap();
        w.write_samples(&[0.0, 0.5, -0.5, 1.0]).unwrap();
        let out = w.finalize().unwrap().into_inner();

        assert_eq!(&out[0..4], b"RIFF");
        assert_eq!(&out[8..12], b"WAVE");
        assert_eq!(&out[12..16], b"fmt ");
        assert_eq!(u16_le(&out, 20), 1); // PCM
        assert_eq!(u16_le(&out, 22), 1); // mono
        assert_eq!(u32_le(&out, 24), 16_000); // sample rate
        assert_eq!(u16_le(&out, 34), 16); // bits per sample
        assert_eq!(&out[36..40], b"data");

        // 4 samples * 2 bytes = 8 data bytes; RIFF size = 36 + 8.
        assert_eq!(u32_le(&out, 40), 8);
        assert_eq!(u32_le(&out, 4), 36 + 8);
        assert_eq!(out.len(), 44 + 8);
    }

    #[test]
    fn samples_round_trip_through_pcm() {
        let buf = Cursor::new(Vec::new());
        let mut w = WavWriter::new(buf, AudioFormat::mono(8_000)).unwrap();
        let input = [0.0f32, 1.0, -1.0, 0.5];
        w.write_samples(&input).unwrap();
        let out = w.finalize().unwrap().into_inner();

        // Decode the 16-bit PCM payload (starts at byte 44).
        let pcm: Vec<i16> = out[44..]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(pcm.len(), 4);
        assert_eq!(pcm[0], 0);
        assert_eq!(pcm[1], i16::MAX);
        assert_eq!(pcm[2], -i16::MAX); // -1.0 → -32767 (clamped, symmetric)
                                       // 0.5 → ~16384
        assert!((pcm[3] - 16384).abs() <= 1);
    }

    #[test]
    fn clamps_out_of_range_samples() {
        assert_eq!(to_i16(2.0), i16::MAX);
        assert_eq!(to_i16(-2.0), -i16::MAX);
    }

    #[test]
    fn multiple_writes_accumulate() {
        let buf = Cursor::new(Vec::new());
        let mut w = WavWriter::new(buf, AudioFormat::mono(8_000)).unwrap();
        w.write_samples(&[0.0, 0.0]).unwrap();
        w.write_samples(&[0.0]).unwrap();
        assert_eq!(w.data_bytes(), 6);
        let out = w.finalize().unwrap().into_inner();
        assert_eq!(u32_le(&out, 40), 6);
    }

    /// A `#[cfg(test)]`-only sink that captures frames for assertions. Never compiled into a
    /// release build, so it can't be wired into production.
    struct CapturingSink {
        format: AudioFormat,
        captured: Vec<f32>,
    }
    impl AudioSink for CapturingSink {
        fn write_samples(&mut self, samples: &[f32]) -> Result<()> {
            self.captured.extend_from_slice(samples);
            Ok(())
        }
        fn format(&self) -> AudioFormat {
            self.format
        }
    }

    #[test]
    fn capturing_sink_collects_frames_in_tests_only() {
        let mut sink = CapturingSink {
            format: AudioFormat::mono(48_000),
            captured: Vec::new(),
        };
        sink.write_samples(&[0.1, 0.2]).unwrap();
        sink.write_samples(&[0.3]).unwrap();
        assert_eq!(sink.captured, vec![0.1, 0.2, 0.3]);
        assert_eq!(sink.format().sample_rate, 48_000);
    }

    #[test]
    fn playback_sink_is_not_implemented() {
        let mut sink = PlaybackSink::new(AudioFormat::mono(48_000));
        assert!(matches!(
            sink.write_samples(&[0.0]),
            Err(Error::NotImplemented(_))
        ));
        // format() still works (it's just metadata).
        assert_eq!(sink.format().channels, 1);
    }
}
