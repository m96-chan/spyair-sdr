# spyair-sdr

> A location-aware RTL-SDR scanner that figures out **what you can hear from where you are**,
> watches those frequencies, and pops up a live TUI with audio the moment something transmits.
> Aviation-first, but not aviation-only.

<p align="left">
  <img alt="status" src="https://img.shields.io/badge/status-WIP-orange">
  <img alt="rust" src="https://img.shields.io/badge/rust-1.78%2B-orange">
  <img alt="license" src="https://img.shields.io/badge/license-MIT-green">
</p>

---

## What it does

You plug in an RTL-SDR. `spyair-sdr` works out your position, builds a list of frequencies you
realistically have a chance of receiving (airband ATC, ATIS, VOLMET, ham repeaters, marine VHF,
weather, …), and scans them. When a channel breaks squelch, it raises a terminal UI with a live
equalizer / waterfall and plays the audio. Each channel is annotated with a human-readable
description in **English and Japanese**.

Optionally, it transcribes the audio with Whisper, and — for aviation — correlates the active
frequency with nearby ADS-B traffic to make a best-effort guess at the **flight (callsign) and the
controlling facility** (tower / ground / approach / center).

---

## TUI preview

```
┌─ spyair-sdr ───────────────────────────────────── 35.55°N 139.78°E · 5m ─┐
│ ● WATCHING   118.100 MHz  AM    S9+10dB   ◉ REC          12:04:33Z       │
├──────────────────────────────────────────────────────────────────────────┤
│  RJTT Haneda Tower  /  東京国際空港 管制塔                                 │
│                                                                            │
│  Waterfall ────────────────────────────────────────────────────────────  │
│   -2k  ░░▒▒▓▓██▓▓▒▒░░ ░▒▓█▓▒░  ░░▒▓▓▒░░    ░▒▒░     ░▒▓██▓▒░   +2k        │
│        ░▒▓██████▓▒░░  ▒▓███▓▒  ░▒▓███▓▒░   ▒▓▒░    ░▒▓████▓▒              │
│                                                                            │
│  Equalizer ─────────────────────────────────────────────────────────────  │
│     63   160   400    1k   2.5k    6k   16k                                 │
│      ▃    ▅     ▇      █     ▆      ▄     ▂                                  │
│      ▃    ▅     ▇      █     ▆      ▄     ▂                                  │
│                                                                            │
│  Transcript (whisper) ───────────────────────────────────────────────────  │
│   12:04:21Z  "Tokyo Tower, JAL515, ready for departure"                     │
│   12:04:30Z  "JAL515, runway 34R, cleared for takeoff, wind 010 at 8"       │
│                                                                            │
│  ✈ Flight: JAL515  (B789, JA873J)   ·   Facility: Haneda TWR (RJTT)         │
├── Watchlist ─────────────────────────────────────────────────────────────┤
│ ▶ 118.100  Haneda Tower          ● active   S9+10                           │
│   121.700  Haneda Ground         · idle                                     │
│   126.000  Tokyo Approach        · idle                                     │
│   124.350  Haneda ATIS           · idle      (info, low priority)           │
│   121.500  Guard / Emergency     · idle      ★ priority                     │
├──────────────────────────────────────────────────────────────────────────┤
│ [space] hold   [s] skip   [l] lockout   [p] pin   [t] transcribe   [q] quit │
└──────────────────────────────────────────────────────────────────────────┘
```

*Layout sketch — actual rendering is built with [`ratatui`](https://github.com/ratatui/ratatui).
The waterfall scrolls in real time, the equalizer bars react to the demodulated audio, and panels
(transcript / flight info) only appear when their feature is enabled.*

---

## Features

### Core

- **Multi-source location estimation**
  - GPS via `gpsd` or a serial NMEA receiver
  - IP-based geolocation as a fallback
  - Manual `lat / lon / altitude` from config
  - Altitude matters: VHF airband is line-of-sight, so range is estimated from your height + a
    standard radio-horizon model.

- **Automatic frequency planning ("what can I hear?")**
  - Given your position, the planner builds a watchlist from the local frequency database
    (see [Frequency database](#frequency-database)).
  - Each candidate is scored by estimated receivability (distance vs. radio horizon, service type,
    typical power). Out-of-range entries are dropped.

- **Squelch-based watching / scanning**
  - Channel-hops across the watchlist, opens on signal, and dwells while there is activity.
  - Priority channels, hold time, and lockout are configurable.

- **Bilingual channel descriptions (EN / JP)**
  - Every frequency carries a short description in both languages, e.g.
    `118.100 MHz — RJTT Haneda Tower / 東京国際空港 管制塔`.

- **TUI with live equalizer**
  - On squelch-open, the terminal UI surfaces a waterfall + VU-style equalizer that animates with
    the received audio, alongside the current frequency and its description.
  - Runs happily over SSH on a headless box (e.g. a Raspberry Pi).

### Optional

- **Whisper transcription**
  - Local speech-to-text via [`whisper-rs`](https://github.com/tazz4843/whisper-rs)
    (bindings to `whisper.cpp`, CPU or GPU). Transcripts are timestamped and logged per channel.

- **Flight & facility identification (aviation)**
  - Ingests ADS-B from a local `dump1090` / `readsb` feed to know which aircraft are nearby.
  - Maps the active ATC frequency to its known facility from the database.
  - Cross-references active aircraft within range to make a best-effort guess at the callsign /
    flight number associated with the exchange. *(Heuristic — treat as a hint, not ground truth.)*

---

## Frequency database

`spyair-sdr` ships with a build step that compiles open sources into a single local **SQLite**
file (`spyair.db`). The planner only ever queries this local DB at runtime, so scanning works
fully offline.

| Band / domain | Source | License / notes |
|---------------|--------|-----------------|
| **Aviation** (TWR / GND / APP / CTR / ATIS / VOLMET …) | [OurAirports `airport-frequencies.csv`](https://davidmegginson.github.io/ourairports-data/airport-frequencies.csv) + [`airports.csv`](https://davidmegginson.github.io/ourairports-data/airports.csv) (repo: [davidmegginson/ourairports-data](https://github.com/davidmegginson/ourairports-data)) | **Public Domain.** Primary bundled source. Join `airport-frequencies.csv` (`ident, type, description, frequency_mhz`) with `airports.csv` for coordinates/elevation. |
| **Amateur repeaters** (2m / 70cm, FM/DMR/D-STAR …) | [RepeaterBook](https://www.repeaterbook.com/) (official API) | **Do NOT bundle.** Bulk extraction / offline bundling / redistribution require written permission. Fetch at runtime with the **user's own API key**, cache locally, and show *"Data courtesy of RepeaterBook.com."* |
| **Marine VHF** | ITU marine VHF channel plan (static table) | Standardized channels; ship a small built-in table. |
| **Weather** | NOAA APT/LRPT satellite passes + NWS weather-radio frequencies (static table) | Public. |
| **User additions** | `extra-frequencies.csv` in your config dir | Free-form local overrides / private notes. |

### Build the DB

```bash
# Pulls the public sources and builds ./data/spyair.db
cargo run --bin build-db
```

### Schema (simplified)

```sql
CREATE TABLE channels (
  id            INTEGER PRIMARY KEY,
  freq_hz       INTEGER NOT NULL,
  mode          TEXT    NOT NULL,        -- AM | NFM | WFM | ...
  service       TEXT    NOT NULL,        -- TWR | GND | APP | CTR | ATIS | REPEATER | MARINE ...
  ident         TEXT,                    -- e.g. RJTT
  desc_en       TEXT,
  desc_jp       TEXT,
  lat           REAL,                    -- transmitter / facility location
  lon           REAL,
  elev_m        REAL,
  priority      INTEGER DEFAULT 0,
  source        TEXT                     -- ourairports | repeaterbook | builtin | user
);
CREATE INDEX idx_channels_geo  ON channels(lat, lon);
CREATE INDEX idx_channels_freq ON channels(freq_hz);
```

> Bilingual descriptions: OurAirports descriptions are English-leaning, so the build step maps
> service codes (`TWR`, `GND`, `APP`, `ATIS`, …) to canonical EN/JP strings, and falls back to the
> raw description for anything unmapped.

---

## Architecture

```
                 ┌──────────────┐
   GPS / IP ───► │   Locator    │ position (lat, lon, alt)
                 └──────┬───────┘
                        ▼
                 ┌──────────────┐   watchlist (scored, in-range)
                 │   Planner    │◄──── spyair.db (EN/JP descriptions)
                 └──────┬───────┘
                        ▼
  RTL-SDR ─► rtlsdr/soapysdr ──► ┌──────────────┐
                                 │   Scanner    │ tune + squelch + demod
                                 └──────┬───────┘
                     audio (PCM) ◄──────┤
                                        ├────────────► Audio sink (cpal/rodio)
                                        ├────────────► TUI (ratatui: waterfall + EQ)
                                        ├──(opt)─────► Whisper  ─► transcript log
                                        └──(opt)─────► Correlator ◄─ dump1090 (ADS-B)
                                                                     │
                                                                     ▼
                                                        flight + facility guess
```

### Stack (Rust)

- **Language:** Rust 1.78+
- **SDR I/O:** [`rtlsdr`](https://crates.io/crates/rtlsdr) or [`soapysdr`](https://crates.io/crates/soapysdr)
- **DSP:** [`num-complex`](https://crates.io/crates/num-complex), [`rustfft`](https://crates.io/crates/rustfft) (filtering, AM/FM demod, squelch)
- **TUI:** [`ratatui`](https://github.com/ratatui/ratatui) + [`crossterm`](https://crates.io/crates/crossterm)
- **Audio out:** [`cpal`](https://crates.io/crates/cpal) / [`rodio`](https://crates.io/crates/rodio)
- **DB:** [`rusqlite`](https://crates.io/crates/rusqlite) (bundled SQLite)
- **GPS (opt):** [`nmea`](https://crates.io/crates/nmea) over serial, or a `gpsd` client
- **Transcription (opt):** [`whisper-rs`](https://github.com/tazz4843/whisper-rs)
- **ADS-B (opt):** parse the JSON feed from [`dump1090`](https://github.com/flightaware/dump1090) / [`readsb`](https://github.com/wiedehopf/readsb)

---

## Hardware requirements

| Item | Notes |
|------|-------|
| **RTL-SDR dongle** | A quality dongle is strongly recommended. The **RTL-SDR Blog V4** has a TCXO (stable tuning) and a built-in HF upconverter. Buy from a reputable source — many cheap clones use weaker tuners and no shielding. |
| **Antenna** | Match it to your target band. Airband ATC is **~118–137 MHz (AM)**, so a 1/4-wave ground plane (~60 cm radials) or an airband-tuned antenna works well. The V4 ships with a dipole kit you can size for airband. |
| **(Optional) second dongle** | ADS-B lives at **1090 MHz**. To watch ATC audio *and* receive ADS-B at the same time, use a separate dongle for `dump1090`. |
| **(Optional) GPS receiver** | Any USB/serial NMEA GPS for automatic positioning. |
| **Host** | Any Linux/macOS machine; a Raspberry Pi 4/5 is plenty for the core scanner (Whisper is heavier — prefer a desktop/GPU or the `tiny`/`base` models on a Pi). |

**Where to buy / learn more about RTL-SDR:**

- Official store & guides: <https://www.rtl-sdr.com/store/>
- About / supported tuners & ranges: <https://www.rtl-sdr.com/about-rtl-sdr/>
- V3 user guide (direct sampling, bias-tee, etc.): <https://www.rtl-sdr.com/rtl-sdr-blog-v-3-dongles-user-guide/>

> Frequency coverage depends on the tuner. Typical R820T2-based dongles cover roughly
> **24 MHz – 1766 MHz**. RTL-SDR Blog **V3/V4** add HF (below ~24 MHz) via direct sampling /
> an upconverter. Airband and ADS-B are both comfortably inside the standard range.

---

## Installation

> ⚠️ Pre-alpha. These steps describe the intended setup; commands may change.

```bash
# 1. System dependencies (Debian/Ubuntu example)
sudo apt install rtl-sdr librtlsdr-dev libsoapysdr-dev libasound2-dev pkg-config

# 2. Toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 3. Clone & build
git clone https://github.com/m96-chan/spyair-sdr.git
cd spyair-sdr
cargo run --bin build-db        # build the frequency DB
cargo build --release

# Optional features
cargo build --release --features whisper   # transcription (needs whisper.cpp deps)
cargo build --release --features adsb       # flight correlation
```

Make sure the kernel DVB-T driver isn't grabbing the dongle:

```bash
echo 'blacklist dvb_usb_rtl28xxu' | sudo tee /etc/modprobe.d/blacklist-rtl.conf
```

---

## Usage

```bash
# Auto-detect location, build a watchlist, and start watching
spyair-sdr watch

# Force a location (skip GPS/IP)
spyair-sdr watch --lat 35.5494 --lon 139.7798 --alt 5

# Aviation profile only, with transcription and ADS-B correlation
spyair-sdr watch --profile airband --whisper --adsb

# Just show what you could hear from here, then exit
spyair-sdr plan
```

### Example config (`config.toml`)

```toml
[location]
source = "auto"          # auto | gps | ip | manual
lat = 35.5494
lon = 139.7798
alt_m = 5

[scanner]
profiles = ["airband", "marine", "ham"]
squelch_db = -25.0
hold_seconds = 2.5
priority = [121.500]     # aviation emergency / guard

[language]
mode = "both"            # en | jp | both

[whisper]
enabled = false
model = "base"           # tiny | base | small | medium
device = "cpu"           # cpu | cuda

[adsb]
enabled = false
source = "http://localhost:8080/data/aircraft.json"

[repeaterbook]
api_key = ""             # your own key; required to fetch ham repeaters
```

---

## Roadmap

- [ ] Locator (GPS / IP / manual) + radio-horizon range model
- [ ] DB build step (OurAirports → SQLite) + EN/JP service mapping
- [ ] Scanner: squelch, channel-hop, priority/lockout
- [ ] TUI: waterfall + equalizer + now-playing panel
- [ ] Audio sink + recording
- [ ] Whisper transcription pipeline
- [ ] ADS-B correlation → flight + facility guess
- [ ] RepeaterBook runtime fetch (user API key)
- [ ] Profiles beyond airband (marine, ham, weather, ACARS)

---

## Legal & ethical note

**Listening is not the same as sharing.** Laws differ by country. In many places it is legal to
*receive* most non-encrypted transmissions for personal use, but **recording, decoding,
re-transmitting, or disclosing the content of communications not addressed to you can be
restricted or illegal**. In Japan in particular, the Radio Act (電波法) protects the secrecy of
communications (秘密の保護). You are responsible for complying with the rules where you live —
this project is intended for personal monitoring, education, and experimentation only.

This is a **receive-only** tool. Don't transmit on frequencies you aren't licensed for.

### Data attribution

- Aviation data courtesy of **OurAirports** (Public Domain) — <https://ourairports.com/data/>
- Amateur repeater data, when enabled, courtesy of **RepeaterBook.com** (fetched at runtime with
  your own API key, per their terms) — <https://www.repeaterbook.com/>

---

## License

MIT © m96-chan
