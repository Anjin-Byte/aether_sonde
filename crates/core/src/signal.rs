//! Propagation primitives: signals, signal kinds, and source identifiers.
//!
//! A [`Signal`] is a finite-duration emission from a [`NodeId`] onto the
//! medium. Signals propagate over the topology's arcs (defined in a later
//! module) and are observed by other nodes as carrier-sense and
//! collision-detect events.
//!
//! # Formal model
//!
//! A transmission `σ = (κ, o, t_0, L_σ, R_σ)` has a kind `κ` (frame or jam),
//! origin port `o`, start time `t_0`, transmitted length `L_σ` in bits, and
//! bit rate `R_σ`. The transmitted duration is `D_σ = L_σ / R_σ`.
//!
//! The implementation precomputes the duration via [`Bits::at_rate`] and
//! stores it; `(L_σ, R_σ)` are not retained on the [`Signal`] itself, since
//! once a signal is on the wire its rate is a property of the segment, not
//! the signal.
//!
//! # Half-open intervals
//!
//! The time interval a signal occupies at its source is `[t_0, t_0 + D_σ)` —
//! closed on the left, open on the right. [`Signal::t_end`] returns the
//! exclusive endpoint.

use crate::time::{BitRate, BitTime, Bits};

// ---------------------------------------------------------------------------
// NodeId
// ---------------------------------------------------------------------------

/// Identifier of a node (end station, repeater, or bridge) in the topology.
///
/// `NodeId` is a `u32` newtype to make node references opaque and to prevent
/// accidental confusion with port or segment identifiers introduced in
/// later rounds.
///
/// # Placement note
///
/// `NodeId` lives in this module because [`Signal`] needs it before the
/// `topology` module exists. When `topology` lands (round 5), `NodeId` may
/// relocate to a dedicated `id` module if other identifiers (`PortId`,
/// `SegmentId`) accumulate. The relocation, if any, is internal to the
/// crate; the type is not part of any external API surface.
///
/// # Examples
///
/// ```
/// use aether_sonde::signal::NodeId;
/// let n = NodeId::new(7);
/// assert_eq!(n.as_u32(), 7);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(u32);

impl NodeId {
    /// Construct a `NodeId` from a raw `u32`.
    #[must_use]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// The underlying `u32`.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

// ---------------------------------------------------------------------------
// SignalKind
// ---------------------------------------------------------------------------

/// The kind of a propagated signal.
///
/// Frames carry user data; jams are emitted after a collision-detect event
/// to enforce the collision per IEEE 802.3 §"jam". This enum is publicly
/// exhaustive (no `#[non_exhaustive]`): adding a variant in a future
/// revision is a deliberate breaking change visible at every consumer's
/// `match`.
///
/// # Examples
///
/// ```
/// use aether_sonde::signal::SignalKind;
/// let kind = SignalKind::Frame;
/// assert_eq!(kind, SignalKind::Frame);
/// assert_ne!(kind, SignalKind::Jam);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignalKind {
    /// A user-data frame.
    Frame,
    /// A collision-enforcement jam.
    Jam,
}

// ---------------------------------------------------------------------------
// SignalError
// ---------------------------------------------------------------------------

/// Errors returned by fallible [`Signal`] constructors.
///
/// Marked `#[non_exhaustive]` so adding a variant in a future revision is
/// not a breaking change for consumers — error variants commonly accumulate
/// over time as new validation rules are codified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SignalError {
    /// A signal was constructed from zero bits.
    ///
    /// Every signal must have a strictly positive duration. A zero-bit
    /// input would produce a zero-duration signal whose occupancy interval
    /// is empty everywhere, which is a useless degenerate.
    ZeroBits,
}

impl core::fmt::Display for SignalError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ZeroBits => f.write_str("signal must have at least 1 bit"),
        }
    }
}

impl core::error::Error for SignalError {}

// ---------------------------------------------------------------------------
// Signal
// ---------------------------------------------------------------------------

/// A finite-duration signal emitted by a node onto the medium.
///
/// `Signal` is opaque: fields are private and are accessed via the
/// [`Signal::source`], [`Signal::t0`], [`Signal::duration`],
/// [`Signal::kind`], and [`Signal::t_end`] methods. Construction goes
/// through the typed [`Signal::frame`] and [`Signal::jam`] constructors
/// which enforce positive duration at the boundary.
///
/// # Examples
///
/// ```
/// use aether_sonde::signal::{NodeId, Signal, SignalKind};
/// use aether_sonde::time::{BitRate, BitTime, Bits};
///
/// let signal = Signal::frame(
///     NodeId::new(0),
///     BitTime::ZERO,
///     Bits::new(512),
///     BitRate::ETHERNET_10M,
/// ).unwrap();
///
/// // IEEE slotTime: 512 bits at 10 Mbps = 51.2 µs.
/// assert_eq!(signal.duration(), BitTime::from_nanos(51_200));
/// assert_eq!(signal.kind(), SignalKind::Frame);
/// assert_eq!(signal.t_end(), BitTime::from_nanos(51_200));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Signal {
    source: NodeId,
    t0: BitTime,
    duration: BitTime,
    kind: SignalKind,
}

impl Signal {
    /// Construct a frame [`Signal`].
    ///
    /// The duration is computed as `bits.at_rate(rate)`.
    ///
    /// # Errors
    ///
    /// Returns [`SignalError::ZeroBits`] if `bits == Bits::ZERO`. Invariant
    /// I5 forbids zero-duration signals.
    ///
    /// # Panics
    ///
    /// Panics if `bits.at_rate(rate)` overflows `u64` picoseconds. For any
    /// realistic Ethernet input this cannot happen — see [`Bits::at_rate`].
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::signal::{NodeId, Signal};
    /// use aether_sonde::time::{BitRate, BitTime, Bits};
    ///
    /// let s = Signal::frame(
    ///     NodeId::new(1),
    ///     BitTime::from_nanos(100),
    ///     Bits::new(96),
    ///     BitRate::ETHERNET_1G,
    /// ).unwrap();
    /// // 96 bits at 1 Gbps = 96 ns.
    /// assert_eq!(s.duration(), BitTime::from_nanos(96));
    /// ```
    pub const fn frame(
        source: NodeId,
        t0: BitTime,
        bits: Bits,
        rate: BitRate,
    ) -> Result<Self, SignalError> {
        if bits.as_u64() == 0 {
            return Err(SignalError::ZeroBits);
        }
        Ok(Self {
            source,
            t0,
            duration: bits.at_rate(rate),
            kind: SignalKind::Frame,
        })
    }

    /// Construct a jam [`Signal`].
    ///
    /// Used after collision detection to enforce the collision per IEEE
    /// 802.3 §"jam"; `bits` is typically `Bits::new(32)` (the standard
    /// `jamSize`).
    ///
    /// # Errors
    ///
    /// Returns [`SignalError::ZeroBits`] if `bits == Bits::ZERO`.
    ///
    /// # Panics
    ///
    /// Panics if `bits.at_rate(rate)` overflows `u64` picoseconds.
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::signal::{NodeId, Signal, SignalKind};
    /// use aether_sonde::time::{BitRate, BitTime, Bits};
    ///
    /// let j = Signal::jam(
    ///     NodeId::new(2),
    ///     BitTime::from_nanos(50),
    ///     Bits::new(32),
    ///     BitRate::ETHERNET_100M,
    /// ).unwrap();
    /// // jamSize 32 bits at 100 Mbps = 320 ns.
    /// assert_eq!(j.duration(), BitTime::from_nanos(320));
    /// assert_eq!(j.kind(), SignalKind::Jam);
    /// ```
    pub const fn jam(
        source: NodeId,
        t0: BitTime,
        bits: Bits,
        rate: BitRate,
    ) -> Result<Self, SignalError> {
        if bits.as_u64() == 0 {
            return Err(SignalError::ZeroBits);
        }
        Ok(Self {
            source,
            t0,
            duration: bits.at_rate(rate),
            kind: SignalKind::Jam,
        })
    }

    /// The node that emitted this signal.
    #[must_use]
    pub const fn source(self) -> NodeId {
        self.source
    }

    /// The start time of this signal, in [`BitTime`] from the simulation's
    /// time origin.
    #[must_use]
    pub const fn t0(self) -> BitTime {
        self.t0
    }

    /// The transmitted duration of this signal: `D_σ = L_σ / R_σ`.
    #[must_use]
    pub const fn duration(self) -> BitTime {
        self.duration
    }

    /// The signal's kind: [`SignalKind::Frame`] or [`SignalKind::Jam`].
    #[must_use]
    pub const fn kind(self) -> SignalKind {
        self.kind
    }

    /// The exclusive end time of this signal.
    ///
    /// Under the half-open interval convention, the signal occupies
    /// `[t_0, t_end())` at its source. Equivalent to
    /// `self.t0() + self.duration()`.
    ///
    /// # Panics
    ///
    /// Panics if `t0 + duration` overflows `u64` picoseconds (~213 days).
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::signal::{NodeId, Signal};
    /// use aether_sonde::time::{BitRate, BitTime, Bits};
    ///
    /// let s = Signal::frame(
    ///     NodeId::new(0),
    ///     BitTime::from_nanos(100),
    ///     Bits::new(96),
    ///     BitRate::ETHERNET_1G,
    /// ).unwrap();
    /// assert_eq!(s.t_end(), BitTime::from_nanos(196));
    /// ```
    #[must_use]
    #[track_caller]
    pub fn t_end(self) -> BitTime {
        self.t0 + self.duration
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Per [Result vs Panic]: unwrap and panic are allowed in tests."
)]
mod tests {
    use super::*;

    const SRC: NodeId = NodeId::new(7);

    // -- NodeId ---------------------------------------------------------------

    #[test]
    fn node_id_round_trips() {
        let n = NodeId::new(42);
        assert_eq!(n.as_u32(), 42);
        assert_eq!(NodeId::new(0), NodeId::new(0));
        assert_ne!(NodeId::new(0), NodeId::new(1));
    }

    // -- Signal::frame --------------------------------------------------------

    #[test]
    fn frame_duration_matches_bits_at_rate_for_every_standard_ethernet_rate() {
        // Sharp oracle: D_σ = L_σ / R_σ.
        let bits = Bits::new(1_500 * 8);
        for rate in [
            BitRate::ETHERNET_10M,
            BitRate::ETHERNET_100M,
            BitRate::ETHERNET_1G,
            BitRate::ETHERNET_10G,
        ] {
            let sig = Signal::frame(SRC, BitTime::ZERO, bits, rate).unwrap();
            assert_eq!(
                sig.duration(),
                bits.at_rate(rate),
                "frame duration should equal bits.at_rate(rate) for rate {} bps",
                rate.as_bps(),
            );
        }
    }

    #[test]
    fn frame_duration_matches_ieee_slot_times() {
        // Canonical IEEE 802.3 slotTime values.
        // 10 Mbps half duplex: slotTime = 512 bits = 51.2 µs.
        let slot_at_ten_mbps =
            Signal::frame(SRC, BitTime::ZERO, Bits::new(512), BitRate::ETHERNET_10M).unwrap();
        assert_eq!(slot_at_ten_mbps.duration(), BitTime::from_nanos(51_200));

        // 100 Mbps: slotTime = 512 bits = 5.12 µs.
        let slot_at_hundred_mbps =
            Signal::frame(SRC, BitTime::ZERO, Bits::new(512), BitRate::ETHERNET_100M).unwrap();
        assert_eq!(slot_at_hundred_mbps.duration(), BitTime::from_nanos(5_120));

        // 1 Gbps half duplex: slotTime = 4096 bits = 4.096 µs.
        let slot_at_one_gbps =
            Signal::frame(SRC, BitTime::ZERO, Bits::new(4_096), BitRate::ETHERNET_1G).unwrap();
        assert_eq!(slot_at_one_gbps.duration(), BitTime::from_nanos(4_096));
    }

    #[test]
    fn frame_records_source_t0_and_kind() {
        let s = Signal::frame(
            SRC,
            BitTime::from_micros(5),
            Bits::new(100),
            BitRate::ETHERNET_1G,
        )
        .unwrap();
        assert_eq!(s.source(), SRC);
        assert_eq!(s.t0(), BitTime::from_micros(5));
        assert_eq!(s.kind(), SignalKind::Frame);
    }

    #[test]
    fn frame_t_end_is_t0_plus_duration() {
        let s = Signal::frame(
            SRC,
            BitTime::from_nanos(100),
            Bits::new(96),
            BitRate::ETHERNET_1G,
        )
        .unwrap();
        // 96 bits at 1 Gbps = 96 ns; t_end = 100 + 96 = 196 ns.
        assert_eq!(s.t_end(), BitTime::from_nanos(196));
        assert_eq!(s.t_end(), s.t0() + s.duration());
    }

    #[test]
    fn frame_rejects_zero_bits() {
        let result = Signal::frame(SRC, BitTime::ZERO, Bits::ZERO, BitRate::ETHERNET_10M);
        assert_eq!(result, Err(SignalError::ZeroBits));
    }

    // -- Signal::jam ----------------------------------------------------------

    #[test]
    fn jam_duration_matches_ieee_jam_size_times() {
        // IEEE jamSize = 32 bits at every standard rate.
        // 10 Mbps: 32 bits = 3.2 µs.
        let jam_at_ten_mbps =
            Signal::jam(SRC, BitTime::ZERO, Bits::new(32), BitRate::ETHERNET_10M).unwrap();
        assert_eq!(jam_at_ten_mbps.duration(), BitTime::from_nanos(3_200));

        // 100 Mbps: 32 bits = 320 ns.
        let jam_at_hundred_mbps =
            Signal::jam(SRC, BitTime::ZERO, Bits::new(32), BitRate::ETHERNET_100M).unwrap();
        assert_eq!(jam_at_hundred_mbps.duration(), BitTime::from_nanos(320));

        // 1 Gbps: 32 bits = 32 ns.
        let jam_at_one_gbps =
            Signal::jam(SRC, BitTime::ZERO, Bits::new(32), BitRate::ETHERNET_1G).unwrap();
        assert_eq!(jam_at_one_gbps.duration(), BitTime::from_nanos(32));
    }

    #[test]
    fn jam_records_source_t0_and_kind() {
        let j = Signal::jam(
            SRC,
            BitTime::from_nanos(50),
            Bits::new(32),
            BitRate::ETHERNET_100M,
        )
        .unwrap();
        assert_eq!(j.source(), SRC);
        assert_eq!(j.t0(), BitTime::from_nanos(50));
        assert_eq!(j.kind(), SignalKind::Jam);
    }

    #[test]
    fn jam_rejects_zero_bits() {
        let result = Signal::jam(SRC, BitTime::ZERO, Bits::ZERO, BitRate::ETHERNET_10M);
        assert_eq!(result, Err(SignalError::ZeroBits));
    }

    // -- SignalKind sealed-enum discipline -----------------------------------

    #[test]
    fn signal_kind_is_exhaustively_matchable_without_wildcard() {
        // Adding a new SignalKind variant without updating this test
        // produces a compile error, not a silent fallthrough — sealed-enum
        // discipline applied so consumers benefit from exhaustive-match
        // breakage when the type changes.
        for kind in [SignalKind::Frame, SignalKind::Jam] {
            let label = match kind {
                SignalKind::Frame => "frame",
                SignalKind::Jam => "jam",
            };
            assert!(!label.is_empty());
        }
    }

    // -- SignalError ----------------------------------------------------------

    #[test]
    fn signal_error_displays_a_useful_message() {
        let msg = format!("{}", SignalError::ZeroBits);
        assert!(msg.contains("at least 1 bit"));
    }

    #[test]
    fn signal_error_implements_error_trait() {
        // Trait-object construction confirms the impl exists; the
        // assertion is structural (compiles), not behavioral.
        let err: &dyn core::error::Error = &SignalError::ZeroBits;
        assert!(err.source().is_none());
    }
}
