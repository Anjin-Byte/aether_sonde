//! Event types, phase ordering, event keys, and the append-only event log.
//!
//! Per design.md §3.c.5–9, this module supplies:
//!
//! * [`Event`] — sealed enum of 12 variants, one per event class in
//!   `report_1.md` §"Event-driven semantics" / `report_0.md` Definition 8.
//!   Publicly exhaustive: adding a variant is a deliberate breaking change.
//! * [`Phase`] — four-phase priority ordering per `report_0.md`
//!   Proposition 17. Variant declaration order encodes priority.
//! * [`EventKey`] — `(time, phase, serial_id)` triple ordered
//!   lexicographically; the priority-queue key.
//! * [`Log`] — append-only event history. The only mutation method is
//!   `pub(crate)`; external consumers receive `&Log` and read but do not
//!   mutate (invariant I7).
//! * [`FrameId`] — typed handle into the engine's frame table (round 8).
//!
//! The engine (round 8) is the only writer of [`Log`] and the only
//! generator of `serial_id`s. This module supplies the type vocabulary;
//! it does not schedule or dispatch events.

use crate::resource::SerializerId;
use crate::signal::{NodeId, Signal};
use crate::time::BitTime;
use crate::topology::PortId;

// ===========================================================================
// FrameId
// ===========================================================================

/// Identifier of a frame in the engine's frame table.
///
/// A typed handle the engine (round 8) uses to map MAC-level frame entities
/// (destination, source, length, payload metadata) to their handles in
/// events. Events that pre-date a frame's wire signal — [`Event::TxAttempt`]
/// and [`Event::FrameEligible`] — reference the frame by its `FrameId`.
///
/// # Examples
///
/// ```
/// use bellwether_core::event::FrameId;
/// assert_eq!(FrameId::new(7).as_u32(), 7);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameId(u32);

impl FrameId {
    /// Construct a `FrameId` from a raw `u32`.
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

// ===========================================================================
// Phase
// ===========================================================================

/// The phase of an event within a single timestamp.
///
/// Per `report_0.md` Proposition 17 ("correct event phases"), events at
/// the same `time` must process in a fixed order to give exact
/// simultaneous-event semantics. Variant declaration order encodes that
/// priority: `Release < Assertion < Reaction < LocalDecision`.
///
/// # Phase assignment
///
/// | Phase           | Events                                                            |
/// |-----------------|-------------------------------------------------------------------|
/// | `Release`       | `BackArrive`, `TxEnd`, `JamEnd`                                   |
/// | `Assertion`     | `FrontArrive`                                                     |
/// | `Reaction`      | `CollisionDetect`, `JamStart`, `FrameEligible`                    |
/// | `LocalDecision` | `TxAttempt`, `TxStart`, `Enqueue`, `Dequeue`, `BackoffExpire`     |
///
/// See [`Event::phase`] for the per-variant mapping.
///
/// # Examples
///
/// ```
/// use bellwether_core::event::Phase;
/// assert!(Phase::Release < Phase::Assertion);
/// assert!(Phase::Assertion < Phase::Reaction);
/// assert!(Phase::Reaction < Phase::LocalDecision);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    /// Things ending at this timestamp: `BackArrive`, `TxEnd`, `JamEnd`.
    Release,
    /// Things asserting at this timestamp: `FrontArrive`.
    Assertion,
    /// Reactions to phase-2 assertions: `CollisionDetect`, `JamStart`,
    /// `FrameEligible`.
    Reaction,
    /// Timer-driven and bridge-internal state mutations: `TxAttempt`,
    /// `TxStart`, `Enqueue`, `Dequeue`, `BackoffExpire`.
    LocalDecision,
}

// ===========================================================================
// Event
// ===========================================================================

/// Discrete event in the simulation.
///
/// The 12 variants below correspond one-to-one with `report_1.md`
/// §"Event-driven semantics" / `report_0.md` Definition 8. Per design.md
/// §3.c.5, this enum is **publicly exhaustive** (no `#[non_exhaustive]`):
/// adding a 13th variant is a deliberate breaking change visible at every
/// consumer's `match` arm.
///
/// # Examples
///
/// ```
/// use bellwether_core::event::{Event, FrameId, Phase};
/// use bellwether_core::signal::NodeId;
///
/// let attempt = Event::TxAttempt {
///     node: NodeId::new(0),
///     frame: FrameId::new(0),
/// };
/// assert_eq!(attempt.phase(), Phase::LocalDecision);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    /// MAC requests permission to transmit a frame.
    TxAttempt {
        /// The node attempting the transmission.
        node: NodeId,
        /// The frame to be transmitted.
        frame: FrameId,
    },
    /// A node begins putting a signal on the wire.
    TxStart {
        /// The transmitting node.
        node: NodeId,
        /// The signal being transmitted.
        signal: Signal,
    },
    /// A node finishes putting a signal on the wire.
    TxEnd {
        /// The transmitting node.
        node: NodeId,
        /// The signal whose tail just left the wire.
        signal: Signal,
    },
    /// The leading edge of a signal arrives at a node's port.
    FrontArrive {
        /// The receiving node.
        node: NodeId,
        /// The port at which the front arrives.
        port: PortId,
        /// The signal whose front arrives.
        signal: Signal,
    },
    /// The trailing edge of a signal arrives at a node's port.
    BackArrive {
        /// The receiving node.
        node: NodeId,
        /// The port at which the back arrives.
        port: PortId,
        /// The signal whose back arrives.
        signal: Signal,
    },
    /// A transmitting node detects a collision (foreign energy on the
    /// medium during its own transmission).
    CollisionDetect {
        /// The transmitting node that detected the collision.
        node: NodeId,
        /// The transmission whose own time window saw the foreign signal.
        signal: Signal,
    },
    /// A node begins emitting a collision-enforcement jam.
    JamStart {
        /// The node beginning to jam.
        node: NodeId,
    },
    /// A node finishes emitting its jam.
    JamEnd {
        /// The node finishing its jam.
        node: NodeId,
    },
    /// A bridge has decoded enough of an ingress frame for it to be
    /// eligible for egress queueing.
    FrameEligible {
        /// The bridge node.
        bridge: NodeId,
        /// The egress port the frame is being scheduled for.
        port: PortId,
        /// The frame becoming eligible.
        frame: FrameId,
    },
    /// A frame is enqueued on a serializer.
    Enqueue {
        /// The serializer being mutated.
        serializer: SerializerId,
        /// The frame being enqueued.
        frame: FrameId,
    },
    /// A frame is dequeued from a serializer (about to be transmitted).
    Dequeue {
        /// The serializer being mutated.
        serializer: SerializerId,
        /// The frame being dequeued.
        frame: FrameId,
    },
    /// A node's backoff timer for a particular retry attempt has expired
    /// and retransmission may be attempted.
    BackoffExpire {
        /// The node whose backoff just expired.
        node: NodeId,
        /// The retry attempt number (0-indexed: first retry is 0).
        attempt: u32,
    },
}

impl Event {
    /// The [`Phase`] this event belongs to.
    ///
    /// Used by the engine's priority queue to order events at the same
    /// timestamp per `report_0.md` Proposition 17.
    ///
    /// # Examples
    ///
    /// ```
    /// use bellwether_core::event::{Event, FrameId, Phase};
    /// use bellwether_core::signal::NodeId;
    ///
    /// let jam_end = Event::JamEnd { node: NodeId::new(0) };
    /// assert_eq!(jam_end.phase(), Phase::Release);
    /// ```
    #[must_use]
    pub const fn phase(&self) -> Phase {
        match self {
            Event::BackArrive { .. } | Event::TxEnd { .. } | Event::JamEnd { .. } => {
                Phase::Release
            }
            Event::FrontArrive { .. } => Phase::Assertion,
            Event::CollisionDetect { .. }
            | Event::JamStart { .. }
            | Event::FrameEligible { .. } => Phase::Reaction,
            Event::TxAttempt { .. }
            | Event::TxStart { .. }
            | Event::Enqueue { .. }
            | Event::Dequeue { .. }
            | Event::BackoffExpire { .. } => Phase::LocalDecision,
        }
    }
}

// ===========================================================================
// EventKey
// ===========================================================================

/// Priority-queue key for an event: `(time, phase, serial_id)`.
///
/// Derived ordering follows field declaration order
/// (`time` → `phase` → `serial_id`), giving the lexicographic order
/// required by `report_0.md` Proposition 17.
///
/// `serial_id` is a monotonically increasing counter the engine maintains;
/// it tie-breaks within the same `(time, phase)` deterministically.
///
/// # Examples
///
/// ```
/// use bellwether_core::event::{EventKey, Phase};
/// use bellwether_core::time::BitTime;
///
/// let early = EventKey { time: BitTime::new(100), phase: Phase::Release, serial_id: 0 };
/// let late = EventKey { time: BitTime::new(200), phase: Phase::Release, serial_id: 0 };
/// assert!(early < late);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventKey {
    /// The event's timestamp.
    pub time: BitTime,
    /// The phase within the timestamp.
    pub phase: Phase,
    /// Monotonically increasing tie-breaker assigned by the engine.
    pub serial_id: u64,
}

// ===========================================================================
// LoggedEvent and Log
// ===========================================================================

/// A single entry in the [`Log`]: an event paired with its scheduling key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LoggedEvent {
    /// The key under which the event was scheduled.
    pub key: EventKey,
    /// The event itself.
    pub event: Event,
}

/// Append-only event history.
///
/// Per design.md §2.c invariant I7, the log is append-only during a run.
/// External consumers receive `&Log` and may iterate, count, and inspect
/// entries — but the only mutation method (`push`) is `pub(crate)`, so
/// only the engine (in this same crate) can write.
///
/// # Examples
///
/// ```
/// use bellwether_core::event::Log;
/// let log = Log::new();
/// assert!(log.is_empty());
/// assert_eq!(log.len(), 0);
/// ```
#[derive(Debug, Clone, Default)]
pub struct Log {
    entries: Vec<LoggedEvent>,
}

impl Log {
    /// Construct an empty log.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// The number of entries in the log.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the log is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over the log's entries in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &LoggedEvent> + '_ {
        self.entries.iter()
    }

    /// All log entries as a slice in insertion order.
    #[must_use]
    pub fn entries(&self) -> &[LoggedEvent] {
        &self.entries
    }

    /// The most recent entry, or `None` if empty.
    #[must_use]
    pub fn last(&self) -> Option<&LoggedEvent> {
        self.entries.last()
    }

    /// Append an event to the log.
    ///
    /// Crate-internal: only the engine may call this. External consumers
    /// receive `&Log` references and cannot mutate.
    pub(crate) fn push(&mut self, key: EventKey, event: Event) {
        self.entries.push(LoggedEvent { key, event });
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Per [Result vs Panic]: unwrap and panic are allowed in tests."
)]
mod tests {
    use super::*;
    use crate::resource::SerializerId;
    use crate::signal::{NodeId, Signal};
    use crate::time::{BitRate, Bits};

    // -- FrameId -------------------------------------------------------------

    #[test]
    fn frame_id_round_trips() {
        assert_eq!(FrameId::new(0).as_u32(), 0);
        assert_eq!(FrameId::new(42).as_u32(), 42);
        assert_eq!(FrameId::new(u32::MAX).as_u32(), u32::MAX);
        assert_ne!(FrameId::new(0), FrameId::new(1));
    }

    // -- Phase: declaration-order ordering ------------------------------------

    #[test]
    fn phase_declaration_order_encodes_priority() {
        assert!(Phase::Release < Phase::Assertion);
        assert!(Phase::Assertion < Phase::Reaction);
        assert!(Phase::Reaction < Phase::LocalDecision);
        // Total order chain holds transitively.
        assert!(Phase::Release < Phase::LocalDecision);
    }

    // -- Event::phase mapping (12 variants) -----------------------------------

    fn signal() -> Signal {
        Signal::frame(
            NodeId::new(0),
            BitTime::ZERO,
            Bits::new(64),
            BitRate::ETHERNET_1G,
        )
        .unwrap()
    }

    #[test]
    fn event_phase_release_for_things_ending_at_t() {
        let n = NodeId::new(0);
        let p = PortId::new(0);
        let s = signal();
        assert_eq!(Event::BackArrive { node: n, port: p, signal: s }.phase(), Phase::Release);
        assert_eq!(Event::TxEnd { node: n, signal: s }.phase(), Phase::Release);
        assert_eq!(Event::JamEnd { node: n }.phase(), Phase::Release);
    }

    #[test]
    fn event_phase_assertion_for_front_arrive() {
        let n = NodeId::new(0);
        let p = PortId::new(0);
        let s = signal();
        assert_eq!(
            Event::FrontArrive { node: n, port: p, signal: s }.phase(),
            Phase::Assertion,
        );
    }

    #[test]
    fn event_phase_reaction_for_collision_jam_eligibility() {
        let n = NodeId::new(0);
        let s = signal();
        assert_eq!(
            Event::CollisionDetect { node: n, signal: s }.phase(),
            Phase::Reaction,
        );
        assert_eq!(Event::JamStart { node: n }.phase(), Phase::Reaction);
        assert_eq!(
            Event::FrameEligible { bridge: n, port: PortId::new(0), frame: FrameId::new(0) }
                .phase(),
            Phase::Reaction,
        );
    }

    #[test]
    fn event_phase_local_decision_for_timer_and_state_mutations() {
        let n = NodeId::new(0);
        let s = signal();
        let f = FrameId::new(0);
        let r = SerializerId::new(0);
        assert_eq!(
            Event::TxAttempt { node: n, frame: f }.phase(),
            Phase::LocalDecision,
        );
        assert_eq!(Event::TxStart { node: n, signal: s }.phase(), Phase::LocalDecision);
        assert_eq!(Event::Enqueue { serializer: r, frame: f }.phase(), Phase::LocalDecision);
        assert_eq!(Event::Dequeue { serializer: r, frame: f }.phase(), Phase::LocalDecision);
        assert_eq!(
            Event::BackoffExpire { node: n, attempt: 0 }.phase(),
            Phase::LocalDecision,
        );
    }

    // -- Sealed-enum discipline ----------------------------------------------

    #[test]
    fn event_is_exhaustively_matchable_without_wildcard() {
        // Adding a 13th Event variant without updating this match arm
        // would produce a compile error. This is the sealed-enum
        // discipline from design.md §3.c.5.
        let n = NodeId::new(0);
        let p = PortId::new(0);
        let s = signal();
        let f = FrameId::new(0);
        let ser = SerializerId::new(0);

        let events = [
            Event::TxAttempt { node: n, frame: f },
            Event::TxStart { node: n, signal: s },
            Event::TxEnd { node: n, signal: s },
            Event::FrontArrive { node: n, port: p, signal: s },
            Event::BackArrive { node: n, port: p, signal: s },
            Event::CollisionDetect { node: n, signal: s },
            Event::JamStart { node: n },
            Event::JamEnd { node: n },
            Event::FrameEligible { bridge: n, port: p, frame: f },
            Event::Enqueue { serializer: ser, frame: f },
            Event::Dequeue { serializer: ser, frame: f },
            Event::BackoffExpire { node: n, attempt: 0 },
        ];

        for ev in events {
            let label: &'static str = match ev {
                Event::TxAttempt { .. } => "TxAttempt",
                Event::TxStart { .. } => "TxStart",
                Event::TxEnd { .. } => "TxEnd",
                Event::FrontArrive { .. } => "FrontArrive",
                Event::BackArrive { .. } => "BackArrive",
                Event::CollisionDetect { .. } => "CollisionDetect",
                Event::JamStart { .. } => "JamStart",
                Event::JamEnd { .. } => "JamEnd",
                Event::FrameEligible { .. } => "FrameEligible",
                Event::Enqueue { .. } => "Enqueue",
                Event::Dequeue { .. } => "Dequeue",
                Event::BackoffExpire { .. } => "BackoffExpire",
            };
            assert!(!label.is_empty());
        }
    }

    // -- EventKey ordering (I4 phase-order totality) -------------------------

    fn key(time: u64, phase: Phase, serial_id: u64) -> EventKey {
        EventKey {
            time: BitTime::new(time),
            phase,
            serial_id,
        }
    }

    #[test]
    fn event_key_phase_only_ordering_at_same_time_and_serial() {
        let release = key(100, Phase::Release, 0);
        let assertion = key(100, Phase::Assertion, 0);
        let reaction = key(100, Phase::Reaction, 0);
        let local = key(100, Phase::LocalDecision, 0);
        assert!(release < assertion);
        assert!(assertion < reaction);
        assert!(reaction < local);
    }

    #[test]
    fn event_key_time_dominates_phase() {
        // Earlier time wins regardless of phase.
        assert!(
            key(100, Phase::LocalDecision, 999) < key(200, Phase::Release, 0),
        );
    }

    #[test]
    fn event_key_phase_dominates_serial_id() {
        // Same time, different phase: phase decides regardless of serial.
        assert!(
            key(100, Phase::Release, 999) < key(100, Phase::Assertion, 0),
        );
    }

    #[test]
    fn event_key_serial_breaks_ties_within_same_phase_and_time() {
        assert!(key(100, Phase::Release, 1) < key(100, Phase::Release, 2));
    }

    #[test]
    fn event_key_total_ordering_closure_via_sort() {
        // Sharp oracle: a manually constructed sequence of 16 keys
        // mixing 3 timestamps × 4 phases × distinct serials sorts to
        // the predicted order.
        let mut keys = vec![
            // Timestamp 200 (latest):
            key(200, Phase::LocalDecision, 1),
            key(200, Phase::Release, 0),
            key(200, Phase::Reaction, 5),
            key(200, Phase::Assertion, 7),
            // Timestamp 100 (middle):
            key(100, Phase::Reaction, 0),
            key(100, Phase::Release, 9),
            key(100, Phase::LocalDecision, 0),
            key(100, Phase::Assertion, 3),
            // Timestamp 50 (earliest):
            key(50, Phase::Reaction, 0),
            key(50, Phase::Release, 1),
            key(50, Phase::LocalDecision, 0),
            key(50, Phase::Assertion, 0),
            // Tie-breakers within the same (time, phase):
            key(50, Phase::Release, 0), // earlier than the other 50/Release
            key(100, Phase::Release, 0), // earlier than 100/Release/9
            key(200, Phase::Release, 5), // later than 200/Release/0
            key(200, Phase::Release, 99), // latest of the 200/Release tier
        ];
        keys.sort();
        let expected = vec![
            // 50:
            key(50, Phase::Release, 0),
            key(50, Phase::Release, 1),
            key(50, Phase::Assertion, 0),
            key(50, Phase::Reaction, 0),
            key(50, Phase::LocalDecision, 0),
            // 100:
            key(100, Phase::Release, 0),
            key(100, Phase::Release, 9),
            key(100, Phase::Assertion, 3),
            key(100, Phase::Reaction, 0),
            key(100, Phase::LocalDecision, 0),
            // 200:
            key(200, Phase::Release, 0),
            key(200, Phase::Release, 5),
            key(200, Phase::Release, 99),
            key(200, Phase::Assertion, 7),
            key(200, Phase::Reaction, 5),
            key(200, Phase::LocalDecision, 1),
        ];
        assert_eq!(keys, expected);
    }

    // -- Log: append-only behavior -------------------------------------------

    fn sample_event() -> Event {
        Event::JamStart {
            node: NodeId::new(0),
        }
    }

    #[test]
    fn log_starts_empty() {
        let log = Log::new();
        assert!(log.is_empty());
        assert_eq!(log.len(), 0);
        assert!(log.last().is_none());
        assert_eq!(log.iter().count(), 0);
    }

    #[test]
    fn log_default_matches_new() {
        let a: Log = Log::default();
        let b: Log = Log::new();
        assert_eq!(a.len(), b.len());
        assert_eq!(a.is_empty(), b.is_empty());
    }

    #[test]
    fn log_push_appends_in_order() {
        let mut log = Log::new();
        let k1 = key(100, Phase::Reaction, 0);
        let k2 = key(200, Phase::LocalDecision, 1);
        log.push(k1, sample_event());
        log.push(k2, sample_event());
        assert_eq!(log.len(), 2);
        let collected: Vec<&LoggedEvent> = log.iter().collect();
        assert_eq!(collected[0].key, k1);
        assert_eq!(collected[1].key, k2);
        assert_eq!(log.last().unwrap().key, k2);
    }

    #[test]
    fn log_entries_slice_matches_iter() {
        let mut log = Log::new();
        log.push(key(0, Phase::Release, 0), sample_event());
        log.push(key(0, Phase::Assertion, 1), sample_event());
        log.push(key(0, Phase::Reaction, 2), sample_event());
        let from_iter: Vec<&LoggedEvent> = log.iter().collect();
        let from_slice: &[LoggedEvent] = log.entries();
        assert_eq!(from_iter.len(), from_slice.len());
        for (a, b) in from_iter.iter().zip(from_slice.iter()) {
            assert_eq!(*a, b);
        }
    }
}
