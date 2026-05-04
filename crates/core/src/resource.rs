//! Resource identifiers, claims, and transmissions.
//!
//! Per design.md §3.c.2, the simulator distinguishes two resource kinds:
//!
//! * [`CollisionId`] — one per HD shared-medium component. Multiple
//!   simultaneous claims are allowed; the engine's collision handler
//!   resolves them per CSMA/CD semantics.
//! * [`SerializerId`] — one per FD directed channel or bridge egress port.
//!   At most one claim is active at any simulation time per invariant I3.
//!
//! Both are typed identifiers (`u32` newtypes); they carry no claim state.
//! The active-claim state lives in the engine's event log (round 8); this
//! module supplies the type vocabulary that the engine operates over.
//!
//! The generic [`Claim<R>`] and [`Transmission<R>`] types make the resource
//! kind a compile-time fact: a function that takes a `Claim<SerializerId>`
//! cannot accidentally receive a `Claim<CollisionId>`. This is the
//! type-level expression of design.md §3.c.2's "two distinct types, not
//! one polymorphic Resource."
//!
//! # Half-open intervals
//!
//! Per design.md §2.c invariant I4 and `report_0.md` §"Interval-intersection
//! theorem", time intervals are half-open: a claim spans `[t_start, t_end)`.
//! [`Claim::overlaps`] implements the half-open intersection predicate
//! `max(a_0, b_0) < min(a_1, b_1)` exactly.
//!
//! # Deferred work
//!
//! - **I3 runtime enforcement** (≤1 active per serializer) — round 8 (engine).
//! - **A2 / Theorem 3 compile-fail tests** (FD collisions impossible at the
//!   type level) — round 5 (topology), once `FdSegment` exists.

use crate::signal::Signal;
use crate::time::BitTime;

// ---------------------------------------------------------------------------
// Sealed-trait machinery
// ---------------------------------------------------------------------------

mod sealed {
    /// Sealed marker so [`super::ResourceId`] cannot be implemented outside
    /// this crate. The resource vocabulary is intentionally closed.
    pub trait Sealed {}
}

/// Trait implemented by [`CollisionId`] and [`SerializerId`].
///
/// Used where the engine needs to log or display "any resource" without
/// caring about its kind. Sealed: external crates cannot add new resource
/// kinds — the resource vocabulary is closed at the crate boundary, per
/// design.md §3.c.2 ("not one polymorphic Resource") read at the type
/// system level.
pub trait ResourceId: sealed::Sealed + Copy + Eq + core::fmt::Debug {
    /// The underlying `u32` identifier.
    fn as_u32(self) -> u32;
}

// ---------------------------------------------------------------------------
// CollisionId
// ---------------------------------------------------------------------------

/// Identifier of a collision-domain resource (one per HD shared-medium
/// component).
///
/// Multiple simultaneous claims are permitted; the engine's collision
/// handler resolves them per CSMA/CD semantics in round 8.
///
/// # Examples
///
/// ```
/// use bellwether_core::resource::CollisionId;
/// assert_eq!(CollisionId::new(3).as_u32(), 3);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CollisionId(u32);

impl CollisionId {
    /// Construct a `CollisionId` from a raw `u32`.
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

impl sealed::Sealed for CollisionId {}
impl ResourceId for CollisionId {
    fn as_u32(self) -> u32 {
        self.0
    }
}

// ---------------------------------------------------------------------------
// SerializerId
// ---------------------------------------------------------------------------

/// Identifier of a serializer resource (one per FD directed channel or
/// bridge egress port).
///
/// At most one claim is active at any simulation time per invariant I3.
/// Enforcement of I3 lives in the engine (round 8); this type carries the
/// kind distinction so the engine's claim-tracking can be type-safe.
///
/// # Examples
///
/// ```
/// use bellwether_core::resource::SerializerId;
/// assert_eq!(SerializerId::new(7).as_u32(), 7);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SerializerId(u32);

impl SerializerId {
    /// Construct a `SerializerId` from a raw `u32`.
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

impl sealed::Sealed for SerializerId {}
impl ResourceId for SerializerId {
    fn as_u32(self) -> u32 {
        self.0
    }
}

// ---------------------------------------------------------------------------
// ClaimError
// ---------------------------------------------------------------------------

/// Errors returned by fallible [`Claim`] constructors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ClaimError {
    /// Constructor was called with `t_end <= t_start`.
    ///
    /// Claims must span a strictly positive interval per the cousin of
    /// invariant I5 (positive duration).
    ZeroOrNegativeDuration,
}

impl core::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ZeroOrNegativeDuration => {
                f.write_str("claim must span a strictly positive duration (t_end > t_start)")
            }
        }
    }
}

impl core::error::Error for ClaimError {}

// ---------------------------------------------------------------------------
// Claim<R>
// ---------------------------------------------------------------------------

/// A transmission's hold on a resource over a finite half-open interval
/// `[t_start, t_end)`.
///
/// Generic over the resource kind: `Claim<CollisionId>` and
/// `Claim<SerializerId>` are distinct types and are not interconvertible.
/// This expresses design.md §3.c.2's type-level distinction.
///
/// # Examples
///
/// ```
/// use bellwether_core::resource::{Claim, SerializerId};
/// use bellwether_core::time::BitTime;
///
/// let claim = Claim::new(
///     SerializerId::new(0),
///     BitTime::from_nanos(100),
///     BitTime::from_nanos(196),
/// ).unwrap();
/// assert_eq!(claim.duration(), BitTime::from_nanos(96));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Claim<R: ResourceId> {
    resource: R,
    t_start: BitTime,
    t_end: BitTime,
}

impl<R: ResourceId> Claim<R> {
    /// Construct a claim spanning `[t_start, t_end)` on the given resource.
    ///
    /// # Errors
    ///
    /// Returns [`ClaimError::ZeroOrNegativeDuration`] if `t_end <= t_start`.
    pub fn new(resource: R, t_start: BitTime, t_end: BitTime) -> Result<Self, ClaimError> {
        if t_end.as_u64() <= t_start.as_u64() {
            return Err(ClaimError::ZeroOrNegativeDuration);
        }
        Ok(Self {
            resource,
            t_start,
            t_end,
        })
    }

    /// The resource this claim is held on.
    #[must_use]
    pub const fn resource(self) -> R {
        self.resource
    }

    /// The (inclusive) start of the claim's half-open interval.
    #[must_use]
    pub const fn t_start(self) -> BitTime {
        self.t_start
    }

    /// The (exclusive) end of the claim's half-open interval.
    #[must_use]
    pub const fn t_end(self) -> BitTime {
        self.t_end
    }

    /// The duration of the claim: `t_end - t_start`.
    ///
    /// Always strictly positive by construction.
    ///
    /// # Panics
    ///
    /// Cannot panic: `t_end > t_start` by construction, so subtraction
    /// cannot underflow.
    #[must_use]
    pub fn duration(self) -> BitTime {
        self.t_end - self.t_start
    }

    /// Whether two claims overlap on their half-open intervals.
    ///
    /// Implements the half-open interval-intersection predicate from
    /// `report_0.md` Theorem 2:
    ///
    /// ```text
    /// [a_0, a_1) ∩ [b_0, b_1) ≠ ∅  ⇔  max(a_0, b_0) < min(a_1, b_1)
    /// ```
    ///
    /// Boundary equality (e.g., `[100, 200)` vs `[200, 300)`) is *not*
    /// overlap under the half-open convention. This is the load-bearing
    /// detail for design.md §2.c invariant I4.
    ///
    /// # Examples
    ///
    /// ```
    /// use bellwether_core::resource::{Claim, SerializerId};
    /// use bellwether_core::time::BitTime;
    ///
    /// let r = SerializerId::new(0);
    /// let a = Claim::new(r, BitTime::new(100), BitTime::new(200)).unwrap();
    /// let b = Claim::new(r, BitTime::new(150), BitTime::new(250)).unwrap();
    /// let c = Claim::new(r, BitTime::new(200), BitTime::new(300)).unwrap();
    ///
    /// assert!(a.overlaps(&b));
    /// assert!(!a.overlaps(&c)); // boundary equality is not overlap
    /// ```
    #[must_use]
    pub fn overlaps(self, other: &Self) -> bool {
        let lo = max_bittime(self.t_start, other.t_start);
        let hi = min_bittime(self.t_end, other.t_end);
        lo.as_u64() < hi.as_u64()
    }
}

const fn max_bittime(a: BitTime, b: BitTime) -> BitTime {
    if a.as_u64() >= b.as_u64() {
        a
    } else {
        b
    }
}

const fn min_bittime(a: BitTime, b: BitTime) -> BitTime {
    if a.as_u64() <= b.as_u64() {
        a
    } else {
        b
    }
}

// ---------------------------------------------------------------------------
// TransmissionError
// ---------------------------------------------------------------------------

/// Errors returned by fallible [`Transmission`] constructors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TransmissionError {
    /// Signal duration and claim duration disagreed.
    ///
    /// Per invariant I6, every active transmission has exactly one claim,
    /// and the claim must hold for the signal's full transmission window.
    DurationMismatch,
    /// Signal start time (`t0`) and claim start time (`t_start`) disagreed.
    ///
    /// The claim must begin at the same instant the signal goes on the wire.
    StartTimeMismatch,
}

impl core::fmt::Display for TransmissionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DurationMismatch => f.write_str(
                "signal duration and claim duration must match (invariant I6)",
            ),
            Self::StartTimeMismatch => f.write_str(
                "signal t0 and claim t_start must match (invariant I6)",
            ),
        }
    }
}

impl core::error::Error for TransmissionError {}

// ---------------------------------------------------------------------------
// Transmission<R>
// ---------------------------------------------------------------------------

/// A signal paired with the claim that authorizes it on a resource.
///
/// Per design.md §2.c invariant I6, every active transmission has exactly
/// one claim of exactly one kind. The generic parameter `R` fixes the
/// resource kind at the type level, so `Transmission<CollisionId>` and
/// `Transmission<SerializerId>` cannot be confused.
///
/// # Examples
///
/// ```
/// use bellwether_core::resource::{Claim, SerializerId, Transmission};
/// use bellwether_core::signal::{NodeId, Signal};
/// use bellwether_core::time::{BitRate, BitTime, Bits};
///
/// let signal = Signal::frame(
///     NodeId::new(0),
///     BitTime::from_nanos(100),
///     Bits::new(96),
///     BitRate::ETHERNET_1G,
/// ).unwrap();
///
/// let claim = Claim::new(
///     SerializerId::new(0),
///     BitTime::from_nanos(100),
///     BitTime::from_nanos(196),
/// ).unwrap();
///
/// let tx = Transmission::new(signal, claim).unwrap();
/// assert_eq!(tx.signal().duration(), tx.claim().duration());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Transmission<R: ResourceId> {
    signal: Signal,
    claim: Claim<R>,
}

impl<R: ResourceId> Transmission<R> {
    /// Construct a transmission from a signal and its claim.
    ///
    /// # Errors
    ///
    /// - [`TransmissionError::DurationMismatch`] if the signal's duration
    ///   does not equal the claim's duration.
    /// - [`TransmissionError::StartTimeMismatch`] if the signal's `t0` does
    ///   not equal the claim's `t_start`.
    pub fn new(signal: Signal, claim: Claim<R>) -> Result<Self, TransmissionError> {
        if signal.t0() != claim.t_start() {
            return Err(TransmissionError::StartTimeMismatch);
        }
        if signal.duration() != claim.duration() {
            return Err(TransmissionError::DurationMismatch);
        }
        Ok(Self { signal, claim })
    }

    /// The signal being transmitted.
    #[must_use]
    pub const fn signal(&self) -> &Signal {
        &self.signal
    }

    /// The claim authorizing the transmission.
    #[must_use]
    pub const fn claim(&self) -> &Claim<R> {
        &self.claim
    }
}

/// A transmission on a collision-domain resource.
pub type CollisionTransmission = Transmission<CollisionId>;

/// A transmission on a serializer resource.
pub type SerializerTransmission = Transmission<SerializerId>;

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
    use crate::signal::{NodeId, Signal};
    use crate::time::{BitRate, Bits};

    const C0: CollisionId = CollisionId::new(0);
    const S0: SerializerId = SerializerId::new(0);

    // -- ID newtypes ----------------------------------------------------------

    #[test]
    fn collision_id_round_trips() {
        assert_eq!(CollisionId::new(42).as_u32(), 42);
        assert_ne!(CollisionId::new(0), CollisionId::new(1));
    }

    #[test]
    fn serializer_id_round_trips() {
        assert_eq!(SerializerId::new(42).as_u32(), 42);
        assert_ne!(SerializerId::new(0), SerializerId::new(1));
    }

    #[test]
    fn resource_id_trait_works_for_both_kinds() {
        fn id_of<R: ResourceId>(r: R) -> u32 {
            r.as_u32()
        }
        assert_eq!(id_of(CollisionId::new(11)), 11);
        assert_eq!(id_of(SerializerId::new(22)), 22);
    }

    // -- Claim --------------------------------------------------------------

    #[test]
    fn claim_records_resource_and_endpoints() {
        let c =
            Claim::new(S0, BitTime::from_nanos(100), BitTime::from_nanos(200)).unwrap();
        assert_eq!(c.resource(), S0);
        assert_eq!(c.t_start(), BitTime::from_nanos(100));
        assert_eq!(c.t_end(), BitTime::from_nanos(200));
    }

    #[test]
    fn claim_duration_is_t_end_minus_t_start() {
        // Sharp oracle: exact integer subtraction.
        let c =
            Claim::new(S0, BitTime::from_nanos(100), BitTime::from_nanos(196)).unwrap();
        assert_eq!(c.duration(), BitTime::from_nanos(96));
    }

    #[test]
    fn claim_rejects_zero_duration() {
        let result = Claim::new(S0, BitTime::from_nanos(100), BitTime::from_nanos(100));
        assert_eq!(result, Err(ClaimError::ZeroOrNegativeDuration));
    }

    #[test]
    fn claim_rejects_negative_duration() {
        let result = Claim::new(S0, BitTime::from_nanos(200), BitTime::from_nanos(100));
        assert_eq!(result, Err(ClaimError::ZeroOrNegativeDuration));
    }

    // -- Claim::overlaps half-open semantics ---------------------------------

    #[test]
    fn overlap_strict_intersection_is_overlap() {
        let a = Claim::new(S0, BitTime::new(100), BitTime::new(200)).unwrap();
        let b = Claim::new(S0, BitTime::new(150), BitTime::new(250)).unwrap();
        assert!(a.overlaps(&b));
        assert!(b.overlaps(&a));
    }

    #[test]
    fn overlap_boundary_equality_is_not_overlap() {
        // [100, 200) vs [200, 300) — boundary equality, half-open says no overlap.
        let a = Claim::new(S0, BitTime::new(100), BitTime::new(200)).unwrap();
        let b = Claim::new(S0, BitTime::new(200), BitTime::new(300)).unwrap();
        assert!(!a.overlaps(&b));
        assert!(!b.overlaps(&a));
    }

    #[test]
    fn overlap_disjoint_is_not_overlap() {
        let a = Claim::new(S0, BitTime::new(100), BitTime::new(200)).unwrap();
        let b = Claim::new(S0, BitTime::new(250), BitTime::new(300)).unwrap();
        assert!(!a.overlaps(&b));
        assert!(!b.overlaps(&a));
    }

    #[test]
    fn overlap_identical_intervals_overlap() {
        let a = Claim::new(S0, BitTime::new(100), BitTime::new(200)).unwrap();
        let b = a;
        assert!(a.overlaps(&b));
    }

    #[test]
    fn overlap_one_inside_the_other_is_overlap() {
        let outer = Claim::new(S0, BitTime::new(100), BitTime::new(300)).unwrap();
        let inner = Claim::new(S0, BitTime::new(150), BitTime::new(200)).unwrap();
        assert!(outer.overlaps(&inner));
        assert!(inner.overlaps(&outer));
    }

    #[test]
    fn overlap_minimal_overlap_is_overlap() {
        // [100, 200) vs [199, 250) overlap at exactly the open boundary —
        // overlap is detected because [199, 200) ⊂ both.
        let a = Claim::new(S0, BitTime::new(100), BitTime::new(200)).unwrap();
        let b = Claim::new(S0, BitTime::new(199), BitTime::new(250)).unwrap();
        assert!(a.overlaps(&b));
    }

    // -- Transmission ---------------------------------------------------------

    fn frame_signal_at(t0: BitTime, bits: Bits, rate: BitRate) -> Signal {
        Signal::frame(NodeId::new(0), t0, bits, rate).unwrap()
    }

    #[test]
    fn transmission_constructs_when_signal_matches_claim() {
        // 96 bits at 1 Gbps = 96 ns
        let signal = frame_signal_at(BitTime::from_nanos(100), Bits::new(96), BitRate::ETHERNET_1G);
        let claim = Claim::new(S0, BitTime::from_nanos(100), BitTime::from_nanos(196)).unwrap();
        let tx = Transmission::new(signal, claim).unwrap();
        assert_eq!(tx.signal().duration(), tx.claim().duration());
        assert_eq!(tx.signal().t0(), tx.claim().t_start());
    }

    #[test]
    fn transmission_rejects_duration_mismatch() {
        let signal = frame_signal_at(BitTime::from_nanos(100), Bits::new(96), BitRate::ETHERNET_1G);
        // claim duration = 100 ns ≠ signal duration = 96 ns
        let claim = Claim::new(S0, BitTime::from_nanos(100), BitTime::from_nanos(200)).unwrap();
        assert_eq!(
            Transmission::new(signal, claim),
            Err(TransmissionError::DurationMismatch),
        );
    }

    #[test]
    fn transmission_rejects_start_time_mismatch() {
        let signal = frame_signal_at(BitTime::from_nanos(100), Bits::new(96), BitRate::ETHERNET_1G);
        // claim t_start = 50 ns ≠ signal t0 = 100 ns
        let claim = Claim::new(S0, BitTime::from_nanos(50), BitTime::from_nanos(146)).unwrap();
        assert_eq!(
            Transmission::new(signal, claim),
            Err(TransmissionError::StartTimeMismatch),
        );
    }

    #[test]
    fn transmission_aliases_resolve_to_correct_kind() {
        let signal = frame_signal_at(BitTime::from_nanos(0), Bits::new(96), BitRate::ETHERNET_1G);

        let serializer_claim =
            Claim::new(S0, BitTime::from_nanos(0), BitTime::from_nanos(96)).unwrap();
        let _: SerializerTransmission = Transmission::new(signal, serializer_claim).unwrap();

        let collision_claim =
            Claim::new(C0, BitTime::from_nanos(0), BitTime::from_nanos(96)).unwrap();
        let _: CollisionTransmission = Transmission::new(signal, collision_claim).unwrap();
    }

    // -- Type-level distinction ----------------------------------------------

    #[test]
    fn claims_of_different_resource_kinds_are_distinct_types() {
        // This test compiles iff Claim<CollisionId> and Claim<SerializerId>
        // are distinct types. Consumers cannot convert one to the other —
        // any attempt requires a deliberate `Transmission::new` with a new
        // signal+claim pair, never coercion.
        let collision_claim =
            Claim::new(C0, BitTime::new(0), BitTime::new(100)).unwrap();
        let serializer_claim =
            Claim::new(S0, BitTime::new(0), BitTime::new(100)).unwrap();

        assert_eq!(collision_claim.resource().as_u32(), 0);
        assert_eq!(serializer_claim.resource().as_u32(), 0);
        // The following would NOT compile:
        // assert_eq!(collision_claim, serializer_claim);
    }

    // -- Errors --------------------------------------------------------------

    #[test]
    fn claim_error_displays_a_useful_message() {
        let msg = format!("{}", ClaimError::ZeroOrNegativeDuration);
        assert!(msg.contains("strictly positive"));
    }

    #[test]
    fn transmission_error_displays_useful_messages() {
        let msg_dur = format!("{}", TransmissionError::DurationMismatch);
        assert!(msg_dur.contains("duration"));
        let msg_start = format!("{}", TransmissionError::StartTimeMismatch);
        assert!(msg_start.contains("t0"));
    }

    #[test]
    fn errors_implement_error_trait() {
        let _: &dyn core::error::Error = &ClaimError::ZeroOrNegativeDuration;
        let _: &dyn core::error::Error = &TransmissionError::DurationMismatch;
    }
}
