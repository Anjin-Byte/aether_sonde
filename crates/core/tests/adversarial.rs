//! Adversarial integration scenarios per the codex's
//! `Tests Should Make Programs Fail` and `Output Format` §4.c. Each test
//! combines features unpleasantly and asserts exact log shape.
//!
//! These tests use **only the public API** of `aether-sonde`.

#![allow(missing_docs)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

mod common;

use aether_sonde::engine::{Edit, Engine};
use aether_sonde::event::{Event, Phase, SignalLostReason};
use aether_sonde::signal::{NodeId, SignalKind};
use aether_sonde::time::{BitRate, BitTime, Bits};
use aether_sonde::topology::{PortId, SegmentId};

use common::{
    bridge_hd_topology, count_events, ep, fd_pair, hd_pair, hd_three_via_repeater, log_is_monotonic,
};

// --------------------------------------------------------------------------
// Scenario 1: Bridge mid-relay removal
// --------------------------------------------------------------------------

#[test]
fn bridge_mid_relay_removal_cascades_cleanly() {
    // Build s1 — bridge — s2 (two HD-1 components joined by a bridge).
    // s1 transmits a frame; the bridge begins relaying. Mid-relay, the
    // bridge is removed. Verify:
    //   - NodeRemoved appears in the log exactly once for the bridge.
    //   - Cascaded SegmentRemoved entries appear (one per incident HD).
    //   - s2 receives no FrontArrive for the relayed signal.
    //   - The log is monotonic.
    let delay = BitTime::from_micros(1);
    let (world, s1, s2, bridge) =
        bridge_hd_topology(delay, delay, Bits::new(64), BitTime::from_nanos(500));
    let mut engine = Engine::with_seed(world, 7);
    let frame = engine
        .register_frame(s1, Bits::new(64), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
    // Run partway: enough for s1's TxStart and the bridge's ingress
    // FrontArrive to dispatch, but not so far that the bridge has
    // finished relaying.
    engine.run_until(BitTime::from_micros(2));

    engine
        .apply_edit(Edit::RemoveNode { node: bridge })
        .unwrap();
    engine.run_until_idle();

    // Bridge gone.
    assert!(engine.world().node(bridge).is_none());

    // Exactly one NodeRemoved for the bridge.
    let node_removed = count_events(engine.log(), |e| {
        matches!(
            e,
            Event::NodeRemoved { node } if *node == bridge,
        )
    });
    assert_eq!(node_removed, 1);

    // Two cascaded SegmentRemoved (one per incident HD segment).
    let seg_removed = count_events(engine.log(), |e| matches!(e, Event::SegmentRemoved { .. }));
    assert_eq!(seg_removed, 2);

    // s2 sees no FrontArrive for the relayed signal — the bridge never
    // got to emit a relay TxStart.
    let s2_arrivals = count_events(
        engine.log(),
        |e| matches!(e, Event::FrontArrive { node, .. } if *node == s2),
    );
    assert_eq!(s2_arrivals, 0);

    assert!(log_is_monotonic(engine.log()));
}

// --------------------------------------------------------------------------
// Scenario 4: Edit at exact t=0
// --------------------------------------------------------------------------

#[test]
fn edit_at_t_zero_is_logged_at_t_zero() {
    // Empty engine, apply an edit before any run; the edit must log at
    // t=0 in LocalDecision phase. Subsequent transmissions see the
    // post-edit topology.
    let mut engine = Engine::with_seed(
        aether_sonde::topology::TopologyBuilder::new()
            .build()
            .unwrap(),
        0,
    );
    engine
        .apply_edit(Edit::AddEndStation { port_count: 1 })
        .unwrap();
    engine
        .apply_edit(Edit::AddEndStation { port_count: 1 })
        .unwrap();
    engine
        .apply_edit(Edit::AddHdSegment {
            rate: BitRate::ETHERNET_10M,
            delay: BitTime::from_micros(1),
            a: ep(NodeId::new(0), 0),
            b: ep(NodeId::new(1), 0),
        })
        .unwrap();
    // No run yet. Inspect log: edits scheduled but not dispatched.
    assert!(engine.log().is_empty());

    engine.run_until_idle();

    // All four topology events log at t=0, in LocalDecision phase.
    for entry in engine.log().iter() {
        assert_eq!(entry.key.time, BitTime::ZERO);
        assert_eq!(entry.key.phase, Phase::LocalDecision);
    }
    let node_added = count_events(engine.log(), |e| matches!(e, Event::NodeAdded { .. }));
    let seg_added = count_events(engine.log(), |e| matches!(e, Event::SegmentAdded { .. }));
    assert_eq!(node_added, 2);
    assert_eq!(seg_added, 1);
}

// --------------------------------------------------------------------------
// Scenario 6: Rapid 50-edit batch
// --------------------------------------------------------------------------

#[test]
fn rapid_50_edit_batch_logs_each() {
    // Apply 50 AddEndStation edits in succession. Verify exactly 50
    // NodeAdded entries log in order, world has 50 nodes, log monotonic.
    let mut engine = Engine::with_seed(
        aether_sonde::topology::TopologyBuilder::new()
            .build()
            .unwrap(),
        0,
    );
    for _ in 0..50 {
        engine
            .apply_edit(Edit::AddEndStation { port_count: 1 })
            .unwrap();
    }
    engine.run_until_idle();
    assert_eq!(engine.world().node_count(), 50);
    let added = count_events(engine.log(), |e| matches!(e, Event::NodeAdded { .. }));
    assert_eq!(added, 50);
    assert!(log_is_monotonic(engine.log()));

    // Sequential NodeIds: NodeAdded events log NodeId(0)..NodeId(49) in order.
    let ids: Vec<u32> = engine
        .log()
        .iter()
        .filter_map(|e| match e.event {
            Event::NodeAdded { node } => Some(node.as_u32()),
            _ => None,
        })
        .collect();
    let expected: Vec<u32> = (0..50).collect();
    assert_eq!(ids, expected);
}

// --------------------------------------------------------------------------
// Scenario 7: FD rate change on active link
// --------------------------------------------------------------------------

#[test]
fn fd_rate_change_preserves_inflight_signal_schedule() {
    // FD pair, frame in flight. Change rate; the in-flight signal
    // completes per its original schedule (its arrival timestamps are
    // already in the queue with absolute values).
    let delay = BitTime::from_nanos(100);
    let (world, s1, s2) = fd_pair(delay, BitRate::ETHERNET_1G);
    let mut engine = Engine::with_seed(world, 1);
    let frame = engine
        .register_frame(s1, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_1G)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
    // Dispatch TxAttempt + TxStart at t=0; FrontArrive at delay remains queued.
    engine.run_until(BitTime::from_nanos(50));

    engine
        .apply_edit(Edit::SetSegmentRate {
            segment: SegmentId::new(0),
            new_rate: BitRate::ETHERNET_10G,
        })
        .unwrap();

    engine.run_until_idle();

    // The original FrontArrive at s2 fires at the *original* delay,
    // unchanged by the rate edit. (Rate doesn't alter propagation
    // delay; it alters bit-duration. The signal's duration is baked
    // in at TxStart per I11.)
    let front_at_s2 = engine
        .log()
        .iter()
        .find(|e| matches!(e.event, Event::FrontArrive { node, .. } if node == s2))
        .expect("FrontArrive at s2 should be logged");
    assert_eq!(front_at_s2.key.time, delay);

    // SegmentRateChanged is in the log with old/new values.
    let rate_change = engine
        .log()
        .iter()
        .find(|e| matches!(e.event, Event::SegmentRateChanged { .. }))
        .expect("SegmentRateChanged should be logged");
    match rate_change.event {
        Event::SegmentRateChanged { old, new, .. } => {
            assert_eq!(old, BitRate::ETHERNET_1G);
            assert_eq!(new, BitRate::ETHERNET_10G);
        }
        _ => unreachable!(),
    }
    assert!(log_is_monotonic(engine.log()));
}

// --------------------------------------------------------------------------
// Scenario 8: Disconnect → reconnect mid-flight
// --------------------------------------------------------------------------

#[test]
fn disconnect_then_reconnect_mid_flight() {
    // Disconnect a port mid-flight (cancels in-flight arrivals), then
    // re-add a fresh HD segment to the same ports. Run a second
    // transmission; verify it propagates normally on the new segment.
    let tau = BitTime::from_micros(5);
    let (world, s1, s2) = hd_pair(tau);
    let mut engine = Engine::with_seed(world, 1);
    let frame_a = engine
        .register_frame(s1, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, frame_a);
    engine.run_until(BitTime::from_nanos(1_000));

    // Disconnect: cancels FrontArrive(t=tau) and BackArrive(t=tau+D_σ)
    // at s2; logs PortDisconnected.
    engine
        .apply_edit(Edit::DisconnectPort {
            node: s2,
            port: PortId::new(0),
        })
        .unwrap();
    // Re-add a fresh HD segment to the same endpoints.
    engine
        .apply_edit(Edit::AddHdSegment {
            rate: BitRate::ETHERNET_10M,
            delay: BitTime::from_micros(2),
            a: ep(s1, 0),
            b: ep(s2, 0),
        })
        .unwrap();
    // Drain everything from frame A.
    engine.run_until_idle();

    // Two SignalLost entries (FrontArrive + BackArrive at s2 from frame A).
    let lost = count_events(engine.log(), |e| {
        matches!(
            e,
            Event::SignalLost {
                reason: SignalLostReason::PortDisconnected,
                ..
            },
        )
    });
    assert_eq!(lost, 2);

    // Schedule frame B on the new segment after frame A's TxEnd settles.
    // Use last_processed_time-relative scheduling: just pick a comfortable
    // time after frame A's TxEnd at t=51.2µs.
    let frame_b = engine
        .register_frame(s1, Bits::new(64), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::from_nanos(70_000), s1, frame_b);
    engine.run_until_idle();

    // Frame B's FrontArrive at s2 fires at t = 70_000 + 2µs = 72µs (new segment delay).
    let front_b_time = BitTime::from_nanos(70_000) + BitTime::from_micros(2);
    let front_b = engine.log().iter().find(|e| {
        matches!(e.event, Event::FrontArrive { node, .. } if node == s2)
            && e.key.time == front_b_time
    });
    assert!(
        front_b.is_some(),
        "frame B's FrontArrive on the new segment should land at the new delay",
    );
    assert!(log_is_monotonic(engine.log()));
}

// --------------------------------------------------------------------------
// Scenario 2: Multi-station HD collision under removal
// --------------------------------------------------------------------------

#[test]
fn multi_station_hd_collision_under_node_removal() {
    // 3 stations + repeater in one HD component. s1 and s2 both
    // TxAttempt at t=0 (collision). After the collision is detected,
    // remove s3 (which never transmitted). Verify the s1/s2 collision
    // still appears in the log; s3's queued events tombstone.
    let delay = BitTime::from_micros(1);
    let delta_h = BitTime::from_nanos(100);
    let (world, s1, s2, s3) = hd_three_via_repeater(delay, delta_h);
    let mut engine = Engine::with_seed(world, 1);
    let f1 = engine
        .register_frame(s1, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    let f2 = engine
        .register_frame(s2, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, f1);
    engine.schedule_tx_attempt(BitTime::ZERO, s2, f2);
    // Run far enough that the collision is observed at s1 and s2 but
    // before transmissions complete.
    engine.run_until(BitTime::from_micros(3));

    engine.apply_edit(Edit::RemoveNode { node: s3 }).unwrap();
    engine.run_until_idle();

    // s3 is removed.
    assert!(engine.world().node(s3).is_none());

    // Both s1 and s2 see CollisionDetect at some point.
    let s1_collisions = count_events(
        engine.log(),
        |e| matches!(e, Event::CollisionDetect { node, .. } if *node == s1),
    );
    let s2_collisions = count_events(
        engine.log(),
        |e| matches!(e, Event::CollisionDetect { node, .. } if *node == s2),
    );
    assert!(s1_collisions >= 1, "s1 should have detected a collision");
    assert!(s2_collisions >= 1, "s2 should have detected a collision");

    // NodeRemoved for s3 logged exactly once.
    let s3_removed = count_events(engine.log(), |e| {
        matches!(
            e,
            Event::NodeRemoved { node } if *node == s3,
        )
    });
    assert_eq!(s3_removed, 1);

    assert!(log_is_monotonic(engine.log()));
}

// --------------------------------------------------------------------------
// Scenario 3: Repeater chain delay change mid-flight
// --------------------------------------------------------------------------

#[test]
fn repeater_chain_delay_change_mid_flight_preserves_inflight_arrival() {
    // Topology: s1 — H_a — r1 — H_b — r2 — H_c — s2 (3 HD segments,
    // 2 repeaters). Each segment has delay τ. Total propagation delay
    // for a frame from s1 to s2: 3τ + 2·δ_h.
    //
    // TxAttempt from s1 at t=0. Mid-flight (after TxStart but before
    // s2's FrontArrive), change H_b's delay. Verify the in-flight
    // signal's FrontArrive at s2 fires at the *original* total delay
    // (the arrival was already scheduled with the old delay at TxStart).
    let tau = BitTime::from_micros(2);
    let delta_h = BitTime::from_nanos(100);
    let mut b = aether_sonde::topology::TopologyBuilder::new();
    let s1 = b.add_end_station(1);
    let s2 = b.add_end_station(1);
    let r1 = b.add_repeater(2, delta_h);
    let r2 = b.add_repeater(2, delta_h);
    b.add_hd_segment(BitRate::ETHERNET_10M, tau, ep(s1, 0), ep(r1, 0))
        .unwrap();
    let h_b = b
        .add_hd_segment(BitRate::ETHERNET_10M, tau, ep(r1, 1), ep(r2, 0))
        .unwrap();
    b.add_hd_segment(BitRate::ETHERNET_10M, tau, ep(r2, 1), ep(s2, 0))
        .unwrap();
    let world = b.build().unwrap();
    let mut engine = Engine::with_seed(world, 0);
    let frame = engine
        .register_frame(s1, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
    // The original total delay: 3τ + 2·δ_h.
    let original_total_delay = tau + tau + tau + delta_h + delta_h;
    // Run partway — past s1's TxStart but well before s2's FrontArrive.
    engine.run_until(BitTime::from_micros(1));

    // Change H_b's delay to something dramatic.
    let new_delay = BitTime::from_micros(50);
    engine
        .apply_edit(Edit::SetSegmentDelay {
            segment: h_b,
            new_delay,
        })
        .unwrap();
    engine.run_until_idle();

    // The in-flight FrontArrive at s2 fires at the original total delay,
    // not the new one.
    let front_s2 = engine
        .log()
        .iter()
        .find(|e| matches!(e.event, Event::FrontArrive { node, .. } if node == s2))
        .expect("s2's FrontArrive should be logged");
    assert_eq!(
        front_s2.key.time, original_total_delay,
        "in-flight signal retains its original schedule across a delay change",
    );

    // SegmentDelayChanged is logged.
    let change = engine
        .log()
        .iter()
        .find(|e| matches!(e.event, Event::SegmentDelayChanged { .. }))
        .expect("SegmentDelayChanged should be logged");
    match change.event {
        Event::SegmentDelayChanged { old, new, .. } => {
            assert_eq!(old, tau);
            assert_eq!(new, new_delay);
        }
        _ => unreachable!(),
    }
    assert!(log_is_monotonic(engine.log()));
}

// --------------------------------------------------------------------------
// Scenario 5: Simultaneous edit + queued event (deterministic ordering)
// --------------------------------------------------------------------------

#[test]
fn simultaneous_edit_and_event_serialize_deterministically() {
    // Build empty world; schedule a TxAttempt at t=0 from a node that
    // will exist post-edit; apply edits in interleaved order; run.
    // Verify: edits and TxAttempt at the same timestamp are serialized
    // by their serial_id (insertion order), and re-runs produce the
    // same serial_id assignment.
    let run = || -> Vec<(BitTime, Phase, u64)> {
        let mut engine = Engine::with_seed(
            aether_sonde::topology::TopologyBuilder::new()
                .build()
                .unwrap(),
            42,
        );
        engine
            .apply_edit(Edit::AddEndStation { port_count: 1 })
            .unwrap();
        engine
            .apply_edit(Edit::AddEndStation { port_count: 1 })
            .unwrap();
        engine
            .apply_edit(Edit::AddHdSegment {
                rate: BitRate::ETHERNET_10M,
                delay: BitTime::from_micros(1),
                a: ep(NodeId::new(0), 0),
                b: ep(NodeId::new(1), 0),
            })
            .unwrap();
        let frame = engine
            .register_frame(
                NodeId::new(0),
                Bits::new(64),
                SignalKind::Frame,
                BitRate::ETHERNET_10M,
            )
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, NodeId::new(0), frame);
        engine.run_until_idle();
        engine
            .log()
            .iter()
            .map(|e| (e.key.time, e.key.phase, e.key.serial_id))
            .collect()
    };
    assert_eq!(run(), run());
}

// --------------------------------------------------------------------------
// Bonus: verify large schedule + edit history doesn't break
// --------------------------------------------------------------------------

#[test]
fn long_schedule_with_periodic_edits_is_stable() {
    // 100 transmissions over 10 ms with a SetSegmentDelay every 10
    // transmissions. Verify run completes, log monotonic, no panics.
    let (world, s1, _) = fd_pair(BitTime::from_nanos(100), BitRate::ETHERNET_1G);
    let mut engine = Engine::with_seed(world, 9);
    for i in 0..100u32 {
        let frame = engine
            .register_frame(s1, Bits::new(64), SignalKind::Frame, BitRate::ETHERNET_1G)
            .unwrap();
        let t = BitTime::from_micros(u64::from(i) * 100);
        engine.schedule_tx_attempt(t, s1, frame);
        if i % 10 == 9 {
            // Vary the delay slightly.
            engine
                .apply_edit(Edit::SetSegmentDelay {
                    segment: SegmentId::new(0),
                    new_delay: BitTime::from_nanos(100 + u64::from(i)),
                })
                .unwrap();
        }
    }
    engine.run_until_idle();
    assert!(log_is_monotonic(engine.log()));
    // 100 TxAttempt entries.
    let attempts = count_events(engine.log(), |e| matches!(e, Event::TxAttempt { .. }));
    assert_eq!(attempts, 100);
}
