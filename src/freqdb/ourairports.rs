//! Ingest OurAirports CSV inputs into [`Channel`]s.
//!
//! Two inputs are joined on the airport identifier:
//! - `airport-frequencies.csv`: `id, airport_ref, airport_ident, type, description, frequency_mhz`
//! - `airports.csv`: `id, ident, type, name, latitude_deg, longitude_deg, elevation_ft, ...`
//!
//! The transform is pure (readers in, `Channel`s out) so it is fully unit-testable with in-memory
//! fixtures. The network download of the public CSVs is a separate, stubbed concern.

use std::collections::HashMap;
use std::io::Read;

use crate::error::{Error, Result};

use super::{service, Channel, Mode, Source};

/// Feet → metres.
const FT_TO_M: f64 = 0.3048;

/// Canonical upstream URLs for the public OurAirports CSVs.
pub const AIRPORTS_URL: &str = "https://davidmegginson.github.io/ourairports-data/airports.csv";
pub const FREQUENCIES_URL: &str =
    "https://davidmegginson.github.io/ourairports-data/airport-frequencies.csv";

/// Download the public OurAirports CSVs over the network.
///
/// **Stub — not implemented.** Fetching requires a runtime network client and is intentionally
/// not wired up here so the build never silently depends on the network. The offline build path
/// reads the CSVs from local files instead (see the `build-db` binary). Tracked under epic #1.
pub fn fetch_public_sources() -> Result<(String, String)> {
    Err(Error::NotImplemented(
        "OurAirports network download — provide --airports/--frequencies local CSV paths instead",
    ))
}

/// Geographic facts about an airport, keyed by `ident` during the join.
#[derive(Debug, Clone, Copy)]
struct AirportGeo {
    lat: Option<f64>,
    lon: Option<f64>,
    elev_m: Option<f64>,
}

/// Parse `airports.csv` into a lookup of `ident` → geographic facts.
pub fn parse_airports<R: Read>(reader: R) -> Result<AirportIndex> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(reader);
    let headers = rdr.headers()?.clone();
    let col = |name: &str| headers.iter().position(|h| h == name);

    let i_ident = col("ident");
    let i_lat = col("latitude_deg");
    let i_lon = col("longitude_deg");
    let i_elev = col("elevation_ft");

    let mut map = HashMap::new();
    for rec in rdr.records() {
        let rec = rec?;
        let Some(ident) = i_ident.and_then(|i| rec.get(i)) else {
            continue;
        };
        if ident.is_empty() {
            continue;
        }
        let parse_f = |idx: Option<usize>| -> Option<f64> {
            idx.and_then(|i| rec.get(i))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .and_then(|s| s.parse::<f64>().ok())
        };
        map.insert(
            ident.to_string(),
            AirportGeo {
                lat: parse_f(i_lat),
                lon: parse_f(i_lon),
                elev_m: parse_f(i_elev).map(|ft| ft * FT_TO_M),
            },
        );
    }
    Ok(AirportIndex { map })
}

/// A parsed `airports.csv`, ready to join against frequency rows.
#[derive(Debug, Default)]
pub struct AirportIndex {
    map: HashMap<String, AirportGeo>,
}

impl AirportIndex {
    /// Number of indexed airports.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Parse `airport-frequencies.csv` and join it against `airports` to produce channels.
///
/// Rows with a missing/zero/unparseable `frequency_mhz` are skipped. Aviation services are
/// demodulated as `AM`.
pub fn parse_frequencies<R: Read>(reader: R, airports: &AirportIndex) -> Result<Vec<Channel>> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(reader);
    let headers = rdr.headers()?.clone();
    let col = |name: &str| headers.iter().position(|h| h == name);

    let i_ident = col("airport_ident");
    let i_type = col("type");
    let i_desc = col("description");
    let i_freq = col("frequency_mhz");

    let mut out = Vec::new();
    for rec in rdr.records() {
        let rec = rec?;
        let get = |idx: Option<usize>| idx.and_then(|i| rec.get(i)).unwrap_or("").trim();

        let freq_mhz: f64 = match get(i_freq).parse::<f64>() {
            Ok(f) if f > 0.0 => f,
            _ => continue, // skip rows with no/invalid frequency
        };
        let freq_hz = (freq_mhz * 1_000_000.0).round() as i64;

        let service_code = get(i_type);
        let raw_desc = get(i_desc);
        let ident = get(i_ident);

        let desc = service::describe(service_code, raw_desc);
        let geo = airports.map.get(ident).copied();

        out.push(Channel {
            freq_hz,
            mode: Mode::Am, // OurAirports = aviation = AM
            service: service_code.to_string(),
            ident: (!ident.is_empty()).then(|| ident.to_string()),
            desc_en: desc.as_ref().map(|d| d.en.clone()),
            desc_jp: desc.as_ref().map(|d| d.jp.clone()),
            lat: geo.and_then(|g| g.lat),
            lon: geo.and_then(|g| g.lon),
            elev_m: geo.and_then(|g| g.elev_m),
            priority: 0,
            source: Source::OurAirports,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AIRPORTS_CSV: &str = "\
id,ident,type,name,latitude_deg,longitude_deg,elevation_ft,continent
1,RJTT,large_airport,Tokyo Haneda,35.5523,139.7798,35,AS
2,RJAA,large_airport,Narita,35.7647,140.3863,141,AS
";

    const FREQ_CSV: &str = "\
id,airport_ref,airport_ident,type,description,frequency_mhz
1,1,RJTT,TWR,Haneda Tower,118.1
2,1,RJTT,GND,Haneda Ground,121.7
3,2,RJAA,TWR,Narita Tower,118.2
4,2,RJAA,BADROW,No frequency,
5,2,RJAA,ZERO,Zero frequency,0
";

    #[test]
    fn joins_frequencies_with_airport_geo() {
        let airports = parse_airports(AIRPORTS_CSV.as_bytes()).unwrap();
        assert_eq!(airports.len(), 2);

        let channels = parse_frequencies(FREQ_CSV.as_bytes(), &airports).unwrap();
        // 5 rows, but the empty-freq and zero-freq rows are skipped → 3.
        assert_eq!(channels.len(), 3);

        let haneda_twr = &channels[0];
        assert_eq!(haneda_twr.freq_hz, 118_100_000);
        assert_eq!(haneda_twr.mode, Mode::Am);
        assert_eq!(haneda_twr.ident.as_deref(), Some("RJTT"));
        assert!(haneda_twr.desc_en.as_deref().unwrap().contains("Tower"));
        assert!(haneda_twr.desc_jp.as_deref().unwrap().contains("管制塔"));
        assert!((haneda_twr.lat.unwrap() - 35.5523).abs() < 1e-6);
        assert!((haneda_twr.lon.unwrap() - 139.7798).abs() < 1e-6);
        // 35 ft → ~10.668 m
        assert!((haneda_twr.elev_m.unwrap() - 10.668).abs() < 1e-3);
        assert_eq!(haneda_twr.source, Source::OurAirports);
    }

    #[test]
    fn skips_rows_without_valid_frequency() {
        let airports = parse_airports(AIRPORTS_CSV.as_bytes()).unwrap();
        let channels = parse_frequencies(FREQ_CSV.as_bytes(), &airports).unwrap();
        assert!(channels.iter().all(|c| c.freq_hz > 0));
        assert!(!channels.iter().any(|c| c.service == "BADROW"));
    }

    #[test]
    fn network_fetch_is_not_implemented() {
        assert!(matches!(
            fetch_public_sources(),
            Err(crate::error::Error::NotImplemented(_))
        ));
    }

    #[test]
    fn channel_with_unknown_airport_has_no_geo() {
        let airports = AirportIndex::default();
        let channels = parse_frequencies(FREQ_CSV.as_bytes(), &airports).unwrap();
        assert!(channels[0].lat.is_none());
        assert!(channels[0].elev_m.is_none());
        // description still resolves from the service code
        assert!(channels[0].desc_en.is_some());
    }
}
