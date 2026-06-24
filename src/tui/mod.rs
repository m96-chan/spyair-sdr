//! TUI — `ratatui` rendering of the watchlist / now-playing / waterfall / equalizer.
//!
//! See issue #9. This module owns a plain **view model** (see [`TuiView`] and friends) that is
//! fully decoupled from the planner / scanner internals: callers mirror the handful of fields they
//! want shown into these structs, and the render functions turn that data into terminal output.
//!
//! Rendering is **infallible** and **side-effect free** with respect to the domain: each
//! `render_*` function takes the relevant view model plus a [`ratatui::Frame`] and an area, and
//! draws into the frame's buffer. The TUI never computes DSP — the waterfall and equalizer are fed
//! a frame of magnitudes by the caller and only *visualise* it.
//!
//! All rendering is exercised with [`ratatui::backend::TestBackend`] (an in-memory buffer); there
//! is no real terminal and no event loop in this module.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use ratatui::Frame;

/// Demodulation mode shown in the now-playing header.
///
/// This mirrors the scanner's notion of a channel mode without depending on it, so the TUI stays
/// decoupled from scanner internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMode {
    /// Amplitude modulation (e.g. airband voice).
    Am,
    /// Narrow-band frequency modulation (e.g. marine / amateur voice).
    Fm,
    /// Wide-band frequency modulation (e.g. broadcast).
    WideFm,
}

impl ChannelMode {
    /// Short, fixed label for the mode (`"AM"`, `"FM"`, `"WFM"`).
    ///
    /// Returns a stable `&'static str` so header rendering is deterministic.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ChannelMode::Am => "AM",
            ChannelMode::Fm => "FM",
            ChannelMode::WideFm => "WFM",
        }
    }
}

/// The channel currently being listened to, as shown in the now-playing header.
///
/// Frequency is stored in **Hz** (SI internally, per the crate conventions) and converted to MHz
/// only at render time.
#[derive(Debug, Clone, PartialEq)]
pub struct NowPlaying {
    /// Tuned frequency, in hertz.
    pub freq_hz: u64,
    /// Demodulation mode.
    pub mode: ChannelMode,
    /// Relative signal strength in the range `0.0..=1.0`. Values are clamped at render time.
    pub signal: f32,
    /// English description of the service (e.g. `"Tokyo Approach"`).
    pub desc_en: String,
    /// Japanese description of the service (e.g. `"東京アプローチ"`), if known.
    pub desc_jp: Option<String>,
}

/// State marker for a watchlist row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchState {
    /// Currently carrying signal / being received.
    Active,
    /// In range but silent.
    Idle,
    /// Priority channel (interrupts lower-priority traffic).
    Priority,
}

impl WatchState {
    /// Single-character marker for the state (`"*"` active, `" "` idle, `"!"` priority).
    ///
    /// Returns a stable `&'static str` so table rendering is deterministic.
    #[must_use]
    pub fn marker(self) -> &'static str {
        match self {
            WatchState::Active => "*",
            WatchState::Idle => " ",
            WatchState::Priority => "!",
        }
    }
}

/// A single row in the watchlist table.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchRow {
    /// Channel frequency, in hertz.
    pub freq_hz: u64,
    /// Human-readable description of the channel.
    pub description: String,
    /// State marker for the row.
    pub state: WatchState,
}

/// Optional flight / facility context panel.
///
/// Shown only when the caller supplies it (e.g. when an ADS-B correlation or a known facility is
/// available for the active channel).
#[derive(Debug, Clone, PartialEq)]
pub struct FacilityInfo {
    /// Facility or station name (e.g. `"RJTT — Tokyo Intl (Haneda)"`).
    pub name: String,
    /// Free-form detail lines (e.g. nearest flight callsign, distance, bearing).
    pub details: Vec<String>,
}

/// The complete view model for one render pass.
///
/// Every field is plain data owned by this module. Optional panels ([`transcript`](Self::transcript)
/// and [`facility`](Self::facility)) are rendered **only** when `Some`.
#[derive(Debug, Clone, PartialEq)]
pub struct TuiView {
    /// The now-playing header content.
    pub now_playing: NowPlaying,
    /// Rows of the watchlist table.
    pub watchlist: Vec<WatchRow>,
    /// One magnitude frame for the equalizer / waterfall, typically `0.0..=1.0` per bin.
    ///
    /// The TUI only visualises these values; it does not compute them.
    pub spectrum: Vec<f32>,
    /// Optional transcript lines (e.g. Whisper output). Rendered only when `Some` and non-empty.
    pub transcript: Option<Vec<String>>,
    /// Optional flight / facility panel. Rendered only when `Some`.
    pub facility: Option<FacilityInfo>,
}

/// Convert a frequency in hertz to a fixed-precision MHz string (3 decimal places).
///
/// Kept private and deterministic so header / table rendering is stable across runs.
fn format_mhz(freq_hz: u64) -> String {
    // freq_hz / 1_000_000, rendered with 3 decimals (kHz resolution).
    let mhz = freq_hz as f64 / 1_000_000.0;
    format!("{mhz:.3} MHz")
}

/// Eight vertical block characters used by the equalizer / waterfall, from empty to full.
///
/// Index `0` is a space (no signal) and index `8` is a full block; intermediate indices step up
/// in eighths. Using a fixed table makes the visualisation deterministic for a given input frame.
const BARS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

/// Map a single magnitude in `0.0..=1.0` to one of the [`BARS`] glyphs.
///
/// Values are clamped into range, and `NaN` maps to the empty bar so rendering never panics.
fn bar_for(magnitude: f32) -> &'static str {
    let m = if magnitude.is_nan() {
        0.0
    } else {
        magnitude.clamp(0.0, 1.0)
    };
    // Quantise into 0..=8 levels.
    let level = (m * 8.0).round() as usize;
    let level = level.min(8);
    BARS[level]
}

/// Render the now-playing header: active frequency, mode, signal, and EN/JP description.
///
/// The header is drawn inside a bordered block titled `"Now Playing"`. The Japanese description is
/// shown only when present in the view model.
pub fn render_now_playing(frame: &mut Frame, area: Rect, np: &NowPlaying) {
    let signal = if np.signal.is_nan() {
        0.0
    } else {
        np.signal.clamp(0.0, 1.0)
    };
    let signal_pct = (signal * 100.0).round() as u32;

    let mut lines = vec![Line::from(vec![
        Span::styled(
            format_mhz(np.freq_hz),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::raw(np.mode.label()),
        Span::raw("  "),
        Span::raw(format!("signal {signal_pct}%")),
    ])];
    lines.push(Line::from(Span::raw(format!("EN: {}", np.desc_en))));
    if let Some(jp) = np.desc_jp.as_ref() {
        lines.push(Line::from(Span::raw(format!("JP: {jp}"))));
    }

    let block = Block::default().borders(Borders::ALL).title("Now Playing");
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Render the watchlist table: one row per channel with a state marker, frequency, and description.
///
/// Rows are drawn in the order supplied by the view model; nothing is sorted or filtered here.
pub fn render_watchlist(frame: &mut Frame, area: Rect, rows: &[WatchRow]) {
    let table_rows: Vec<Row> = rows
        .iter()
        .map(|r| {
            Row::new(vec![
                Cell::from(r.state.marker()),
                Cell::from(format_mhz(r.freq_hz)),
                Cell::from(r.description.clone()),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(1),
        Constraint::Length(12),
        Constraint::Min(10),
    ];
    let table = Table::new(table_rows, widths)
        .header(Row::new(vec![
            Cell::from(" "),
            Cell::from("Freq"),
            Cell::from("Description"),
        ]))
        .block(Block::default().borders(Borders::ALL).title("Watchlist"));
    frame.render_widget(table, area);
}

/// Render the equalizer: a single horizontal bar of vertical block glyphs, one per magnitude bin.
///
/// The output is fully determined by `spectrum`: the same input frame always produces the same
/// glyphs. Bins beyond the available width are dropped by the layout; magnitudes are clamped to
/// `0.0..=1.0`.
pub fn render_equalizer(frame: &mut Frame, area: Rect, spectrum: &[f32]) {
    let bars: String = spectrum.iter().map(|&m| bar_for(m)).collect();
    let block = Block::default().borders(Borders::ALL).title("Equalizer");
    frame.render_widget(Paragraph::new(bars).block(block), area);
}

/// Render the waterfall: a row of block glyphs for the provided magnitude frame.
///
/// This module visualises a single provided frame; callers that keep a history can pass each frame
/// in turn. The glyph for each bin is the same deterministic mapping used by the equalizer, so a
/// given input frame always renders identically.
pub fn render_waterfall(frame: &mut Frame, area: Rect, spectrum: &[f32]) {
    let bars: String = spectrum.iter().map(|&m| bar_for(m)).collect();
    let block = Block::default().borders(Borders::ALL).title("Waterfall");
    frame.render_widget(Paragraph::new(bars).block(block), area);
}

/// Render the optional transcript panel.
///
/// Lines are shown verbatim, newest last, inside a bordered block titled `"Transcript"`.
pub fn render_transcript(frame: &mut Frame, area: Rect, lines: &[String]) {
    let text: Vec<Line> = lines.iter().map(|l| Line::from(l.clone())).collect();
    let block = Block::default().borders(Borders::ALL).title("Transcript");
    frame.render_widget(Paragraph::new(text).block(block), area);
}

/// Render the optional flight / facility panel.
///
/// Shows the facility name (bold) followed by its detail lines, inside a bordered block.
pub fn render_facility(frame: &mut Frame, area: Rect, facility: &FacilityInfo) {
    let mut lines = vec![Line::from(Span::styled(
        facility.name.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    ))];
    for d in &facility.details {
        lines.push(Line::from(d.clone()));
    }
    let block = Block::default().borders(Borders::ALL).title("Facility");
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Render the full TUI for one frame into the whole `area`.
///
/// The layout is computed top-to-bottom: header, watchlist, equalizer, waterfall, and then the
/// optional transcript / facility panels. **Optional panels consume layout space only when present
/// in the view model** — when absent, no area is reserved and nothing is drawn for them.
pub fn render(frame: &mut Frame, area: Rect, view: &TuiView) {
    // Build the constraint list dynamically so optional panels take space only when shown.
    let mut constraints: Vec<Constraint> = vec![
        Constraint::Length(5), // now playing
        Constraint::Min(5),    // watchlist
        Constraint::Length(3), // equalizer
        Constraint::Length(3), // waterfall
    ];
    let show_transcript = view.transcript.as_ref().is_some_and(|t| !t.is_empty());
    if show_transcript {
        constraints.push(Constraint::Length(5));
    }
    if view.facility.is_some() {
        constraints.push(Constraint::Length(5));
    }

    let chunks = Layout::new(Direction::Vertical, constraints).split(area);

    render_now_playing(frame, chunks[0], &view.now_playing);
    render_watchlist(frame, chunks[1], &view.watchlist);
    render_equalizer(frame, chunks[2], &view.spectrum);
    render_waterfall(frame, chunks[3], &view.spectrum);

    let mut next = 4;
    if show_transcript {
        if let Some(t) = view.transcript.as_ref() {
            render_transcript(frame, chunks[next], t);
        }
        next += 1;
    }
    if let Some(f) = view.facility.as_ref() {
        render_facility(frame, chunks[next], f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Flatten a [`TestBackend`] buffer into a single string of its cell symbols.
    ///
    /// Cells are concatenated in row-major order, which is enough for `contains(..)` assertions.
    fn buffer_to_string(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn sample_now_playing() -> NowPlaying {
        NowPlaying {
            freq_hz: 118_100_000,
            mode: ChannelMode::Am,
            signal: 0.75,
            desc_en: "Tokyo Tower".to_string(),
            desc_jp: Some("東京タワー".to_string()),
        }
    }

    fn sample_watchlist() -> Vec<WatchRow> {
        vec![
            WatchRow {
                freq_hz: 118_100_000,
                description: "Tokyo Tower".to_string(),
                state: WatchState::Active,
            },
            WatchRow {
                freq_hz: 121_500_000,
                description: "Emergency".to_string(),
                state: WatchState::Priority,
            },
            WatchRow {
                freq_hz: 119_000_000,
                description: "Ground".to_string(),
                state: WatchState::Idle,
            },
        ]
    }

    #[test]
    fn now_playing_shows_freq_mode_and_descriptions() {
        let np = sample_now_playing();
        let mut terminal = Terminal::new(TestBackend::new(60, 6)).expect("terminal");
        terminal
            .draw(|f| render_now_playing(f, f.area(), &np))
            .expect("draw");
        let s = buffer_to_string(&terminal);
        assert!(s.contains("118.100 MHz"), "freq missing: {s}");
        assert!(s.contains("AM"), "mode missing: {s}");
        assert!(s.contains("Tokyo Tower"), "EN desc missing: {s}");
        // The JP description is rendered; note that double-width (CJK) glyphs each occupy two
        // buffer cells (symbol + empty continuation), so the flattened string is not contiguous.
        // Asserting on the label and an individual glyph is the robust check.
        assert!(s.contains("JP:"), "JP label missing: {s}");
        assert!(s.contains('東'), "JP desc missing: {s}");
    }

    #[test]
    fn now_playing_omits_jp_when_absent() {
        let mut np = sample_now_playing();
        np.desc_jp = None;
        let mut terminal = Terminal::new(TestBackend::new(60, 6)).expect("terminal");
        terminal
            .draw(|f| render_now_playing(f, f.area(), &np))
            .expect("draw");
        let s = buffer_to_string(&terminal);
        assert!(s.contains("Tokyo Tower"), "EN desc missing: {s}");
        assert!(!s.contains("JP:"), "JP label should be absent: {s}");
    }

    #[test]
    fn watchlist_renders_freq_and_description_per_row() {
        let rows = sample_watchlist();
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).expect("terminal");
        terminal
            .draw(|f| render_watchlist(f, f.area(), &rows))
            .expect("draw");
        let s = buffer_to_string(&terminal);
        assert!(s.contains("118.100 MHz"), "row freq missing: {s}");
        assert!(s.contains("Tokyo Tower"), "row desc missing: {s}");
        assert!(s.contains("121.500 MHz"), "row freq missing: {s}");
        assert!(s.contains("Emergency"), "row desc missing: {s}");
        // Priority marker present for the emergency row.
        assert!(s.contains('!'), "priority marker missing: {s}");
    }

    #[test]
    fn equalizer_is_deterministic_for_a_given_frame() {
        let spectrum = vec![0.0, 0.25, 0.5, 0.75, 1.0];

        let render = || {
            let mut terminal = Terminal::new(TestBackend::new(20, 3)).expect("terminal");
            terminal
                .draw(|f| render_equalizer(f, f.area(), &spectrum))
                .expect("draw");
            buffer_to_string(&terminal)
        };

        let first = render();
        let second = render();
        assert_eq!(first, second, "same input must produce same buffer");
        // Full magnitude maps to the full block.
        assert!(first.contains('█'), "full bar missing: {first}");
    }

    #[test]
    fn equalizer_distinguishes_levels() {
        // Two different frames must produce different output (visualisation is data-driven).
        let low = vec![0.0, 0.0, 0.0, 0.0, 0.0];
        let high = vec![1.0, 1.0, 1.0, 1.0, 1.0];

        let render = |spec: &Vec<f32>| {
            let mut terminal = Terminal::new(TestBackend::new(20, 3)).expect("terminal");
            terminal
                .draw(|f| render_equalizer(f, f.area(), spec))
                .expect("draw");
            buffer_to_string(&terminal)
        };

        assert_ne!(render(&low), render(&high), "levels must differ visually");
    }

    #[test]
    fn waterfall_is_deterministic_for_a_given_frame() {
        let spectrum = vec![0.1, 0.4, 0.9, 0.2];
        let render = || {
            let mut terminal = Terminal::new(TestBackend::new(20, 3)).expect("terminal");
            terminal
                .draw(|f| render_waterfall(f, f.area(), &spectrum))
                .expect("draw");
            buffer_to_string(&terminal)
        };
        assert_eq!(render(), render(), "same input must produce same buffer");
    }

    #[test]
    fn bar_mapping_clamps_and_handles_nan() {
        assert_eq!(bar_for(0.0), " ");
        assert_eq!(bar_for(1.0), "█");
        assert_eq!(bar_for(2.0), "█", "above 1.0 clamps to full");
        assert_eq!(bar_for(-1.0), " ", "below 0.0 clamps to empty");
        assert_eq!(bar_for(f32::NAN), " ", "NaN maps to empty, never panics");
    }

    fn core_view() -> TuiView {
        TuiView {
            now_playing: sample_now_playing(),
            watchlist: sample_watchlist(),
            spectrum: vec![0.0, 0.5, 1.0],
            transcript: None,
            facility: None,
        }
    }

    #[test]
    fn transcript_panel_absent_when_none() {
        let view = core_view();
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).expect("terminal");
        terminal.draw(|f| render(f, f.area(), &view)).expect("draw");
        let s = buffer_to_string(&terminal);
        assert!(
            !s.contains("Transcript"),
            "transcript should be absent: {s}"
        );
    }

    #[test]
    fn transcript_panel_present_when_some() {
        let mut view = core_view();
        view.transcript = Some(vec!["cleared for takeoff".to_string()]);
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).expect("terminal");
        terminal.draw(|f| render(f, f.area(), &view)).expect("draw");
        let s = buffer_to_string(&terminal);
        assert!(s.contains("Transcript"), "transcript title missing: {s}");
        assert!(
            s.contains("cleared for takeoff"),
            "transcript text missing: {s}"
        );
    }

    #[test]
    fn facility_panel_absent_when_none() {
        let view = core_view();
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).expect("terminal");
        terminal.draw(|f| render(f, f.area(), &view)).expect("draw");
        let s = buffer_to_string(&terminal);
        assert!(!s.contains("Facility"), "facility should be absent: {s}");
    }

    #[test]
    fn facility_panel_present_when_some() {
        let mut view = core_view();
        view.facility = Some(FacilityInfo {
            name: "RJTT Haneda".to_string(),
            details: vec!["JAL123 12km NE".to_string()],
        });
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).expect("terminal");
        terminal.draw(|f| render(f, f.area(), &view)).expect("draw");
        let s = buffer_to_string(&terminal);
        assert!(s.contains("Facility"), "facility title missing: {s}");
        assert!(s.contains("RJTT Haneda"), "facility name missing: {s}");
        assert!(s.contains("JAL123 12km NE"), "facility detail missing: {s}");
    }

    #[test]
    fn full_render_shows_core_panels_together() {
        let view = core_view();
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).expect("terminal");
        terminal.draw(|f| render(f, f.area(), &view)).expect("draw");
        let s = buffer_to_string(&terminal);
        assert!(s.contains("Now Playing"), "header missing: {s}");
        assert!(s.contains("Watchlist"), "watchlist missing: {s}");
        assert!(s.contains("Equalizer"), "equalizer missing: {s}");
        assert!(s.contains("Waterfall"), "waterfall missing: {s}");
    }
}
