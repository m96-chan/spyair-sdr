# `data/` — bundled frequency database

## `spyair.db`

A prebuilt, **airband-only** SQLite frequency database (VHF voice comms, 118.000–136.975 MHz),
ready to use offline. This is a convenience snapshot — it is fully regenerable with `build-db`.

| | |
|---|---|
| Channels | 27,699 (airband-filtered from 30,298 aviation comms) |
| Band | 118.000–136.975 MHz (AM) |
| Schema | see `channels` table in the top-level `README.md` |
| Source | **OurAirports** `airports.csv` + `airport-frequencies.csv` |
| Source URL | <https://ourairports.com/data/> (repo: `davidmegginson/ourairports-data`) |
| License | **Public Domain** — *Aviation data courtesy of OurAirports.* |
| Snapshot date | 2026-06-24 |

Each channel carries a bilingual (EN/JP) description derived from the OurAirports service code
(`TWR` → Tower / 管制塔, `ATIS` → … / 飛行場情報放送業務, …); unmapped codes fall back to the raw
OurAirports description.

## Rebuilding / updating

The snapshot goes stale as OurAirports updates. To regenerate from the latest public data:

```bash
# 1. Fetch the public sources (Public Domain)
curl -sSLO https://davidmegginson.github.io/ourairports-data/airports.csv
curl -sSLO https://davidmegginson.github.io/ourairports-data/airport-frequencies.csv

# 2. Build the airband database
cargo run --release --bin build-db -- \
  --airband \
  --airports airports.csv \
  --frequencies airport-frequencies.csv \
  --out data/spyair.db
```

Drop `--airband` to keep all aviation comms (HF/UHF included), or filter to a different band in
your own tooling via `Channel::in_band(min_hz, max_hz)`.
