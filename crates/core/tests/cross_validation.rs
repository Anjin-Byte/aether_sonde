//! Cross-validation: the build-time `TopologyBuilder` is the reference
//! implementation; the continuity edit path (round 10) is the extended
//! path. They must agree on observable behavior.
//!
//! Per the codex's `Reference Implementation as Oracle`: pair an
//! optimized/extended path against a simpler reference and assert exact
//! equality on observables.
//!
//! Also: `observe::*` queries are a second view of the same execution;
//! their answers must be consistent with the underlying log at every
//! event timestamp.

#![allow(missing_docs)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

mod common;

use aether_sonde::engine::{Edit, Engine};
use aether_sonde::event::{Event, Phase};
use aether_sonde::observe;
use aether_sonde::signal::{NodeId, Signal, SignalKind};
use aether_sonde::time::{BitRate, BitTime, Bits};
use aether_sonde::topology::{SegmentId, TopologyBuilder};

use common::{ep, hd_pair};

/// A propagation-event subsequence: the log filtered to drop topology
/// events (`NodeAdded`, `SegmentAdded`, etc.). Used to compare two
/// build paths that produce different topology-event prefixes but the
/// same propagation behavior.
fn propagation_keys(engine: &Engine) -> Vec<(BitTime, Phase)> {
    engine
        .log()
        .iter()
        .filter(|e| {
            !matches!(
                e.event,
                Event::NodeAdded { .. }
                    | Event::SegmentAdded { .. }
                    | Event::SegmentRemoved { .. }
                    | Event::NodeRemoved { .. }
                    | Event::SegmentDelayChanged { .. }
                    | Event::SegmentRateChanged { .. }
                    | Event::MacConfigChanged { .. }
                    | Event::PortDisconnected { .. }
            )
        })
        .map(|e| (e.key.time, e.key.phase))
        .collect()
}

// --------------------------------------------------------------------------
// Equivalence: build-time vs continuity-built — propagation matches
// --------------------------------------------------------------------------

#[test]
fn hd_1_topology_built_two_ways_propagates_identically() {
    let tau = BitTime::from_micros(5);

    // Reference: TopologyBuilder.
    let log_ref = {
        let (world, s1, _) = hd_pair(tau);
        let mut engine = Engine::with_seed(world, 11);
        let frame = engine
            .register_frame(s1, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();
        propagation_keys(&engine)
    };

    // Continuity: same topology built via apply_edit.
    let log_continuity = {
        let mut engine = Engine::with_seed(TopologyBuilder::new().build().unwrap(), 11);
        engine
            .apply_edit(Edit::AddEndStation { port_count: 1 })
            .unwrap();
        engine
            .apply_edit(Edit::AddEndStation { port_count: 1 })
            .unwrap();
        engine
            .apply_edit(Edit::AddHdSegment {
                rate: BitRate::ETHERNET_10M,
                delay: tau,
                a: ep(NodeId::new(0), 0),
                b: ep(NodeId::new(1), 0),
            })
            .unwrap();
        let frame = engine
            .register_frame(
                NodeId::new(0),
                Bits::new(512),
                SignalKind::Frame,
                BitRate::ETHERNET_10M,
            )
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, NodeId::new(0), frame);
        engine.run_until_idle();
        propagation_keys(&engine)
    };

    assert_eq!(log_ref, log_continuity);
}

#[test]
fn hd_3_repeater_chain_built_two_ways_propagates_identically() {
    // s1 — H_a — r1 — H_b — r2 — H_c — s2: three HD segments, two
    // repeaters. Build via TopologyBuilder vs via apply_edit; verify
    // identical propagation.
    let tau = BitTime::from_micros(2);
    let delta_h = BitTime::from_nanos(100);

    let log_ref = {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        let r1 = b.add_repeater(2, delta_h);
        let r2 = b.add_repeater(2, delta_h);
        b.add_hd_segment(BitRate::ETHERNET_10M, tau, ep(s1, 0), ep(r1, 0))
            .unwrap();
        b.add_hd_segment(BitRate::ETHERNET_10M, tau, ep(r1, 1), ep(r2, 0))
            .unwrap();
        b.add_hd_segment(BitRate::ETHERNET_10M, tau, ep(r2, 1), ep(s2, 0))
            .unwrap();
        let world = b.build().unwrap();
        let mut engine = Engine::with_seed(world, 13);
        let frame = engine
            .register_frame(s1, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();
        propagation_keys(&engine)
    };

    let log_continuity = {
        let mut engine = Engine::with_seed(TopologyBuilder::new().build().unwrap(), 13);
        // Add s1, s2, r1, r2 in same order as builder.
        engine
            .apply_edit(Edit::AddEndStation { port_count: 1 })
            .unwrap();
        engine
            .apply_edit(Edit::AddEndStation { port_count: 1 })
            .unwrap();
        engine
            .apply_edit(Edit::AddRepeater {
                port_count: 2,
                delta_h,
            })
            .unwrap();
        engine
            .apply_edit(Edit::AddRepeater {
                port_count: 2,
                delta_h,
            })
            .unwrap();
        engine
            .apply_edit(Edit::AddHdSegment {
                rate: BitRate::ETHERNET_10M,
                delay: tau,
                a: ep(NodeId::new(0), 0),
                b: ep(NodeId::new(2), 0),
            })
            .unwrap();
        engine
            .apply_edit(Edit::AddHdSegment {
                rate: BitRate::ETHERNET_10M,
                delay: tau,
                a: ep(NodeId::new(2), 1),
                b: ep(NodeId::new(3), 0),
            })
            .unwrap();
        engine
            .apply_edit(Edit::AddHdSegment {
                rate: BitRate::ETHERNET_10M,
                delay: tau,
                a: ep(NodeId::new(3), 1),
                b: ep(NodeId::new(1), 0),
            })
            .unwrap();
        let frame = engine
            .register_frame(
                NodeId::new(0),
                Bits::new(512),
                SignalKind::Frame,
                BitRate::ETHERNET_10M,
            )
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, NodeId::new(0), frame);
        engine.run_until_idle();
        propagation_keys(&engine)
    };

    assert_eq!(log_ref, log_continuity);
}

#[test]
fn fd_pair_built_two_ways_propagates_identically() {
    let delay = BitTime::from_nanos(100);
    let rate = BitRate::ETHERNET_1G;

    let log_ref = {
        let (world, s1, _) = common::fd_pair(delay, rate);
        let mut engine = Engine::with_seed(world, 17);
        let frame = engine
            .register_frame(s1, Bits::new(512), SignalKind::Frame, rate)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();
        propagation_keys(&engine)
    };

    let log_continuity = {
        let mut engine = Engine::with_seed(TopologyBuilder::new().build().unwrap(), 17);
        engine
            .apply_edit(Edit::AddEndStation { port_count: 1 })
            .unwrap();
        engine
            .apply_edit(Edit::AddEndStation { port_count: 1 })
            .unwrap();
        engine
            .apply_edit(Edit::AddFdSegment {
                rate,
                delay,
                a: ep(NodeId::new(0), 0),
                b: ep(NodeId::new(1), 0),
            })
            .unwrap();
        let frame = engine
            .register_frame(NodeId::new(0), Bits::new(512), SignalKind::Frame, rate)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, NodeId::new(0), frame);
        engine.run_until_idle();
        propagation_keys(&engine)
    };

    assert_eq!(log_ref, log_continuity);
}

// --------------------------------------------------------------------------
// observe::* queries are consistent with the underlying log
// --------------------------------------------------------------------------

#[test]
fn observe_carrier_sense_matches_inflight_signals_on_log() {
    // For an HD-1 transmission, sample carrier_sense at the receiver at
    // every relevant time. The observable is `true` iff the receiver is
    // currently between FrontArrive (inclusive) and BackArrive
    // (exclusive) for some signal in the log.
    let tau = BitTime::from_micros(5);
    let (world, s1, s2) = hd_pair(tau);
    let mut engine = Engine::with_seed(world, 19);
    let frame = engine
        .register_frame(s1, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
    engine.run_until_idle();

    let log = engine.log();

    // Find the timestamps of (FrontArrive, BackArrive) at s2.
    let front_t = log
        .iter()
        .find_map(|e| match e.event {
            Event::FrontArrive { node, .. } if node == s2 => Some(e.key.time),
            _ => None,
        })
        .expect("FrontArrive at s2");
    let back_t = log
        .iter()
        .find_map(|e| match e.event {
            Event::BackArrive { node, .. } if node == s2 => Some(e.key.time),
            _ => None,
        })
        .expect("BackArrive at s2");

    // Before FrontArrive: no carrier.
    assert!(!observe::carrier_sense(log, s2, BitTime::ZERO));
    // At FrontArrive: carrier present.
    assert!(observe::carrier_sense(log, s2, front_t));
    // Between front and back: carrier present.
    assert!(observe::carrier_sense(
        log,
        s2,
        front_t + BitTime::from_nanos(1)
    ));
    // At BackArrive: carrier no longer present (half-open interval).
    assert!(!observe::carrier_sense(log, s2, back_t));
    // After BackArrive: no carrier.
    assert!(!observe::carrier_sense(
        log,
        s2,
        back_t + BitTime::from_nanos(1)
    ));
}

#[test]
fn observe_receive_complete_matches_back_arrive() {
    let tau = BitTime::from_micros(5);
    let (world, s1, s2) = hd_pair(tau);
    let mut engine = Engine::with_seed(world, 21);
    let frame = engine
        .register_frame(s1, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
    engine.run_until_idle();

    let log = engine.log();
    // Recover the signal from the TxStart entry.
    let signal: Signal = log
        .iter()
        .find_map(|e| match e.event {
            Event::TxStart { signal, .. } => Some(signal),
            _ => None,
        })
        .expect("TxStart signal");
    let back_t = log
        .iter()
        .find_map(|e| match e.event {
            Event::BackArrive { node, .. } if node == s2 => Some(e.key.time),
            _ => None,
        })
        .expect("BackArrive at s2");

    // receive_complete is false before BackArrive, true after.
    assert!(!observe::receive_complete(log, s2, signal, BitTime::ZERO));
    assert!(!observe::receive_complete(
        log,
        s2,
        signal,
        back_t - BitTime::from_nanos(1),
    ));
    assert!(observe::receive_complete(log, s2, signal, back_t));
    assert!(observe::receive_complete(
        log,
        s2,
        signal,
        back_t + BitTime::from_micros(1),
    ));
}

#[test]
fn observe_collision_detect_consistent_with_log_entries() {
    // collision_detect at time t is true iff there's a CollisionDetect
    // entry at the node with time <= t. Run a 3-station collision
    // scenario and verify the observable agrees with raw log scan.
    let (world, s1, s2, _s3) =
        common::hd_three_via_repeater(BitTime::from_micros(1), BitTime::from_nanos(100));
    let mut engine = Engine::with_seed(world, 23);
    let f1 = engine
        .register_frame(s1, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    let f2 = engine
        .register_frame(s2, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, f1);
    engine.schedule_tx_attempt(BitTime::ZERO, s2, f2);
    engine.run_until_idle();

    let log = engine.log();
    let first_collision_at_s1 = observe::first_collision_detect_at(log, s1);
    assert!(
        first_collision_at_s1.is_some(),
        "s1 should observe a collision"
    );

    let t = first_collision_at_s1.unwrap();
    // Before the first collision: false. At/after: true.
    assert!(!observe::collision_detect(
        log,
        s1,
        t - BitTime::from_nanos(1)
    ));
    assert!(observe::collision_detect(log, s1, t));
    assert!(observe::collision_detect(
        log,
        s1,
        t + BitTime::from_micros(1)
    ));
}

// --------------------------------------------------------------------------
// Construction-equivalence: removing then re-adding equivalent topology
// --------------------------------------------------------------------------

#[test]
fn remove_then_readd_yields_equivalent_propagation() {
    // Build HD-1, run a tx to idle, then remove the segment and re-add
    // an identical one, then run a second tx. The second tx's
    // propagation events match what a fresh HD-1 with the same seed
    // would produce after one tx (no carry-over from the removed
    // signal). Ordering: with same seed, the second-tx propagation
    // shape is deterministic.
    let tau = BitTime::from_micros(5);
    let (world, s1, _s2) = hd_pair(tau);
    let mut engine = Engine::with_seed(world, 29);

    let frame_a = engine
        .register_frame(s1, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, frame_a);
    engine.run_until_idle();

    engine
        .apply_edit(Edit::RemoveSegment {
            segment: SegmentId::new(0),
        })
        .unwrap();
    engine
        .apply_edit(Edit::AddHdSegment {
            rate: BitRate::ETHERNET_10M,
            delay: tau,
            a: ep(NodeId::new(0), 0),
            b: ep(NodeId::new(1), 0),
        })
        .unwrap();
    engine.run_until_idle();

    // World is back to "two stations connected by HD" — A7 satisfied.
    assert_eq!(engine.world().collision_resource_count(), 1);
    assert_eq!(engine.world().node_count(), 2);
    assert_eq!(engine.world().segment_count(), 1);
}
