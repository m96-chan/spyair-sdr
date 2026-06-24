//! The Planner: turn the channel database + a receiver position into a scored,
//! in-range [`Watchlist`].
//!
//! Given a receiver [`GeoPosition`] and a set of candidate [`Channel`]s, the planner:
//!
//! 1. **Filters by radio range.** For channels that carry transmitter coordinates
//!    (`lat`/`lon`, with `elev_m` defaulting to ground level when absent), it builds the
//!    transmitter position and drops any that fall outside the shared VHF radio horizon
//!    (see [`crate::geo::radio_horizon_km`]).
//! 2. **Scores receivability.** Channels closer to the receiver (relative to their own radio
//!    horizon) score higher; the channel's `priority` and a per-service weight
//!    (emergency/guard > tower/approach > ATIS/info) further lift the score.
//! 3. **Honours priority frequencies.** A configured set of priority frequencies is forced to
//!    the top of the list, ahead of every non-priority channel, regardless of score.
//!
//! ## No-coordinate policy
//! Channels with no usable coordinates (`lat`/`lon` not both present) **cannot** be proven
//! out-of-range, so they are **kept** rather than dropped — silently discarding them would lose
//! real signals (e.g. en-route/centre frequencies, marine, weather radio with no fixed point).
//! They are flagged [`ScoredChannel::range_unknown`] `== true` and scored conservatively low (no
//! distance bonus), so geographically-confirmed channels naturally rank above them. They are
//! never excluded by the range filter.
//!
//! ## Units
//! All frequencies are **hertz** (`i64`), matching [`Channel::freq_hz`] and the crate-wide SI
//! convention. The priority-frequency list passed to [`plan`] / [`plan_from_store`] is therefore
//! `&[i64]` in Hz. Distances are kilometres and altitudes metres, as in [`crate::geo`].

use crate::freqdb::store::ChannelStore;
use crate::freqdb::Channel;
use crate::geo::{radio_horizon_km, GeoPosition};

/// Errors raised while planning a watchlist.
///
/// Module-local on purpose: the planner does not touch hardware or external services, so it does
/// not need the crate-wide `Error::NotImplemented` machinery. Storage-backed planning surfaces
/// the underlying [`crate::error::Error`] via [`PlannerError::Store`].
#[derive(Debug)]
pub enum PlannerError {
    /// The receiver position was invalid (non-finite or out of range).
    InvalidReceiver(crate::error::Error),
    /// A query against the [`ChannelStore`] failed.
    Store(crate::error::Error),
}

impl std::fmt::Display for PlannerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlannerError::InvalidReceiver(e) => write!(f, "invalid receiver position: {e}"),
            PlannerError::Store(e) => write!(f, "channel store error: {e}"),
        }
    }
}

impl std::error::Error for PlannerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PlannerError::InvalidReceiver(e) | PlannerError::Store(e) => Some(e),
        }
    }
}

/// Result alias for planner operations.
pub type Result<T> = std::result::Result<T, PlannerError>;

/// A channel paired with its computed receivability score and supporting facts.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredChannel {
    /// The channel itself.
    pub channel: Channel,
    /// Receivability score in `[0.0, ∞)`. Higher is better. Priority frequencies are not given
    /// an inflated score; they are instead ordered ahead of everything else (see [`is_priority`]).
    pub score: f64,
    /// Whether this channel was matched against the configured priority-frequency list.
    pub is_priority: bool,
    /// `true` when the channel carries no usable coordinates and therefore could not be
    /// range-checked (kept per the module's no-coordinate policy).
    pub range_unknown: bool,
    /// Surface distance from the receiver to the transmitter in kilometres, when coordinates were
    /// available. `None` for channels with [`range_unknown`](ScoredChannel::range_unknown) `== true`.
    pub distance_km: Option<f64>,
}

/// An ordered list of receivable channels, highest-ranked first.
///
/// Ordering: all priority-frequency matches first (themselves ordered by score), then the
/// remaining channels by descending score. Out-of-range channels are not present.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Watchlist {
    /// The scored channels, already ordered (best first).
    pub entries: Vec<ScoredChannel>,
}

impl Watchlist {
    /// Number of channels in the watchlist.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the watchlist is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over the scored channels in rank order (best first).
    pub fn iter(&self) -> std::slice::Iter<'_, ScoredChannel> {
        self.entries.iter()
    }
}

/// Per-service receivability weight: how much we prioritise a service when it is otherwise
/// receivable. Emergency/guard outranks active ATC, which outranks informational broadcasts.
///
/// Unknown services get a neutral baseline weight. The returned value is a multiplier applied to
/// the base receivability score (see [`score_channel`]).
fn service_weight(service: &str) -> f64 {
    match service.trim().to_ascii_uppercase().as_str() {
        // Emergency / guard — always the highest operational interest.
        "EMERG" | "EMRG" | "GUARD" => 3.0,
        // Active air-traffic control.
        "TWR" | "APP" | "DEP" | "GND" | "CTR" | "RADAR" => 2.0,
        // Advisory / common-traffic services.
        "UNICOM" | "CTAF" | "MULTICOM" | "FSS" => 1.5,
        // Informational / automated broadcasts.
        "ATIS" | "AWOS" | "ASOS" | "VOLMET" | "CLD" => 1.0,
        // Anything else: neutral baseline.
        _ => 1.2,
    }
}

/// Score factor in `[0, 1]` derived from how close the transmitter is relative to its radio
/// horizon. `1.0` at the receiver, falling linearly to `0.0` at the horizon edge.
///
/// `distance_km` and `horizon_km` are kilometres. A non-positive horizon yields `0.0`.
fn proximity_factor(distance_km: f64, horizon_km: f64) -> f64 {
    if horizon_km <= 0.0 {
        return 0.0;
    }
    let frac = 1.0 - (distance_km / horizon_km);
    frac.clamp(0.0, 1.0)
}

/// Compute a receivability score for one (already in-range) channel.
///
/// Combines the per-service weight, the channel's `priority` (each unit adds a modest bonus), and
/// the proximity factor. Channels with unknown range get the base service/priority score without
/// any proximity contribution, keeping them below geographically-confirmed close channels.
fn score_channel(channel: &Channel, proximity: Option<f64>) -> f64 {
    let weight = service_weight(&channel.service);
    // Priority is an integer knob from the DB; clamp negatives to 0 so it can only help.
    let priority_bonus = 1.0 + 0.25 * (channel.priority.max(0) as f64);
    let base = weight * priority_bonus;
    match proximity {
        // In-range with coordinates: scale by proximity, but keep a small floor so a barely
        // in-range high-value service still beats a far weaker one.
        Some(p) => base * (0.25 + 0.75 * p),
        // Range unknown: conservative — base value only, no proximity boost.
        None => base * 0.25,
    }
}

/// Build the transmitter position from a channel's coordinates, if both lat and lon are present.
///
/// `elev_m` defaults to `0.0` (ground level) when absent. Returns `None` when the channel has no
/// usable coordinates, or when the coordinates are invalid (which [`GeoPosition::new`] rejects).
fn transmitter_position(channel: &Channel) -> Option<GeoPosition> {
    match (channel.lat, channel.lon) {
        (Some(lat), Some(lon)) => {
            let elev = channel.elev_m.unwrap_or(0.0);
            GeoPosition::new(lat, lon, elev).ok()
        }
        _ => None,
    }
}

/// Plan a [`Watchlist`] from a receiver position and a slice of candidate channels.
///
/// `priority_freqs_hz` is a set of frequencies in **hertz** that must surface ahead of all
/// non-priority channels (see the module docs for the policy and unit rationale).
///
/// # Errors
/// Returns [`PlannerError::InvalidReceiver`] if `receiver` is not a valid [`GeoPosition`].
pub fn plan(
    receiver: &GeoPosition,
    channels: &[Channel],
    priority_freqs_hz: &[i64],
) -> Result<Watchlist> {
    // Validate the receiver by round-tripping through the constructor.
    GeoPosition::new(receiver.lat, receiver.lon, receiver.alt_m)
        .map_err(PlannerError::InvalidReceiver)?;

    let mut scored: Vec<ScoredChannel> = Vec::new();

    for channel in channels {
        let is_priority = priority_freqs_hz.contains(&channel.freq_hz);

        match transmitter_position(channel) {
            Some(tx) => {
                let distance = receiver.distance_km(&tx);
                let horizon = radio_horizon_km(receiver.alt_m, tx.alt_m);
                // Out of radio range: exclude entirely.
                if distance > horizon {
                    continue;
                }
                let proximity = proximity_factor(distance, horizon);
                let score = score_channel(channel, Some(proximity));
                scored.push(ScoredChannel {
                    channel: channel.clone(),
                    score,
                    is_priority,
                    range_unknown: false,
                    distance_km: Some(distance),
                });
            }
            None => {
                // No coordinates: keep, flag range unknown, score conservatively (policy above).
                let score = score_channel(channel, None);
                scored.push(ScoredChannel {
                    channel: channel.clone(),
                    score,
                    is_priority,
                    range_unknown: true,
                    distance_km: None,
                });
            }
        }
    }

    // Order: priority frequencies first, then by descending score. Ties fall back to frequency
    // for a deterministic, stable order.
    scored.sort_by(|a, b| {
        b.is_priority
            .cmp(&a.is_priority)
            .then_with(|| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.channel.freq_hz.cmp(&b.channel.freq_hz))
    });

    Ok(Watchlist { entries: scored })
}

/// Plan a [`Watchlist`] by reading all candidate channels from a [`ChannelStore`].
///
/// Convenience wrapper over [`plan`]: pulls every channel via [`ChannelStore::all_channels`] and
/// applies the same scoring and filtering. `priority_freqs_hz` is in **hertz**.
///
/// # Errors
/// Returns [`PlannerError::Store`] if the query fails, or [`PlannerError::InvalidReceiver`] if the
/// receiver position is invalid.
pub fn plan_from_store(
    receiver: &GeoPosition,
    store: &ChannelStore,
    priority_freqs_hz: &[i64],
) -> Result<Watchlist> {
    let channels = store.all_channels().map_err(PlannerError::Store)?;
    plan(receiver, &channels, priority_freqs_hz)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::freqdb::{Mode, Source};

    /// Build a minimal channel fixture; coordinates are optional so tests can exercise the
    /// no-coordinate policy.
    fn chan(
        freq_hz: i64,
        service: &str,
        priority: i64,
        coords: Option<(f64, f64, f64)>,
    ) -> Channel {
        let (lat, lon, elev_m) = match coords {
            Some((la, lo, e)) => (Some(la), Some(lo), Some(e)),
            None => (None, None, None),
        };
        Channel {
            freq_hz,
            mode: Mode::Am,
            service: service.into(),
            ident: None,
            desc_en: None,
            desc_jp: None,
            lat,
            lon,
            elev_m,
            priority,
            source: Source::OurAirports,
        }
    }

    fn rx() -> GeoPosition {
        // 5 m ground receiver near Tokyo.
        GeoPosition::new(35.55, 139.78, 5.0).unwrap()
    }

    #[test]
    fn excludes_out_of_range_channels() {
        // Co-located tower (in range) vs. a sea-level station ~150 km away (out of the ~18 km
        // ground-to-ground horizon).
        let near = chan(118_100_000, "TWR", 0, Some((35.55, 139.78, 10.0)));
        let far = chan(120_000_000, "TWR", 0, Some((36.90, 139.78, 5.0)));

        let wl = plan(&rx(), &[near.clone(), far.clone()], &[]).unwrap();

        assert_eq!(wl.len(), 1, "the distant channel must be filtered out");
        assert_eq!(wl.entries[0].channel.freq_hz, near.freq_hz);
    }

    #[test]
    fn closer_channel_ranks_above_distant_in_range_channel() {
        // Both in range (each within the shared horizon), one much closer than the other.
        let close = chan(118_100_000, "TWR", 0, Some((35.55, 139.78, 50.0)));
        // ~7 km away but still within the horizon thanks to a tall mast.
        let farther = chan(119_100_000, "TWR", 0, Some((35.61, 139.78, 200.0)));

        let wl = plan(&rx(), &[farther.clone(), close.clone()], &[]).unwrap();

        assert_eq!(wl.len(), 2);
        assert_eq!(
            wl.entries[0].channel.freq_hz, close.freq_hz,
            "the closer transmitter should rank first"
        );
    }

    #[test]
    fn higher_priority_channel_ranks_above_low_priority_at_same_distance() {
        let coords = Some((35.55, 139.78, 10.0));
        let low = chan(118_100_000, "ATIS", 0, coords);
        let high = chan(119_100_000, "ATIS", 5, coords);

        let wl = plan(&rx(), &[low.clone(), high.clone()], &[]).unwrap();

        assert_eq!(wl.entries[0].channel.freq_hz, high.freq_hz);
    }

    #[test]
    fn emergency_service_outranks_info_at_same_distance() {
        let coords = Some((35.55, 139.78, 10.0));
        let info = chan(118_100_000, "ATIS", 0, coords);
        let emerg = chan(121_500_000, "EMERG", 0, coords);

        let wl = plan(&rx(), &[info.clone(), emerg.clone()], &[]).unwrap();

        assert_eq!(wl.entries[0].channel.freq_hz, emerg.freq_hz);
    }

    #[test]
    fn no_coordinate_channels_are_kept_and_flagged() {
        let nocoord = chan(123_450_000, "CTR", 0, None);
        let wl = plan(&rx(), std::slice::from_ref(&nocoord), &[]).unwrap();

        assert_eq!(wl.len(), 1, "no-coordinate channels are kept, not dropped");
        assert!(wl.entries[0].range_unknown);
        assert_eq!(wl.entries[0].distance_km, None);
    }

    #[test]
    fn no_coordinate_channel_ranks_below_close_confirmed_channel() {
        // A strong, close, geographically-confirmed channel should beat an unknown-range one of
        // the same service/priority.
        let close = chan(118_100_000, "CTR", 0, Some((35.55, 139.78, 10.0)));
        let unknown = chan(123_450_000, "CTR", 0, None);

        let wl = plan(&rx(), &[unknown.clone(), close.clone()], &[]).unwrap();

        assert_eq!(wl.entries[0].channel.freq_hz, close.freq_hz);
        assert!(wl.entries[1].range_unknown);
    }

    #[test]
    fn priority_frequencies_surface_at_the_top() {
        // A low-value, distant-but-in-range info channel that would normally rank last…
        let info_far = chan(118_100_000, "ATIS", 0, Some((35.61, 139.78, 200.0)));
        // …vs. several strong ATC channels.
        let twr = chan(119_100_000, "TWR", 0, Some((35.55, 139.78, 10.0)));
        let app = chan(120_100_000, "APP", 0, Some((35.55, 139.78, 10.0)));

        // Mark the otherwise-weak info channel as a configured priority frequency (Hz).
        let priorities = [info_far.freq_hz];
        let wl = plan(
            &rx(),
            &[twr.clone(), app.clone(), info_far.clone()],
            &priorities,
        )
        .unwrap();

        assert_eq!(wl.len(), 3);
        assert_eq!(
            wl.entries[0].channel.freq_hz, info_far.freq_hz,
            "configured priority frequency must surface at the top despite a low score"
        );
        assert!(wl.entries[0].is_priority);
        // The non-priority channels follow, and are not flagged as priority.
        assert!(!wl.entries[1].is_priority);
        assert!(!wl.entries[2].is_priority);
    }

    #[test]
    fn out_of_range_priority_frequency_is_still_excluded() {
        // Being a priority frequency does not override physics: an out-of-range transmitter with
        // coordinates is still filtered out.
        let far = chan(121_500_000, "EMERG", 9, Some((36.90, 139.78, 5.0)));
        let wl = plan(&rx(), std::slice::from_ref(&far), &[far.freq_hz]).unwrap();
        assert!(
            wl.is_empty(),
            "a priority frequency that is provably out of range is still excluded"
        );
    }

    #[test]
    fn rejects_invalid_receiver() {
        let bad = GeoPosition {
            lat: 999.0,
            lon: 0.0,
            alt_m: 0.0,
        };
        let err = plan(&bad, &[], &[]).unwrap_err();
        assert!(matches!(err, PlannerError::InvalidReceiver(_)));
    }

    #[test]
    fn plans_from_an_in_memory_store() {
        let mut store = ChannelStore::in_memory().unwrap();
        let near = chan(118_100_000, "TWR", 0, Some((35.55, 139.78, 10.0)));
        let far = chan(120_000_000, "TWR", 0, Some((36.90, 139.78, 5.0)));
        store.insert_channels(&[near.clone(), far.clone()]).unwrap();

        let wl = plan_from_store(&rx(), &store, &[]).unwrap();

        assert_eq!(
            wl.len(),
            1,
            "store-backed planning applies the range filter"
        );
        assert_eq!(wl.entries[0].channel.freq_hz, near.freq_hz);
    }

    #[test]
    fn watchlist_is_empty_for_no_candidates() {
        let wl = plan(&rx(), &[], &[]).unwrap();
        assert!(wl.is_empty());
        assert_eq!(wl.len(), 0);
        assert_eq!(wl.iter().count(), 0);
    }
}
