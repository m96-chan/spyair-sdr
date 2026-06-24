# CLAUDE.md — working rules for `spyair-sdr`

These are the **authoritative rules** for any AI/agent (and humans) contributing to this
repository. Read this before writing code. When in doubt, this file wins over habit.

`spyair-sdr` is a location-aware RTL-SDR scanner (see `README.md` for the product vision and
architecture). It is being built **from scratch, test-first**.

---

## 1. Golden rules (non-negotiable)

1. **Issue first.** Every unit of work starts as a GitHub issue. Large efforts hang off the
   tracking epic (#1). The issue states scope, acceptance criteria, and what is *out of scope*.
2. **TDD: red → green → refactor.** Write the failing test first, make it pass with the
   simplest code, then refactor. No production code without a test that motivated it.
3. **Mock policy (hard rule).**
   - Mocks/fakes/stubs-that-return-data live **only** under `#[cfg(test)]`.
   - **Mocks must never run in production.** They must be impossible to wire into a release
     build (keep them inside `#[cfg(test)] mod tests`).
   - Every hardware/network boundary is a `trait`. Production uses a **real** implementation.
   - If a real implementation cannot run in the current environment, it returns
     **`Error::NotImplemented`** — it never fabricates, guesses, or silently fakes data.
4. **Be explicit about what can't be built.** Anything blocked by hardware or an external
   service is implemented as a trait + `NotImplemented` stub **and** recorded as an issue
   (see the matrix in epic #1). Never paper over a gap with a mock.
5. **Branch.** Develop on the assigned feature branch. Never push to `main` directly.
6. **Green before push.** `cargo test`, `cargo clippy --all-targets`, and `cargo fmt --check`
   must all pass before every push.

---

## 2. TDD workflow per slice

1. Cut (or update) the issue with scope + acceptance criteria.
2. Add failing unit tests next to the code (`#[cfg(test)] mod tests`).
3. Implement the minimum to go green.
4. `cargo fmt && cargo clippy --all-targets && cargo test`.
5. Commit with a descriptive message; push to the feature branch (updates the open PR).
6. Post a short progress comment on the PR: what landed, test count, and the
   implementable-vs-stubbed split for that slice. Close the issue.

A slice is **done** only when: tests pass, clippy+fmt clean, the PR comment is posted, and the
issue is closed (or explicitly left open with remaining work noted).

---

## 3. Architecture & module status

Pipeline (from `README.md`):

```
GPS/IP → Locator → Planner (← spyair.db) → Scanner → Audio / TUI / Whisper / ADS-B correlator
```

| Module | Status | Notes |
|--------|--------|-------|
| `error` | ✅ | Crate-wide `Error`, incl. `NotImplemented`. |
| `geo` | ✅ | `GeoPosition`, haversine, VHF radio-horizon model. |
| `location` | ✅ (manual) | `LocationSource` trait; live GPS/IP backends are `NotImplemented` stubs. |
| `gps` | ✅ | NMEA 0183 parsing + fix resolution; serial/device I/O is a `NotImplemented` stub. |
| `ipgeo` | ✅ | IP-geolocation response parsing + provider boundary; HTTP transport is a `NotImplemented` stub. |
| `freqdb` | ✅ | Service EN/JP mapping, OurAirports→SQLite, `build-db` binary. |
| `planner` | ✅ | Receivability scoring + in-range filtering. |
| `dsp` | ✅ | AM/FM demod, squelch with hysteresis, `power_spectrum` (pure radix-2 FFT). Pure math. |
| `scanner` | ✅ | Channel-hop/hold/priority/lockout state machine over a `trait SdrSource`. |
| `adsb` | ✅ | Pure dump1090 `aircraft.json` parse + ATC-frequency correlation heuristic; live HTTP fetch is out of scope (issue #16). |
| `audio` | ✅ | `AudioSink` trait + WAV recorder; real device playback is a `NotImplemented` stub. |
| `tui` | ✅ | `ratatui` `TestBackend` rendering (now-playing, watchlist, EQ/waterfall, device picker). |
| `sdr` | ✅ | Device model + selection policy + `SdrEnumerator`. **Real librtlsdr I/O backend** (`RtlSdrDevice`/enumerator) behind the `rtlsdr` Cargo feature; default build keeps `NotImplemented` stubs + the pure `decode_rtl_iq`. |
| `bin/spyair-tui` | ✅ | Live terminal dashboard. Demo mode by default; **live mode** (real spectrum) under `--features rtlsdr -- --freq <MHz>`. |
| Hardware/external | 🔒 | Audio out, GPS, IP geo, Whisper, RepeaterBook, live ADS-B — trait + `NotImplemented`, tracked individually. (RTL-SDR I/O is now implemented behind the `rtlsdr` feature.) |

---

## 4. Commands

```bash
cargo test                       # all unit tests (must pass)
cargo clippy --all-targets       # must be warning-free
cargo fmt                        # format (CI checks --check)
cargo run --bin build-db -- \
  --airports airports.csv \
  --frequencies airport-frequencies.csv \
  --out data/spyair.db           # build the local frequency DB (offline path)

cargo run --bin spyair-tui                                   # TUI dashboard (demo data)
cargo run --bin spyair-tui --features rtlsdr -- --freq 82.5  # live spectrum from a real dongle
cargo build --features rtlsdr    # compile the real librtlsdr I/O backend (needs system librtlsdr)
```

---

## 4a. Status & remaining work (updated 2026-06-25)

Working end-to-end on real hardware: a live RTL-SDR can be opened, tuned, and its
spectrum shown in `spyair-tui` (verified on FM broadcast). The real I/O backend lives
behind the `rtlsdr` feature; the default build stays pure + stubbed.

Done so far: error, geo, location, gps (parse), ipgeo (parse), freqdb, planner, dsp
(+`power_spectrum`), scanner, adsb (parse+correlate), audio (recorder + stub), tui,
sdr (selection + **real librtlsdr backend**), `spyair-tui` (demo + live spectrum).

Remaining / next slices:
- **Live ADS-B validation** of the `adsb` module against real data — blocked: the local
  1090 MHz feed was dead overnight (only Mode-AC, no DF17). Retry by day; do **not**
  fabricate aircraft data. (issue #16 covers the live fetch boundary.)
- **Reception proof for Tokyo Approach 120.5/120.8 MHz** — needs a daytime capture
  (overnight airband is silent); tuning/capture itself already works.
- **Scanner wiring**: drive the hop/hold state machine from a live `SdrSource` and surface
  it in the TUI (watchlist activity, now-playing).
- **Audio output** (issue #11): real `AudioSink` (cpal/rodio) so demodulated audio is audible.
- **`power_spectrum` absolute/AGC normalisation** mode (current mode is per-frame-peak, so a
  flat-noise frame reads as near-full bars).
- **TUI live device picker** wired to the real `RtlSdrEnumerator` (under the feature).
- Still hardware/external-blocked: GPS, IP geo, Whisper (#14), RepeaterBook (#15), live ADS-B (#16).

---

## 5. Code conventions

- **Errors:** return `crate::error::Result<T>`; add a variant to `Error` rather than
  stringly-typed panics. Reserve `NotImplemented(&'static str)` for genuine hardware/external
  gaps, with a message that tells the user what to do instead.
- **Units:** SI internally — Hz for frequency, metres for altitude, km for distances. Convert at
  the edges (MHz/ft) and document the conversion.
- **Purity:** keep domain logic (math, parsing, state machines) free of I/O so it stays unit
  testable. Push I/O to thin trait implementations at the boundary.
- **Docs:** every public item has a doc comment. Note in the doc when something is a stub.
- **No `unwrap()`/`expect()`** in library code paths (tests may use them).

---

## 6. Commit & PR

- Conventional, descriptive commit subjects; body explains the *why* and notes the test count.
- Do **not** open a new PR unless asked — push to the feature branch to update the existing PR.
- Keep PR comments frugal: one update per landed slice, not per command.

---

## 7. Sub-agents & skills

Project-specific agents and how to use them are documented in `SKILLS.md`. The primary one is
**`rust-tdd-engineer`** (`.claude/agents/rust-tdd-engineer.md`) — use it to implement a slice
test-first under the rules above.
