//! `spyair-tui` — a live terminal dashboard for the scanner.
//!
//! This binary is the **thin I/O shell** around the pure rendering in [`spyair_sdr::tui`] and the
//! pure DSP in [`spyair_sdr::dsp`]: it owns the real terminal (raw mode + alternate screen), an
//! event loop, and a quit key.
//!
//! Two modes:
//! - **Demo** (default): a representative static snapshot (Tokyo / Haneda airband) with an
//!   animated, RNG-free spectrum, so the layout can be seen without hardware.
//! - **Live** (requires `--features rtlsdr` and `--freq`): opens a real RTL-SDR, tunes, and feeds
//!   real IQ blocks through [`spyair_sdr::dsp::power_spectrum`] into the spectrum panel and
//!   [`spyair_sdr::dsp::signal_power_db`] into the signal meter.
//!
//! Press `q` / `Esc` / `Ctrl-C` to quit.
//!
//! Run (demo):  `cargo run --bin spyair-tui`
//! Run (live):  `cargo run --bin spyair-tui --features rtlsdr -- --freq 82.5 --device 0`

use std::io::{self, Stdout};
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::crossterm::ExecutableCommand;
use ratatui::Terminal;

use spyair_sdr::tui::{
    render, ChannelMode, FacilityInfo, NowPlaying, TuiView, WatchRow, WatchState,
};

/// Number of bins in the spectrum frame (demo animation and live FFT down-binning alike).
const SPECTRUM_BINS: usize = 48;

/// Parsed command-line options.
#[cfg_attr(not(feature = "rtlsdr"), allow(dead_code))]
struct Args {
    /// RTL-SDR device index for live mode.
    device: u32,
    /// Centre frequency in Hz for live mode; `None` means demo mode.
    freq_hz: Option<i64>,
}

/// Parse `--device <idx>` and `--freq <MHz>` from the process arguments.
fn parse_args() -> Result<Args, String> {
    let mut device: u32 = 0;
    let mut freq_hz: Option<i64> = None;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--device" | "-d" => {
                let v = it.next().ok_or("--device needs a value")?;
                device = v.parse().map_err(|_| format!("invalid --device: {v}"))?;
            }
            "--freq" | "-f" => {
                let v = it.next().ok_or("--freq needs a value (MHz)")?;
                let mhz: f64 = v.parse().map_err(|_| format!("invalid --freq: {v}"))?;
                freq_hz = Some((mhz * 1_000_000.0).round() as i64);
            }
            "-h" | "--help" => {
                return Err("usage: spyair-tui [--freq <MHz>] [--device <idx>]".into());
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Args { device, freq_hz })
}

/// True when a key event should terminate the loop (`q`, `Esc`, or `Ctrl-C`).
fn is_quit(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}

/// Demo watchlist (Tokyo / Haneda airband).
fn demo_watchlist() -> Vec<WatchRow> {
    vec![
        WatchRow {
            freq_hz: 118_100_000,
            description: "RJTT Tower (Haneda)".into(),
            state: WatchState::Active,
        },
        WatchRow {
            freq_hz: 120_500_000,
            description: "Tokyo Approach".into(),
            state: WatchState::Priority,
        },
        WatchRow {
            freq_hz: 120_800_000,
            description: "Tokyo Approach".into(),
            state: WatchState::Idle,
        },
        WatchRow {
            freq_hz: 121_500_000,
            description: "Emergency / guard".into(),
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
                "demo data — run with --features rtlsdr --freq <MHz> for live".into(),
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

/// Demo render loop.
fn run_demo(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    let mut tick: u64 = 0;
    loop {
        terminal.draw(|frame| render(frame, frame.area(), &demo_view(tick)))?;
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if is_quit(key) {
                    return Ok(());
                }
            }
        }
        tick = tick.wrapping_add(1);
    }
}

/// Pick a display mode + label for a tuned frequency (rough, for the header only).
#[cfg(feature = "rtlsdr")]
fn mode_and_desc(freq_hz: i64) -> (ChannelMode, String) {
    let mhz = freq_hz as f64 / 1_000_000.0;
    if (76.0..95.0).contains(&mhz) {
        (ChannelMode::WideFm, "FM broadcast".into())
    } else if (108.0..137.0).contains(&mhz) {
        (ChannelMode::Am, "Airband (AM)".into())
    } else {
        (ChannelMode::Fm, "NFM".into())
    }
}

/// Live render loop: open a real RTL-SDR, tune, and show its spectrum + signal level.
#[cfg(feature = "rtlsdr")]
fn run_live(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    device: u32,
    freq_hz: i64,
) -> io::Result<()> {
    use spyair_sdr::dsp::{power_spectrum, signal_power_db};
    use spyair_sdr::scanner::SdrSource;
    use spyair_sdr::sdr::RtlSdrDevice;

    let to_io = |e: spyair_sdr::Error| io::Error::other(e.to_string());

    let mut dev = RtlSdrDevice::open(device).map_err(to_io)?;
    dev.tune(freq_hz).map_err(to_io)?;
    let _ = dev.read_block().map_err(to_io)?; // discard the first post-tune block
    let (mode, desc) = mode_and_desc(freq_hz);

    loop {
        let block = dev.read_block().map_err(to_io)?;
        let spectrum = power_spectrum(&block, SPECTRUM_BINS);
        let db = signal_power_db(&block);
        // Map roughly [-50 dB, 0 dB] of mean IQ power onto the 0..1 signal meter.
        let signal = ((db + 50.0) / 50.0).clamp(0.0, 1.0);

        let view = TuiView {
            now_playing: NowPlaying {
                freq_hz: freq_hz.max(0) as u64,
                mode,
                signal,
                desc_en: desc.clone(),
                desc_jp: None,
            },
            watchlist: demo_watchlist(),
            spectrum,
            transcript: None,
            facility: Some(FacilityInfo {
                name: format!("LIVE — RTL-SDR device {device}"),
                details: vec![
                    format!(
                        "{:.3} MHz  {}  ({:.1} dB)",
                        freq_hz as f64 / 1e6,
                        mode.label(),
                        db
                    ),
                    "press q / Esc to quit".into(),
                ],
            }),
        };
        terminal.draw(|frame| render(frame, frame.area(), &view))?;

        // ~25 fps cap; also services the quit key promptly.
        if event::poll(Duration::from_millis(40))? {
            if let Event::Key(key) = event::read()? {
                if is_quit(key) {
                    return Ok(());
                }
            }
        }
    }
}

/// Dispatch to the live loop when a frequency is given and the feature is built in; otherwise demo.
#[cfg(feature = "rtlsdr")]
fn run_app(terminal: &mut Terminal<CrosstermBackend<Stdout>>, args: &Args) -> io::Result<()> {
    match args.freq_hz {
        Some(f) => run_live(terminal, args.device, f),
        None => run_demo(terminal),
    }
}

/// Without the `rtlsdr` feature there is no live backend; always run the demo.
#[cfg(not(feature = "rtlsdr"))]
fn run_app(terminal: &mut Terminal<CrosstermBackend<Stdout>>, _args: &Args) -> io::Result<()> {
    run_demo(terminal)
}

fn main() -> io::Result<()> {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return Ok(());
        }
    };

    // Guard: live requested but the backend was not compiled in.
    #[cfg(not(feature = "rtlsdr"))]
    if args.freq_hz.is_some() {
        eprintln!(
            "live mode (--freq) requires building with `--features rtlsdr`; \
             run without --freq for the demo."
        );
        return Ok(());
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, &args);
    restore(&mut terminal);
    result
}
