//! SQLite-backed channel store (`rusqlite`, bundled SQLite).
//!
//! Owns the schema from the README and provides bulk insert + read-back queries. Tests run
//! against an in-memory database — a real code path, not a mock.

use rusqlite::{params, Connection};

use crate::error::Result;

use super::{Channel, Mode, Source};

/// DDL for the `channels` table and its indexes (mirrors the README schema).
const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS channels (
  id            INTEGER PRIMARY KEY,
  freq_hz       INTEGER NOT NULL,
  mode          TEXT    NOT NULL,
  service       TEXT    NOT NULL,
  ident         TEXT,
  desc_en       TEXT,
  desc_jp       TEXT,
  lat           REAL,
  lon           REAL,
  elev_m        REAL,
  priority      INTEGER DEFAULT 0,
  source        TEXT
);
CREATE INDEX IF NOT EXISTS idx_channels_geo  ON channels(lat, lon);
CREATE INDEX IF NOT EXISTS idx_channels_freq ON channels(freq_hz);
";

/// A handle to the frequency database.
pub struct ChannelStore {
    conn: Connection,
}

impl ChannelStore {
    /// Open (or create) a store at `path`, ensuring the schema exists.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    /// Create an in-memory store (used by tests and ephemeral builds).
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(SCHEMA)?;
        Ok(())
    }

    /// Insert all `channels` in a single transaction. Returns the number inserted.
    pub fn insert_channels(&mut self, channels: &[Channel]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO channels
                   (freq_hz, mode, service, ident, desc_en, desc_jp, lat, lon, elev_m, priority, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            for c in channels {
                stmt.execute(params![
                    c.freq_hz,
                    c.mode.as_str(),
                    c.service,
                    c.ident,
                    c.desc_en,
                    c.desc_jp,
                    c.lat,
                    c.lon,
                    c.elev_m,
                    c.priority,
                    c.source.as_str(),
                ])?;
            }
        }
        tx.commit()?;
        Ok(channels.len())
    }

    /// Total number of channels stored.
    pub fn count(&self) -> Result<i64> {
        let n = self
            .conn
            .query_row("SELECT COUNT(*) FROM channels", [], |r| r.get(0))?;
        Ok(n)
    }

    /// Read back all channels (ordered by frequency) — primarily for tests/inspection.
    pub fn all_channels(&self) -> Result<Vec<Channel>> {
        self.query("SELECT freq_hz, mode, service, ident, desc_en, desc_jp, lat, lon, elev_m, priority, source \
                     FROM channels ORDER BY freq_hz", params![])
    }

    /// Channels matching a service code (e.g. `TWR`), ordered by frequency.
    pub fn by_service(&self, service: &str) -> Result<Vec<Channel>> {
        self.query(
            "SELECT freq_hz, mode, service, ident, desc_en, desc_jp, lat, lon, elev_m, priority, source \
             FROM channels WHERE service = ?1 ORDER BY freq_hz",
            params![service],
        )
    }

    fn query(&self, sql: &str, p: impl rusqlite::Params) -> Result<Vec<Channel>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(p, |row| {
            let mode = match row.get::<_, String>(1)?.as_str() {
                "NFM" => Mode::Nfm,
                "WFM" => Mode::Wfm,
                _ => Mode::Am,
            };
            let source = match row.get::<_, Option<String>>(10)?.as_deref() {
                Some("repeaterbook") => Source::RepeaterBook,
                Some("builtin") => Source::Builtin,
                Some("user") => Source::User,
                _ => Source::OurAirports,
            };
            Ok(Channel {
                freq_hz: row.get(0)?,
                mode,
                service: row.get(2)?,
                ident: row.get(3)?,
                desc_en: row.get(4)?,
                desc_jp: row.get(5)?,
                lat: row.get(6)?,
                lon: row.get(7)?,
                elev_m: row.get(8)?,
                priority: row.get(9)?,
                source,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Channel> {
        vec![
            Channel {
                freq_hz: 118_100_000,
                mode: Mode::Am,
                service: "TWR".into(),
                ident: Some("RJTT".into()),
                desc_en: Some("Tower".into()),
                desc_jp: Some("管制塔".into()),
                lat: Some(35.55),
                lon: Some(139.78),
                elev_m: Some(10.6),
                priority: 0,
                source: Source::OurAirports,
            },
            Channel {
                freq_hz: 121_700_000,
                mode: Mode::Am,
                service: "GND".into(),
                ident: Some("RJTT".into()),
                desc_en: Some("Ground".into()),
                desc_jp: Some("地上管制".into()),
                lat: Some(35.55),
                lon: Some(139.78),
                elev_m: Some(10.6),
                priority: 0,
                source: Source::OurAirports,
            },
        ]
    }

    #[test]
    fn insert_and_read_back_roundtrips() {
        let mut store = ChannelStore::in_memory().unwrap();
        let n = store.insert_channels(&sample()).unwrap();
        assert_eq!(n, 2);
        assert_eq!(store.count().unwrap(), 2);

        let all = store.all_channels().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0], sample()[0]);
        assert_eq!(all[1].mode, Mode::Am);
        assert_eq!(all[1].source, Source::OurAirports);
    }

    #[test]
    fn query_by_service_filters() {
        let mut store = ChannelStore::in_memory().unwrap();
        store.insert_channels(&sample()).unwrap();

        let twr = store.by_service("TWR").unwrap();
        assert_eq!(twr.len(), 1);
        assert_eq!(twr[0].service, "TWR");

        let gnd = store.by_service("GND").unwrap();
        assert_eq!(gnd.len(), 1);
        assert_eq!(gnd[0].freq_hz, 121_700_000);

        assert!(store.by_service("CTR").unwrap().is_empty());
    }
}
