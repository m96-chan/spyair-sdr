//! SDR device enumeration and selection.
//!
//! See issue #18. When more than one RTL-SDR is connected, "device 0" is not a safe default — the
//! user must be able to choose. This module owns the **device model** ([`SdrDeviceInfo`]), the
//! pure **resolution policy** ([`resolve`]) that turns a list of devices + a user selector into a
//! concrete choice (or an explicit "ambiguous" outcome), and the [`SdrEnumerator`] boundary.
//!
//! The resolution policy is pure and fully unit-tested with a `#[cfg(test)]` mock enumerator. The
//! real enumeration backend (librtlsdr / SoapySDR) requires hardware and returns
//! [`crate::error::Error::NotImplemented`] until a dongle is present (tracked in #10) — it never
//! fabricates a device list.

use crate::error::{Error, Result};
use crate::scanner::SdrSource;

/// Static facts about a connected SDR device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdrDeviceInfo {
    /// Zero-based device index as reported by the driver.
    pub index: u32,
    /// Human-readable device name (e.g. `"Generic RTL2832U OEM"`).
    pub name: String,
    /// Device serial string (stable across reboots; the preferred selector). May be empty if the
    /// driver reports none.
    pub serial: String,
    /// Tuner chip (e.g. `"R820T2"`), when known.
    pub tuner: String,
}

/// How the user asked to pick a device.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DeviceSelector {
    /// No explicit choice — auto-select if unambiguous, otherwise report [`Resolution::Ambiguous`].
    #[default]
    Auto,
    /// Select by device index.
    Index(u32),
    /// Select by serial string.
    Serial(String),
}

/// The outcome of resolving a [`DeviceSelector`] against the available devices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// A single device was chosen.
    Selected(SdrDeviceInfo),
    /// Multiple devices are present and none was specified — the caller must pick (e.g. prompt via
    /// the TUI picker, or error in a headless run).
    Ambiguous(Vec<SdrDeviceInfo>),
}

/// Errors from device selection. Module-local so the crate-wide `Error` stays untouched.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectError {
    /// No SDR devices are connected.
    #[error("no SDR devices found")]
    NoDevices,
    /// The selector did not match any connected device.
    #[error("no SDR device matches {selector:?}; available: {}", format_available(.available))]
    NotFound {
        /// The selector that failed to match.
        selector: DeviceSelector,
        /// The devices that *were* available, for a helpful message.
        available: Vec<SdrDeviceInfo>,
    },
    /// Two roles were assigned the same physical device.
    #[error("device {0} cannot be assigned to two roles at once")]
    RoleConflict(u32),
    /// The underlying enumeration backend failed (e.g. hardware not available).
    #[error("device enumeration failed: {0}")]
    Enumerate(String),
}

fn format_available(devices: &[SdrDeviceInfo]) -> String {
    if devices.is_empty() {
        return "(none)".to_string();
    }
    devices
        .iter()
        .map(|d| format!("[{}] {} (serial {})", d.index, d.name, d.serial))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Resolve a selector against a device list (pure).
///
/// Rules:
/// - empty list → [`SelectError::NoDevices`];
/// - [`DeviceSelector::Auto`] with exactly one device → [`Resolution::Selected`];
/// - [`DeviceSelector::Auto`] with several → [`Resolution::Ambiguous`] (never a silent pick);
/// - index/serial selector → the matching device, or [`SelectError::NotFound`].
pub fn resolve(
    devices: &[SdrDeviceInfo],
    selector: &DeviceSelector,
) -> std::result::Result<Resolution, SelectError> {
    if devices.is_empty() {
        return Err(SelectError::NoDevices);
    }
    match selector {
        DeviceSelector::Auto => {
            if devices.len() == 1 {
                Ok(Resolution::Selected(devices[0].clone()))
            } else {
                Ok(Resolution::Ambiguous(devices.to_vec()))
            }
        }
        DeviceSelector::Index(i) => devices
            .iter()
            .find(|d| d.index == *i)
            .cloned()
            .map(Resolution::Selected)
            .ok_or_else(|| SelectError::NotFound {
                selector: selector.clone(),
                available: devices.to_vec(),
            }),
        DeviceSelector::Serial(s) => devices
            .iter()
            .find(|d| &d.serial == s)
            .cloned()
            .map(Resolution::Selected)
            .ok_or_else(|| SelectError::NotFound {
                selector: selector.clone(),
                available: devices.to_vec(),
            }),
    }
}

/// Guard against assigning the same physical device to two roles (e.g. scanner *and* ADS-B).
///
/// Per-role wiring is a follow-up, but the conflict check exists now so callers can rely on it.
pub fn ensure_distinct(
    a: &SdrDeviceInfo,
    b: &SdrDeviceInfo,
) -> std::result::Result<(), SelectError> {
    if a.index == b.index || (!a.serial.is_empty() && a.serial == b.serial) {
        Err(SelectError::RoleConflict(a.index))
    } else {
        Ok(())
    }
}

/// The boundary for discovering and opening SDR devices.
///
/// Real implementations are hardware-backed; see [`RtlSdrEnumerator`]. Tests use a
/// `#[cfg(test)]`-only mock — never a production mock.
pub trait SdrEnumerator {
    /// List the connected devices.
    fn list(&self) -> Result<Vec<SdrDeviceInfo>>;
    /// Open a specific device into a tunable [`SdrSource`].
    fn open(&self, device: &SdrDeviceInfo) -> Result<Box<dyn SdrSource>>;
}

/// Convenience: enumerate, then resolve a selector — the normal entry point.
///
/// Returns [`Resolution::Ambiguous`] when the choice is genuinely undecidable; callers turn that
/// into a TUI prompt (interactive) or an error (headless).
pub fn select<E: SdrEnumerator>(
    enumerator: &E,
    selector: &DeviceSelector,
) -> std::result::Result<Resolution, SelectError> {
    let devices = enumerator
        .list()
        .map_err(|e| SelectError::Enumerate(e.to_string()))?;
    resolve(&devices, selector)
}

/// The real RTL-SDR enumeration backend (librtlsdr / SoapySDR). **Not implemented** — requires a
/// physical dongle; tracked in #10. Returns [`Error::NotImplemented`], never a fabricated list.
#[derive(Debug, Clone, Copy, Default)]
pub struct RtlSdrEnumerator;

impl SdrEnumerator for RtlSdrEnumerator {
    fn list(&self) -> Result<Vec<SdrDeviceInfo>> {
        Err(Error::NotImplemented(
            "RTL-SDR device enumeration requires librtlsdr + hardware",
        ))
    }

    fn open(&self, _device: &SdrDeviceInfo) -> Result<Box<dyn SdrSource>> {
        Err(Error::NotImplemented(
            "opening an RTL-SDR device requires librtlsdr + hardware",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::Iq;

    fn dev(index: u32, serial: &str) -> SdrDeviceInfo {
        SdrDeviceInfo {
            index,
            name: format!("RTL2832U #{index}"),
            serial: serial.to_string(),
            tuner: "R820T2".to_string(),
        }
    }

    /// A `#[cfg(test)]`-only enumerator returning a canned device list. Never compiled into a
    /// release build.
    struct MockEnumerator {
        devices: Vec<SdrDeviceInfo>,
    }

    /// A trivial `#[cfg(test)]`-only SdrSource so `open` can return something.
    struct MockSource;
    impl SdrSource for MockSource {
        fn tune(&mut self, _freq_hz: i64) -> Result<()> {
            Ok(())
        }
        fn read_block(&mut self) -> Result<Vec<Iq>> {
            Ok(vec![Iq::new(0.0, 0.0); 4])
        }
    }

    impl SdrEnumerator for MockEnumerator {
        fn list(&self) -> Result<Vec<SdrDeviceInfo>> {
            Ok(self.devices.clone())
        }
        fn open(&self, _device: &SdrDeviceInfo) -> Result<Box<dyn SdrSource>> {
            Ok(Box::new(MockSource))
        }
    }

    #[test]
    fn single_device_auto_selects() {
        let devices = vec![dev(0, "AAAA")];
        let r = resolve(&devices, &DeviceSelector::Auto).unwrap();
        assert_eq!(r, Resolution::Selected(dev(0, "AAAA")));
    }

    #[test]
    fn selector_by_index_and_serial_resolve() {
        let devices = vec![dev(0, "AAAA"), dev(1, "BBBB")];
        assert_eq!(
            resolve(&devices, &DeviceSelector::Index(1)).unwrap(),
            Resolution::Selected(dev(1, "BBBB"))
        );
        assert_eq!(
            resolve(&devices, &DeviceSelector::Serial("AAAA".into())).unwrap(),
            Resolution::Selected(dev(0, "AAAA"))
        );
    }

    #[test]
    fn multiple_devices_without_selector_is_ambiguous() {
        let devices = vec![dev(0, "AAAA"), dev(1, "BBBB")];
        let r = resolve(&devices, &DeviceSelector::Auto).unwrap();
        assert_eq!(r, Resolution::Ambiguous(devices));
    }

    #[test]
    fn unknown_selector_errors_with_available() {
        let devices = vec![dev(0, "AAAA")];
        let err = resolve(&devices, &DeviceSelector::Serial("ZZZZ".into())).unwrap_err();
        match err {
            SelectError::NotFound { available, .. } => assert_eq!(available, devices),
            other => panic!("expected NotFound, got {other:?}"),
        }
        // message lists what was available
        assert!(resolve(&devices, &DeviceSelector::Index(9))
            .unwrap_err()
            .to_string()
            .contains("AAAA"));
    }

    #[test]
    fn no_devices_errors() {
        assert_eq!(
            resolve(&[], &DeviceSelector::Auto).unwrap_err(),
            SelectError::NoDevices
        );
    }

    #[test]
    fn same_device_for_two_roles_conflicts() {
        assert!(ensure_distinct(&dev(0, "AAAA"), &dev(0, "AAAA")).is_err());
        assert!(ensure_distinct(&dev(0, "AAAA"), &dev(1, "BBBB")).is_ok());
        // same serial, different index reported → still a conflict
        assert!(ensure_distinct(&dev(0, "AAAA"), &dev(1, "AAAA")).is_err());
    }

    #[test]
    fn select_combines_list_and_resolve() {
        let e = MockEnumerator {
            devices: vec![dev(0, "AAAA")],
        };
        assert_eq!(
            select(&e, &DeviceSelector::Auto).unwrap(),
            Resolution::Selected(dev(0, "AAAA"))
        );
        // open() yields a usable source
        assert!(e.open(&dev(0, "AAAA")).is_ok());
    }

    #[test]
    fn real_enumerator_is_not_implemented() {
        assert!(matches!(
            RtlSdrEnumerator.list(),
            Err(Error::NotImplemented(_))
        ));
        assert!(matches!(
            RtlSdrEnumerator.open(&dev(0, "AAAA")),
            Err(Error::NotImplemented(_))
        ));
    }
}
