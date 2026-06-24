//! Canonical EN/JP descriptions for aviation service codes.
//!
//! OurAirports `airport-frequencies.csv` uses short `type` codes (`TWR`, `GND`, …). They are
//! English-leaning and inconsistent, so we map the well-known codes to canonical English and
//! Japanese strings. Unmapped codes fall back to the raw OurAirports description.

/// A bilingual description of a channel's service/function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceDescription {
    /// English description.
    pub en: String,
    /// Japanese description.
    pub jp: String,
}

/// Normalise a raw OurAirports service code to a canonical uppercase key.
///
/// Trims whitespace, uppercases, and folds a few common synonyms (e.g. `CENTER` → `CTR`).
fn normalize(code: &str) -> String {
    let up = code.trim().to_ascii_uppercase();
    match up.as_str() {
        "CENTER" | "CENTRE" | "ARTCC" | "CTL" => "CTR".to_string(),
        "TOWER" => "TWR".to_string(),
        "GROUND" => "GND".to_string(),
        "APPROACH" | "ARR" | "ARRIVAL" => "APP".to_string(),
        "DEPARTURE" => "DEP".to_string(),
        "DELIVERY" | "CLNC" | "CLNC DEL" | "CLEARANCE" => "CLD".to_string(),
        other => other.to_string(),
    }
}

/// Map a service code to its canonical EN/JP descriptions.
///
/// `raw_description` is the OurAirports free-text description, used as the fallback for codes we
/// do not have a canonical mapping for. Returns `None` only when the code is unmapped *and* the
/// raw description is empty.
pub fn describe(code: &str, raw_description: &str) -> Option<ServiceDescription> {
    let key = normalize(code);
    let canned: Option<(&str, &str)> = match key.as_str() {
        "TWR" => Some(("Tower", "管制塔")),
        "GND" => Some(("Ground", "地上管制")),
        "APP" => Some(("Approach", "進入管制")),
        "DEP" => Some(("Departure", "出発管制")),
        "CTR" => Some(("Center / Control", "管制区管制")),
        "ATIS" => Some((
            "ATIS (automatic terminal information)",
            "飛行場情報放送業務",
        )),
        "AWOS" => Some(("AWOS (automated weather)", "自動気象観測")),
        "ASOS" => Some(("ASOS (automated weather)", "自動気象観測")),
        "VOLMET" => Some(("VOLMET (in-flight weather)", "気象通報 (VOLMET)")),
        "CLD" => Some(("Clearance Delivery", "管制承認伝達")),
        "UNICOM" => Some(("UNICOM (advisory)", "ユニコム (運航情報)")),
        "CTAF" => Some(("CTAF (common traffic advisory)", "共通交通情報")),
        "MULTICOM" => Some(("MULTICOM", "マルチコム")),
        "RADAR" => Some(("Radar", "レーダー管制")),
        "FSS" => Some(("Flight Service Station", "飛行援助業務")),
        "EMERG" | "EMRG" | "GUARD" => Some(("Emergency / Guard", "緊急 / ガード")),
        _ => None,
    };

    match canned {
        Some((en, jp)) => Some(ServiceDescription {
            en: en.to_string(),
            jp: jp.to_string(),
        }),
        None => {
            let raw = raw_description.trim();
            if raw.is_empty() {
                None
            } else {
                // Unmapped: fall back to the raw description for both languages.
                Some(ServiceDescription {
                    en: raw.to_string(),
                    jp: raw.to_string(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_codes_bilingually() {
        let twr = describe("TWR", "ignored when mapped").unwrap();
        assert!(twr.en.contains("Tower"));
        assert!(twr.jp.contains("管制塔"));

        let atis = describe("ATIS", "").unwrap();
        assert!(atis.en.contains("ATIS"));
        assert!(atis.jp.contains("情報"));
    }

    #[test]
    fn folds_synonyms() {
        assert_eq!(describe("Center", "x"), describe("CTR", "x"));
        assert_eq!(describe("ground", "x"), describe("GND", "x"));
    }

    #[test]
    fn falls_back_to_raw_for_unmapped() {
        let d = describe("XYZ", "Special discrete").unwrap();
        assert_eq!(d.en, "Special discrete");
        assert_eq!(d.jp, "Special discrete");
    }

    #[test]
    fn none_when_unmapped_and_no_raw() {
        assert!(describe("XYZ", "   ").is_none());
    }
}
