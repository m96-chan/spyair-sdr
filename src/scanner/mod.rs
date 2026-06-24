//! Scanner — the channel-hop / hold / priority / lockout state machine.
//!
//! See issue #7. The scanning *logic* is a pure state machine driven by signal power read from a
//! [`SdrSource`] plus a caller-supplied monotonic clock. It is fully unit-testable against a
//! `#[cfg(test)]` mock source; the real hardware-backed `SdrSource` (#10) returns
//! [`crate::error::Error::NotImplemented`] until a dongle is available — never fabricated samples.
//!
//! Flow: hop across the watchlist while idle → open and dwell while a channel is active → hold for
//! a configured time after activity ends → resume hopping. Priority channels pre-empt a
//! lower-priority dwell; locked-out channels are skipped; [`Scanner::skip`] advances immediately.

use std::collections::HashSet;

use crate::dsp::{signal_power, Iq, Squelch};
use crate::error::Result;
use crate::planner::Watchlist;

/// Production boundary for SDR sample acquisition: tune to a frequency and read an IQ block.
///
/// Real implementations require hardware (tracked in #10) and return `NotImplemented` until then.
/// Tests drive the scanner with a `#[cfg(test)]`-only mock — never a production mock.
pub trait SdrSource {
    /// Tune the front-end to `freq_hz`.
    fn tune(&mut self, freq_hz: i64) -> Result<()>;
    /// Read one block of IQ samples at the currently tuned frequency.
    fn read_block(&mut self) -> Result<Vec<Iq>>;
}

/// A channel the scanner rotates through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanChannel {
    /// Centre frequency in hertz.
    pub freq_hz: i64,
    /// Whether this channel is a priority channel (pre-empts lower-priority dwells).
    pub priority: bool,
}

impl ScanChannel {
    /// Construct a scan channel.
    pub fn new(freq_hz: i64, priority: bool) -> Self {
        Self { freq_hz, priority }
    }
}

/// Scanner tuning/squelch/hold configuration.
#[derive(Debug, Clone, Copy)]
pub struct ScannerConfig {
    /// Squelch open threshold (linear signal power).
    pub open_threshold: f32,
    /// Squelch close threshold (linear signal power, `< open_threshold`).
    pub close_threshold: f32,
    /// How long to keep dwelling after a channel goes quiet, in milliseconds.
    pub hold_ms: u64,
}

impl Default for ScannerConfig {
    fn default() -> Self {
        Self {
            open_threshold: 1.0,
            close_threshold: 0.5,
            hold_ms: 2_500,
        }
    }
}

/// The scanner's high-level state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanState {
    /// Hopping across the watchlist looking for activity.
    Scanning,
    /// Dwelling on an active channel (squelch open).
    Active,
    /// Squelch closed but still holding before resuming the scan.
    Holding,
}

/// What happened on a [`Scanner::tick`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanEvent {
    /// Hopped to a new channel while scanning (carries its frequency).
    Tuned(i64),
    /// Squelch broke on the current channel; now dwelling.
    Opened(i64),
    /// A priority channel pre-empted a lower-priority dwell.
    Preempted(i64),
    /// Squelch closed; holding before resuming the scan.
    Holding(i64),
    /// Hold expired; the channel closed and the scan is resuming.
    Closed(i64),
    /// Nothing to scan (empty watchlist or every channel locked out).
    Idle,
}

/// The channel-hopping state machine over a [`SdrSource`].
pub struct Scanner<S: SdrSource> {
    source: S,
    cfg: ScannerConfig,
    channels: Vec<ScanChannel>,
    idx: usize,
    state: ScanState,
    squelch: Squelch,
    lockout: HashSet<i64>,
    hold_start_ms: u64,
    current: Option<i64>,
}

impl<S: SdrSource> Scanner<S> {
    /// Build a scanner over an explicit channel list.
    ///
    /// Tunes to the first non-locked channel. Returns the configured channels in rotation order.
    pub fn new(source: S, cfg: ScannerConfig, channels: Vec<ScanChannel>) -> Result<Self> {
        // `close < open` is required by the squelch; `Default`/callers ensure finite values.
        let squelch = Squelch::new(cfg.open_threshold, cfg.close_threshold)
            .unwrap_or_else(|| Squelch::new(1.0, 0.5).expect("constant thresholds are valid"));
        let mut scanner = Self {
            source,
            cfg,
            channels,
            idx: 0,
            state: ScanState::Scanning,
            squelch,
            lockout: HashSet::new(),
            hold_start_ms: 0,
            current: None,
        };
        scanner.tune_current()?;
        Ok(scanner)
    }

    /// Build a scanner from a planner [`Watchlist`], preserving its rank order and priority flags.
    pub fn from_watchlist(source: S, cfg: ScannerConfig, watchlist: &Watchlist) -> Result<Self> {
        let channels = watchlist
            .iter()
            .map(|e| ScanChannel::new(e.channel.freq_hz, e.is_priority))
            .collect();
        Self::new(source, cfg, channels)
    }

    /// The frequency the scanner is currently parked on, if any.
    pub fn current_freq(&self) -> Option<i64> {
        self.current
    }

    /// The current high-level state.
    pub fn state(&self) -> ScanState {
        self.state
    }

    /// Lock a channel out of the rotation. If it is the current channel, the scan advances.
    pub fn lockout(&mut self, freq_hz: i64) -> Result<()> {
        self.lockout.insert(freq_hz);
        if self.current == Some(freq_hz) {
            self.reset_squelch();
            self.state = ScanState::Scanning;
            self.advance()?;
        }
        Ok(())
    }

    /// Whether a channel is currently locked out.
    pub fn is_locked(&self, freq_hz: i64) -> bool {
        self.lockout.contains(&freq_hz)
    }

    /// Skip the current channel immediately and advance to the next eligible one.
    pub fn skip(&mut self) -> Result<ScanEvent> {
        self.reset_squelch();
        self.state = ScanState::Scanning;
        self.advance()?;
        Ok(match self.current {
            Some(f) => ScanEvent::Tuned(f),
            None => ScanEvent::Idle,
        })
    }

    /// Advance the scanning state machine by one step at logical time `now_ms` (monotonic,
    /// milliseconds). Reads the current channel (and probes priority channels when dwelling),
    /// applies squelch + hold logic, and returns the resulting [`ScanEvent`].
    pub fn tick(&mut self, now_ms: u64) -> Result<ScanEvent> {
        if self.eligible_count() == 0 {
            self.current = None;
            return Ok(ScanEvent::Idle);
        }
        if self.current.is_none() {
            self.tune_current()?;
        }

        // Priority pre-emption: while dwelling/holding on a non-priority channel, probe priority
        // channels; if one is active, switch to it.
        if matches!(self.state, ScanState::Active | ScanState::Holding)
            && !self.current_is_priority()
        {
            if let Some(pf) = self.probe_priority()? {
                self.switch_to(pf)?;
                self.state = ScanState::Active;
                self.hold_start_ms = now_ms;
                return Ok(ScanEvent::Preempted(pf));
            }
            // No priority hit — make sure we are tuned back to the current channel before reading.
            if let Some(cur) = self.current {
                self.source.tune(cur)?;
            }
        }

        let power = self.read_power()?;
        let open = self.squelch.update(power);
        let current = self.current.expect("eligible channel is tuned");

        match self.state {
            ScanState::Scanning => {
                if open {
                    self.state = ScanState::Active;
                    self.hold_start_ms = now_ms;
                    Ok(ScanEvent::Opened(current))
                } else {
                    self.reset_squelch();
                    self.advance()?;
                    Ok(match self.current {
                        Some(f) => ScanEvent::Tuned(f),
                        None => ScanEvent::Idle,
                    })
                }
            }
            ScanState::Active => {
                if open {
                    self.hold_start_ms = now_ms;
                    Ok(ScanEvent::Opened(current))
                } else {
                    self.state = ScanState::Holding;
                    Ok(ScanEvent::Holding(current))
                }
            }
            ScanState::Holding => {
                if open {
                    self.state = ScanState::Active;
                    self.hold_start_ms = now_ms;
                    Ok(ScanEvent::Opened(current))
                } else if now_ms.saturating_sub(self.hold_start_ms) >= self.cfg.hold_ms {
                    self.reset_squelch();
                    self.state = ScanState::Scanning;
                    self.advance()?;
                    Ok(ScanEvent::Closed(current))
                } else {
                    Ok(ScanEvent::Holding(current))
                }
            }
        }
    }

    // --- internals -------------------------------------------------------------------------

    fn eligible_count(&self) -> usize {
        self.channels
            .iter()
            .filter(|c| !self.lockout.contains(&c.freq_hz))
            .count()
    }

    fn current_is_priority(&self) -> bool {
        match self.current {
            Some(f) => self.channels.iter().any(|c| c.freq_hz == f && c.priority),
            None => false,
        }
    }

    fn reset_squelch(&mut self) {
        self.squelch = Squelch::new(self.cfg.open_threshold, self.cfg.close_threshold)
            .unwrap_or_else(|| Squelch::new(1.0, 0.5).expect("constant thresholds are valid"));
    }

    /// Tune to the channel at `idx`, skipping locked-out entries. Sets `current`.
    fn tune_current(&mut self) -> Result<()> {
        if self.channels.is_empty() {
            self.current = None;
            return Ok(());
        }
        // Find the next eligible channel starting at `idx`.
        for offset in 0..self.channels.len() {
            let i = (self.idx + offset) % self.channels.len();
            let ch = self.channels[i];
            if !self.lockout.contains(&ch.freq_hz) {
                self.idx = i;
                self.current = Some(ch.freq_hz);
                self.source.tune(ch.freq_hz)?;
                return Ok(());
            }
        }
        self.current = None;
        Ok(())
    }

    /// Advance `idx` to the next eligible channel and tune to it.
    fn advance(&mut self) -> Result<()> {
        if self.eligible_count() == 0 {
            self.current = None;
            return Ok(());
        }
        self.idx = (self.idx + 1) % self.channels.len();
        // `tune_current` scans forward from `idx` for the next eligible channel.
        self.tune_current()
    }

    /// Switch the current channel to a specific frequency (used by pre-emption).
    fn switch_to(&mut self, freq_hz: i64) -> Result<()> {
        if let Some(i) = self.channels.iter().position(|c| c.freq_hz == freq_hz) {
            self.idx = i;
        }
        self.reset_squelch();
        self.current = Some(freq_hz);
        self.source.tune(freq_hz)?;
        Ok(())
    }

    /// Probe each priority channel (not the current one, not locked out); return the first that is
    /// active (power at/above the open threshold).
    fn probe_priority(&mut self) -> Result<Option<i64>> {
        let priorities: Vec<i64> = self
            .channels
            .iter()
            .filter(|c| c.priority && !self.lockout.contains(&c.freq_hz))
            .map(|c| c.freq_hz)
            .filter(|f| Some(*f) != self.current)
            .collect();
        for pf in priorities {
            self.source.tune(pf)?;
            let power = self.read_power()?;
            if power >= self.cfg.open_threshold {
                return Ok(Some(pf));
            }
        }
        Ok(None)
    }

    fn read_power(&mut self) -> Result<f32> {
        let block = self.source.read_block()?;
        Ok(signal_power(&block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A test-only SDR source. Scripts a sequence of signal-power values per frequency; each
    /// `read_block` returns an IQ block whose `signal_power` equals the next scripted value (the
    /// last value repeats once a script is exhausted). Lives under `#[cfg(test)]` only.
    struct MockSdrSource {
        tuned: Option<i64>,
        scripts: HashMap<i64, Vec<f32>>,
        cursor: HashMap<i64, usize>,
        default_power: f32,
    }

    impl MockSdrSource {
        fn new(default_power: f32) -> Self {
            Self {
                tuned: None,
                scripts: HashMap::new(),
                cursor: HashMap::new(),
                default_power,
            }
        }

        /// Script a sequence of powers for a frequency (consumed one per read, last repeats).
        fn script(mut self, freq_hz: i64, powers: &[f32]) -> Self {
            self.scripts.insert(freq_hz, powers.to_vec());
            self
        }

        fn next_power(&mut self) -> f32 {
            let Some(freq) = self.tuned else {
                return self.default_power;
            };
            let Some(seq) = self.scripts.get(&freq) else {
                return self.default_power;
            };
            if seq.is_empty() {
                return self.default_power;
            }
            let cur = self.cursor.entry(freq).or_insert(0);
            let val = seq[(*cur).min(seq.len() - 1)];
            if *cur < seq.len() - 1 {
                *cur += 1;
            }
            val
        }
    }

    impl SdrSource for MockSdrSource {
        fn tune(&mut self, freq_hz: i64) -> Result<()> {
            self.tuned = Some(freq_hz);
            Ok(())
        }

        fn read_block(&mut self) -> Result<Vec<Iq>> {
            let power = self.next_power();
            // signal_power = mean(|s|^2); a block of (sqrt(power), 0) yields exactly `power`.
            let amp = power.max(0.0).sqrt();
            Ok(vec![Iq::new(amp, 0.0); 16])
        }
    }

    fn cfg() -> ScannerConfig {
        ScannerConfig {
            open_threshold: 1.0,
            close_threshold: 0.5,
            hold_ms: 1000,
        }
    }

    #[test]
    fn hops_in_watchlist_order_when_idle() {
        // All channels silent → the scanner keeps hopping in order.
        let src = MockSdrSource::new(0.0);
        let chans = vec![
            ScanChannel::new(118_100_000, false),
            ScanChannel::new(121_700_000, false),
            ScanChannel::new(126_000_000, false),
        ];
        let mut sc = Scanner::new(src, cfg(), chans).unwrap();
        assert_eq!(sc.current_freq(), Some(118_100_000));

        assert_eq!(sc.tick(0).unwrap(), ScanEvent::Tuned(121_700_000));
        assert_eq!(sc.tick(1).unwrap(), ScanEvent::Tuned(126_000_000));
        assert_eq!(sc.tick(2).unwrap(), ScanEvent::Tuned(118_100_000));
    }

    #[test]
    fn opens_dwells_holds_then_resumes() {
        // Channel B (121.7) is active for two reads, then goes quiet.
        let src = MockSdrSource::new(0.0).script(121_700_000, &[2.0, 2.0, 0.0]);
        let chans = vec![
            ScanChannel::new(118_100_000, false),
            ScanChannel::new(121_700_000, false),
        ];
        let mut sc = Scanner::new(src, cfg(), chans).unwrap();

        // 118.1 silent → hop to 121.7
        assert_eq!(sc.tick(0).unwrap(), ScanEvent::Tuned(121_700_000));
        // 121.7 active → open + dwell
        assert_eq!(sc.tick(100).unwrap(), ScanEvent::Opened(121_700_000));
        assert_eq!(sc.state(), ScanState::Active);
        // still active
        assert_eq!(sc.tick(200).unwrap(), ScanEvent::Opened(121_700_000));
        // goes quiet → holding (within hold window)
        assert_eq!(sc.tick(300).unwrap(), ScanEvent::Holding(121_700_000));
        assert_eq!(sc.state(), ScanState::Holding);
        // still within hold window
        assert_eq!(sc.tick(400).unwrap(), ScanEvent::Holding(121_700_000));
        // hold expires (>= 1000 ms since last activity at t=200) → close + resume
        assert_eq!(sc.tick(1300).unwrap(), ScanEvent::Closed(121_700_000));
        assert_eq!(sc.state(), ScanState::Scanning);
        // Closed already advanced to 118.1; the next tick finds it silent and hops onward.
        assert_eq!(sc.current_freq(), Some(118_100_000));
        assert_eq!(sc.tick(1400).unwrap(), ScanEvent::Tuned(121_700_000));
    }

    #[test]
    fn priority_channel_preempts_lower_priority_dwell() {
        // 118.1 (normal) is active; 121.5 (priority/guard) is also active and must pre-empt.
        let src = MockSdrSource::new(0.0)
            .script(118_100_000, &[2.0])
            .script(121_500_000, &[2.0]);
        let chans = vec![
            ScanChannel::new(118_100_000, false),
            ScanChannel::new(121_500_000, true),
        ];
        let mut sc = Scanner::new(src, cfg(), chans).unwrap();

        // 118.1 active → dwell
        assert_eq!(sc.tick(0).unwrap(), ScanEvent::Opened(118_100_000));
        assert_eq!(sc.state(), ScanState::Active);
        // next tick: priority 121.5 is active → pre-empt
        assert_eq!(sc.tick(100).unwrap(), ScanEvent::Preempted(121_500_000));
        assert_eq!(sc.current_freq(), Some(121_500_000));
        assert_eq!(sc.state(), ScanState::Active);
    }

    #[test]
    fn lockout_excludes_channel() {
        let src = MockSdrSource::new(0.0);
        let chans = vec![
            ScanChannel::new(118_100_000, false),
            ScanChannel::new(121_700_000, false),
            ScanChannel::new(126_000_000, false),
        ];
        let mut sc = Scanner::new(src, cfg(), chans).unwrap();

        // Lock out the middle channel; hopping must skip it.
        sc.lockout(121_700_000).unwrap();
        assert!(sc.is_locked(121_700_000));

        // From 118.1, advancing skips 121.7 → 126.0 → back to 118.1.
        assert_eq!(sc.tick(0).unwrap(), ScanEvent::Tuned(126_000_000));
        assert_eq!(sc.tick(1).unwrap(), ScanEvent::Tuned(118_100_000));
        // 121.7 is never visited.
        assert_eq!(sc.tick(2).unwrap(), ScanEvent::Tuned(126_000_000));
    }

    #[test]
    fn skip_advances_immediately() {
        let src = MockSdrSource::new(0.0);
        let chans = vec![
            ScanChannel::new(118_100_000, false),
            ScanChannel::new(121_700_000, false),
        ];
        let mut sc = Scanner::new(src, cfg(), chans).unwrap();
        assert_eq!(sc.current_freq(), Some(118_100_000));
        assert_eq!(sc.skip().unwrap(), ScanEvent::Tuned(121_700_000));
        assert_eq!(sc.current_freq(), Some(121_700_000));
    }

    #[test]
    fn all_locked_out_is_idle() {
        let src = MockSdrSource::new(0.0);
        let chans = vec![ScanChannel::new(118_100_000, false)];
        let mut sc = Scanner::new(src, cfg(), chans).unwrap();
        sc.lockout(118_100_000).unwrap();
        assert_eq!(sc.tick(0).unwrap(), ScanEvent::Idle);
    }
}
