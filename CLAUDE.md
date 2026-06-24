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
| `location` | ✅ (manual) | `LocationSource` trait; GPS/IP are `NotImplemented` stubs. |
| `freqdb` | ✅ | Service EN/JP mapping, OurAirports→SQLite, `build-db` binary. |
| `planner` | ✅ | Receivability scoring + in-range filtering. |
| `dsp` | ✅ | AM/FM demod, squelch with hysteresis. Pure math. |
| `scanner` | ✅ | Channel-hop/hold/priority/lockout state machine over a `trait SdrSource`. |
| `tui` | ✅ | `ratatui` `TestBackend` rendering (now-playing, watchlist, EQ/waterfall, device picker). |
| `sdr` | ✅ | Device model + selection policy + `SdrEnumerator` trait; real enumerator is a `NotImplemented` stub. |
| Hardware/external | 🔒 | RTL-SDR I/O, audio out, GPS, IP geo, Whisper, RepeaterBook, live ADS-B — trait + `NotImplemented`, tracked individually. |

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
```

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
