//! `spyair-tui` — a live terminal dashboard for the scanner.
//!
//! This binary is the **thin I/O shell** around the pure rendering in [`spyair_sdr::tui`]: it owns
//! the real terminal (raw mode + alternate screen), an event loop, and a quit key. It does not
//! compute DSP or talk to hardware — it feeds the renderer a [`spyair_sdr::tui::TuiView`].
//!
//! Until the scanner pipeline is wired in, the view is populated with a representative **demo**
//! snapshot (Tokyo / Haneda airband) and an animated spectrum so the layout can be seen and
//! reviewed on a real terminal. Press `q` or `Esc` (or `Ctrl-C`) to quit.
//!
//! Run: `cargo run --bin spyair-tui`

use std::io::{self, Stdout};
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::crossterm::ExecutableCommand;
use ratatui::Terminal;

use spyair_sdr::tui::{
    render, ChannelMode, FacilityInfo, NowPlaying, TuiView, WatchRow, WatchState,
};

/// Number of bins in the demo spectrum frame.
const SPECTRUM_BINS: usize = 48;

/// Build the static parts of the demo view (Tokyo / Haneda airband).
fn demo_watchlist() -> Vec<WatchRow> {
    vec![
        WatchRow {
            freq_hz: 118_100_000,
            description: "RJTT Tower (Haneda)".into(),
            state: WatchState::Active,
        },
        WatchRow {
            freq_hz: 121_500_000,
            description: "Emergency / guard".into(),
            state: WatchState::Priority,
        },
        WatchRow {
            freq_hz: 124_350_000,
            description: "Tokyo Approach".into(),
            state: WatchState::Idle,
        },
        WatchRow {
            freq_hz: 126_000_000,
            description: "Tokyo Departure".into(),
            state: WatchState::Idle,
        },
        WatchRow {
            freq_hz: 119_100_000,
            description: "RJTT Ground".into(),
            state: WatchState::Idle,
        },
    ]
}

/// Animate a spectrum frame deterministically from a frame counter (no RNG): a moving carrier
/// bump over a low noise floor, values in `0.0..=1.0`.
fn demo_spectrum(tick: u64) -> Vec<f32> {
    let phase = tick as f32 * 0.15;
    let centre = SPECTRUM_BINS as f32 / 2.0 + (phase.sin() * 6.0);
    (0..SPECTRUM_BINS)
        .map(|i| {
            let x = i as f32;
            let floor = 0.12 + 0.06 * ((x * 0.5 + phase).sin() * 0.5 + 0.5);
            let d = (x - centre) / 3.0;
            let bump = 0.85 * (-d * d).exp();
            (floor + bump).clamp(0.0, 1.0)
        })
        .collect()
}

/// Build the full demo view for a given animation tick.
fn demo_view(tick: u64) -> TuiView {
    let signal = 0.55 + 0.35 * ((tick as f32 * 0.2).sin() * 0.5 + 0.5);
    TuiView {
        now_playing: NowPlaying {
            freq_hz: 118_100_000,
            mode: ChannelMode::Am,
            signal,
            desc_en: "Tokyo Tower (Haneda)".into(),
            desc_jp: Some("東京タワー（羽田）".into()),
        },
        watchlist: demo_watchlist(),
        spectrum: demo_spectrum(tick),
        transcript: None,
        facility: Some(FacilityInfo {
            name: "RJTT — Tokyo Intl (Haneda)".into(),
            details: vec![
                "demo data — scanner pipeline not yet wired".into(),
                "press q / Esc to quit".into(),
            ],
        }),
    }
}

/// Restore the terminal to its normal state. Best-effort; errors are ignored on teardown.
fn restore(terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
    let _ = disable_raw_mode();
    let _ = terminal.backend_mut().execute(LeaveAlternateScreen);
    let _ = terminal.show_cursor();
}

fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    let mut tick: u64 = 0;
    loop {
        terminal.draw(|frame| {
            let area = frame.area();
            let view = demo_view(tick);
            render(frame, area, &view);
        })?;

        // Poll for input; tick the animation roughly every 200 ms otherwise.
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                let quit = matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL));
                if quit {
                    return Ok(());
                }
            }
        }
        tick = tick.wrapping_add(1);
    }
}

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal);
    restore(&mut terminal);
    result
}
