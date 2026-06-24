//! `build-db` — assemble the local frequency database (`spyair.db`) from OurAirports CSVs.
//!
//! Offline path (real, supported here):
//! ```text
//! build-db --airports airports.csv --frequencies airport-frequencies.csv --out data/spyair.db
//! ```
//!
//! Without `--airports`/`--frequencies`, the README's network auto-download is attempted, which
//! is currently a stub that returns `NotImplemented` (it never fabricates data). See epic #1.

use std::fs::File;
use std::path::PathBuf;
use std::process::ExitCode;

use spyair_sdr::error::Result;
use spyair_sdr::freqdb::{ourairports, ChannelStore};

struct Args {
    airports: Option<PathBuf>,
    frequencies: Option<PathBuf>,
    out: PathBuf,
    airband_only: bool,
}

fn parse_args() -> std::result::Result<Args, String> {
    let mut airports = None;
    let mut frequencies = None;
    let mut out = PathBuf::from("data/spyair.db");
    let mut airband_only = false;

    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--airports" => {
                airports = Some(PathBuf::from(
                    it.next().ok_or("--airports requires a path")?,
                ))
            }
            "--frequencies" => {
                frequencies = Some(PathBuf::from(
                    it.next().ok_or("--frequencies requires a path")?,
                ))
            }
            "--out" => out = PathBuf::from(it.next().ok_or("--out requires a path")?),
            "--airband" => airband_only = true,
            "-h" | "--help" => return Err(usage()),
            other => return Err(format!("unknown argument: {other}\n{}", usage())),
        }
    }
    Ok(Args {
        airports,
        frequencies,
        out,
        airband_only,
    })
}

fn usage() -> String {
    "usage: build-db [--airports <airports.csv>] [--frequencies <airport-frequencies.csv>] \
     [--airband] [--out data/spyair.db]"
        .to_string()
}

fn run(args: Args) -> Result<usize> {
    // Resolve CSV inputs: local files (offline) or the (stubbed) network download.
    let (airports_index, frequencies_reader): (_, Box<dyn std::io::Read>) =
        match (&args.airports, &args.frequencies) {
            (Some(a), Some(f)) => {
                let idx = ourairports::parse_airports(File::open(a)?)?;
                (idx, Box::new(File::open(f)?))
            }
            _ => {
                // No local inputs → attempt the network download (currently NotImplemented).
                let (airports_csv, freq_csv) = ourairports::fetch_public_sources()?;
                let idx = ourairports::parse_airports(airports_csv.as_bytes())?;
                (idx, Box::new(std::io::Cursor::new(freq_csv)))
            }
        };

    let mut channels = ourairports::parse_frequencies(frequencies_reader, &airports_index)?;

    if args.airband_only {
        let before = channels.len();
        channels.retain(|c| c.is_airband());
        eprintln!(
            "build-db: airband filter kept {} of {} channels (118-137 MHz)",
            channels.len(),
            before
        );
    }

    if let Some(parent) = args.out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut store = ChannelStore::open(&args.out)?;
    let n = store.insert_channels(&channels)?;
    Ok(n)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };
    let out = args.out.clone();
    match run(args) {
        Ok(n) => {
            println!("build-db: wrote {n} channels to {}", out.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("build-db: {e}");
            ExitCode::FAILURE
        }
    }
}
