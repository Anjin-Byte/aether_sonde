//! Endpoint observables: pure read-side projection of an event [`Log`].
//!
//! The simulator's correctness contract is defined in terms of four
//! endpoint observables that callers can query against the produced
//! event log:
//!
//! - [`carrier_sense`] — is the medium busy at this node at time `t`?
//! - [`collision_detect`] — has this node experienced a collision by time `t`?
//! - [`first_collision_detect_at`] — earliest collision-detect time, if any.
//! - [`receive_complete`] — has the node fully received this signal by `t`?
//!
//! All four are pure functions of `&Log`. Callers can persist a log,
//! replay it, or query observables long after the engine has been dropped.
//! No engine state is required.
//!
//! # Half-open intervals
//!
//! Time intervals are half-open `[t_start, t_end)`. The implementations
//! here respect that convention: a `BackArrive` at time `t` means the
//! signal is *not* present at exactly `t = back_arrival_time`. See
//! [`carrier_sense`] for the exact counting rule.

use crate::event::{Event, Log};
use crate::signal::{NodeId, Signal};
use crate::time::BitTime;

// ===========================================================================
// carrier_sense
// ===========================================================================

/// Whether at least one signal is occupying `node` at time `t`.
///
/// True iff the node-occupancy union at `node` covers `t`. The node's
/// own transmission also counts as carrier, matching IEEE 802.3
/// carrier-sense semantics.
///
/// # Counting rule
///
/// At time `t`, the carrier is present iff the count
///
/// ```text
/// count = #{FrontArrive at node, time ≤ t}
///       + #{TxStart    at node, time ≤ t}
///       − #{BackArrive  at node, time ≤ t}
///       − #{TxEnd       at node, time ≤ t}
/// ```
///
/// is positive. Each `FrontArrive` / `TxStart` admits a signal into the
/// node's occupancy; each `BackArrive` / `TxEnd` ejects it. Because
/// intervals are half-open, an `BackArrive` at `t == query_time` removes
/// the signal at the boundary — the signal is *not* present at `t`.
///
/// # Examples
///
/// ```
/// use aether_sonde::event::Log;
/// use aether_sonde::observe::carrier_sense;
/// use aether_sonde::signal::NodeId;
/// use aether_sonde::time::BitTime;
///
/// // An empty log has no carrier anywhere.
/// let log = Log::new();
/// assert!(!carrier_sense(&log, NodeId::new(0), BitTime::from_nanos(100)));
/// ```
#[must_use]
pub fn carrier_sense(log: &Log, node: NodeId, t: BitTime) -> bool {
    let mut count: i64 = 0;
    for entry in log.iter() {
        if entry.key.time > t {
            break;
        }
        match entry.event {
            Event::FrontArrive { node: n, .. } if n == node => count += 1,
            Event::BackArrive { node: n, .. } if n == node => count -= 1,
            Event::TxStart { node: n, .. } if n == node => count += 1,
            Event::TxEnd { node: n, .. } if n == node => count -= 1,
            _ => {}
        }
    }
    count > 0
}

// ===========================================================================
// collision_detect
// ===========================================================================

/// Whether `node` has experienced a collision by time `t`.
///
/// Cumulative semantics: returns true iff at least one `CollisionDetect`
/// event has fired at `node` at a time `≤ t`. Once a collision is
/// detected, the predicate stays true thereafter.
///
/// A duration-based definition (true throughout the overlap of own-
/// transmit and foreign-occupancy intervals) is also possible. The
/// cumulative form here is the simpler, more useful default for
/// diagnostics; callers wanting the duration form can compose
/// [`carrier_sense`] with their own transmit-state tracking.
///
/// # Examples
///
/// ```
/// use aether_sonde::event::Log;
/// use aether_sonde::observe::collision_detect;
/// use aether_sonde::signal::NodeId;
/// use aether_sonde::time::BitTime;
///
/// let log = Log::new();
/// assert!(!collision_detect(&log, NodeId::new(0), BitTime::from_nanos(100)));
/// ```
#[must_use]
pub fn collision_detect(log: &Log, node: NodeId, t: BitTime) -> bool {
    log.iter().any(|entry| {
        matches!(
            entry.event,
            Event::CollisionDetect { node: n, .. } if n == node
        ) && entry.key.time <= t
    })
}

// ===========================================================================
// first_collision_detect_at
// ===========================================================================

/// The earliest time at which `node` detected a collision, or `None` if
/// no collision was ever detected at this node.
///
/// The log is monotonically non-decreasing in time (per the engine's
/// dispatch order), so the first matching event is the earliest.
///
/// # Examples
///
/// ```
/// use aether_sonde::event::Log;
/// use aether_sonde::observe::first_collision_detect_at;
/// use aether_sonde::signal::NodeId;
///
/// let log = Log::new();
/// assert_eq!(first_collision_detect_at(&log, NodeId::new(0)), None);
/// ```
#[must_use]
pub fn first_collision_detect_at(log: &Log, node: NodeId) -> Option<BitTime> {
    log.iter().find_map(|entry| match entry.event {
        Event::CollisionDetect { node: n, .. } if n == node => Some(entry.key.time),
        _ => None,
    })
}

// ===========================================================================
// receive_complete
// ===========================================================================

/// Whether `node` has fully received `signal` by time `t`.
///
/// True iff a `BackArrive` event for `signal` at `node` has fired at a
/// time `≤ t`. The trailing edge of the signal having reached `node`
/// indicates the entire signal has passed through.
///
/// **Note:** this predicate does *not* validate that the reception was
/// uncorrupted. A signal may have been `receive_complete`'d concurrently
/// with a collision; callers wanting clean-reception semantics should
/// additionally check [`collision_detect`] for the relevant time window.
///
/// # Examples
///
/// ```
/// use aether_sonde::event::Log;
/// use aether_sonde::observe::receive_complete;
/// use aether_sonde::signal::{NodeId, Signal};
/// use aether_sonde::time::{BitRate, BitTime, Bits};
///
/// let log = Log::new();
/// let signal = Signal::frame(
///     NodeId::new(0),
///     BitTime::ZERO,
///     Bits::new(64),
///     BitRate::ETHERNET_1G,
/// ).unwrap();
/// assert!(!receive_complete(&log, NodeId::new(1), signal, BitTime::from_micros(1)));
/// ```
#[must_use]
pub fn receive_complete(log: &Log, node: NodeId, signal: Signal, t: BitTime) -> bool {
    log.iter().any(|entry| match entry.event {
        Event::BackArrive {
            node: n, signal: s, ..
        } => n == node && s == signal && entry.key.time <= t,
        _ => false,
    })
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
    use crate::engine::Engine;
    use crate::signal::SignalKind;
    use crate::time::{BitRate, Bits};
    use crate::topology::{Endpoint, PortId, TopologyBuilder};

    fn ep(node: NodeId, port: u32) -> Endpoint {
        Endpoint::new(node, PortId::new(port))
    }

    // -- Empty-log behavior --------------------------------------------------

    #[test]
    fn empty_log_has_no_carrier_anywhere() {
        let log = Log::new();
        assert!(!carrier_sense(&log, NodeId::new(0), BitTime::ZERO));
        assert!(!carrier_sense(
            &log,
            NodeId::new(0),
            BitTime::from_micros(100)
        ));
    }

    #[test]
    fn empty_log_has_no_collision_anywhere() {
        let log = Log::new();
        assert!(!collision_detect(
            &log,
            NodeId::new(0),
            BitTime::from_micros(100)
        ));
        assert_eq!(first_collision_detect_at(&log, NodeId::new(0)), None);
    }

    #[test]
    fn empty_log_has_no_receive_complete() {
        let log = Log::new();
        let signal = Signal::frame(
            NodeId::new(0),
            BitTime::ZERO,
            Bits::new(64),
            BitRate::ETHERNET_1G,
        )
        .unwrap();
        assert!(!receive_complete(
            &log,
            NodeId::new(1),
            signal,
            BitTime::from_micros(100),
        ));
    }

    // -- HD-1 ordinary success ----------------------------------------------

    fn hd_pair_world(delay_ps: u64) -> (crate::topology::World, NodeId, NodeId) {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        b.add_hd_segment(
            BitRate::ETHERNET_10M,
            BitTime::new(delay_ps),
            ep(s1, 0),
            ep(s2, 0),
        )
        .unwrap();
        (b.build().unwrap(), s1, s2)
    }

    fn run_hd_1() -> (Engine, NodeId, NodeId) {
        // τ = 5 µs, frame = 512 bits at 10 Mbps → duration = 51.2 µs.
        let tau = BitTime::from_micros(5);
        let (world, s1, s2) = hd_pair_world(tau.as_u64());
        let mut engine = Engine::with_seed(world, 1);
        let frame = engine
            .register_frame(s1, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();
        (engine, s1, s2)
    }

    #[test]
    fn hd_1_carrier_sense_at_source() {
        let (engine, s1, _) = run_hd_1();
        let log = engine.log();
        // s1 is transmitting from t=0 to t=51.2 µs.
        assert!(carrier_sense(log, s1, BitTime::ZERO));
        assert!(carrier_sense(log, s1, BitTime::from_nanos(51_199)));
        // At exactly t=51.2 µs, TxEnd fires and decrements own count.
        assert!(!carrier_sense(log, s1, BitTime::from_nanos(51_200)));
    }

    #[test]
    fn hd_1_carrier_sense_at_receiver() {
        let (engine, _, s2) = run_hd_1();
        let log = engine.log();
        // FrontArrive at s2 at t=5 µs; BackArrive at t=56.2 µs.
        assert!(!carrier_sense(log, s2, BitTime::from_nanos(4_999)));
        assert!(carrier_sense(log, s2, BitTime::from_micros(5)));
        assert!(carrier_sense(log, s2, BitTime::from_nanos(56_199)));
        assert!(!carrier_sense(log, s2, BitTime::from_nanos(56_200)));
    }

    #[test]
    fn hd_1_no_collision_anywhere() {
        let (engine, s1, s2) = run_hd_1();
        let log = engine.log();
        assert!(!collision_detect(log, s1, BitTime::from_millis(1)));
        assert!(!collision_detect(log, s2, BitTime::from_millis(1)));
        assert_eq!(first_collision_detect_at(log, s1), None);
        assert_eq!(first_collision_detect_at(log, s2), None);
    }

    #[test]
    fn hd_1_receive_complete_at_predicted_time() {
        let (engine, s1, s2) = run_hd_1();
        let log = engine.log();
        // Reconstruct the signal s1 transmitted: 512 bits at 10 Mbps from t=0.
        let signal =
            Signal::frame(s1, BitTime::ZERO, Bits::new(512), BitRate::ETHERNET_10M).unwrap();
        // BackArrive at s2 fires at t = τ + duration = 5 + 51.2 = 56.2 µs.
        assert!(!receive_complete(
            log,
            s2,
            signal,
            BitTime::from_nanos(56_199)
        ));
        assert!(receive_complete(
            log,
            s2,
            signal,
            BitTime::from_nanos(56_200)
        ));
        assert!(receive_complete(log, s2, signal, BitTime::from_millis(1)));
    }

    // -- Theorem 1: HD collision detection ----------------------------------

    fn run_theorem_1() -> (Engine, NodeId, NodeId) {
        // τ = 5 µs; A starts at 0, B starts at 4.9 µs.
        let tau = BitTime::from_micros(5);
        let (world, a, b) = hd_pair_world(tau.as_u64());
        let mut engine = Engine::with_seed(world, 1);
        let fa = engine
            .register_frame(a, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        let fb = engine
            .register_frame(b, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, a, fa);
        engine.schedule_tx_attempt(BitTime::from_nanos(4_900), b, fb);
        engine.run_until(BitTime::from_micros(20));
        (engine, a, b)
    }

    #[test]
    fn theorem_1_collision_detect_cumulative() {
        let (engine, a, b) = run_theorem_1();
        let log = engine.log();

        // B detects collision at exactly t = 5 µs.
        assert!(!collision_detect(log, b, BitTime::from_nanos(4_999)));
        assert!(collision_detect(log, b, BitTime::from_micros(5)));
        // Cumulative: stays true thereafter.
        assert!(collision_detect(log, b, BitTime::from_micros(20)));

        // A detects collision at exactly t = 9.9 µs.
        assert!(!collision_detect(log, a, BitTime::from_nanos(9_899)));
        assert!(collision_detect(log, a, BitTime::from_nanos(9_900)));
    }

    #[test]
    fn theorem_1_first_collision_detect_at_matches_closed_form() {
        let (engine, a, b) = run_theorem_1();
        let log = engine.log();
        assert_eq!(
            first_collision_detect_at(log, b),
            Some(BitTime::from_micros(5))
        );
        assert_eq!(
            first_collision_detect_at(log, a),
            Some(BitTime::from_nanos(9_900)),
        );
    }

    // -- FD-1: Theorem 3 (no collisions on FD) ------------------------------

    #[test]
    fn fd_1_observables_show_no_collision_and_clean_receive() {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        b.add_fd_segment(
            BitRate::ETHERNET_1G,
            BitTime::from_nanos(100),
            ep(s1, 0),
            ep(s2, 0),
        )
        .unwrap();
        let world = b.build().unwrap();
        let mut engine = Engine::with_seed(world, 1);
        let frame = engine
            .register_frame(s1, Bits::new(96), SignalKind::Frame, BitRate::ETHERNET_1G)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();

        let log = engine.log();
        // No collisions on FD per Theorem 3.
        assert_eq!(first_collision_detect_at(log, s1), None);
        assert_eq!(first_collision_detect_at(log, s2), None);
        assert!(!collision_detect(log, s1, BitTime::from_micros(1)));
        assert!(!collision_detect(log, s2, BitTime::from_micros(1)));

        // Reconstruct the signal: 96 bits at 1 Gbps starting at t=0.
        let signal = Signal::frame(s1, BitTime::ZERO, Bits::new(96), BitRate::ETHERNET_1G).unwrap();
        // BackArrive at s2 fires at t = 100 + 96 = 196 ns.
        assert!(!receive_complete(log, s2, signal, BitTime::from_nanos(195)));
        assert!(receive_complete(log, s2, signal, BitTime::from_nanos(196)));
    }

    // -- Bridge relay: Theorem 5 (no collisions across bridge) --------------

    #[test]
    fn bridge_relay_observables_show_no_collisions() {
        // s1 — HD A — bridge:0 ; bridge:1 — HD B — s2
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        let br = b.add_bridge(2, Bits::new(64), BitTime::from_nanos(100));
        b.add_hd_segment(
            BitRate::ETHERNET_1G,
            BitTime::from_nanos(50),
            ep(s1, 0),
            ep(br, 0),
        )
        .unwrap();
        b.add_hd_segment(
            BitRate::ETHERNET_1G,
            BitTime::from_nanos(50),
            ep(s2, 0),
            ep(br, 1),
        )
        .unwrap();
        let world = b.build().unwrap();
        let mut engine = Engine::with_seed(world, 1);
        let frame = engine
            .register_frame(s1, Bits::new(96), SignalKind::Frame, BitRate::ETHERNET_1G)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();

        let log = engine.log();
        // Theorem 5: no collisions anywhere in the relay path.
        assert!(!collision_detect(log, s1, BitTime::from_micros(10)));
        assert!(!collision_detect(log, s2, BitTime::from_micros(10)));
        assert!(!collision_detect(log, br, BitTime::from_micros(10)));
        assert_eq!(first_collision_detect_at(log, br), None);
    }

    // -- Counting-rule property tests ---------------------------------------

    #[test]
    fn carrier_sense_count_returns_to_zero_after_full_run() {
        // For HD-1, after the run completes, no node should still have
        // carrier present (everything is back to zero net count).
        let (engine, s1, s2) = run_hd_1();
        let log = engine.log();
        let after = BitTime::from_millis(1);
        assert!(!carrier_sense(log, s1, after));
        assert!(!carrier_sense(log, s2, after));
    }

    #[test]
    fn collision_detect_query_at_zero_returns_false_for_empty_node() {
        let (engine, a, _) = run_hd_1();
        // a never collided in the ordinary HD-1 case.
        assert!(!collision_detect(engine.log(), a, BitTime::ZERO));
    }
}
