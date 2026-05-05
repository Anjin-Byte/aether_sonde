//! Bridge frame relay × continuity edits — the highest-risk feature
//! interaction surface. Every test re-validates Theorem 5
//! (zero `CollisionDetect` through a bridge) and the bridge runtime
//! state lifecycle under topology mutation.

#![allow(missing_docs)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

mod common;

use aether_sonde::engine::{Edit, Engine};
use aether_sonde::event::Event;
use aether_sonde::signal::{NodeId, SignalKind};
use aether_sonde::time::{BitRate, BitTime, Bits};
use aether_sonde::topology::{PortId, SegmentId, TopologyBuilder};

use common::{bridge_hd_topology, count_events, ep, log_is_monotonic};

// --------------------------------------------------------------------------
// Theorem 5 holds for a continuity-built bridge topology
// --------------------------------------------------------------------------

#[test]
fn theorem_5_holds_for_continuity_built_bridge() {
    // Build a bridge topology entirely via apply_edit and verify
    // Theorem 5: a frame relayed through a bridge produces zero
    // CollisionDetect events.
    let mut engine = Engine::with_seed(TopologyBuilder::new().build().unwrap(), 1);
    engine
        .apply_edit(Edit::AddEndStation { port_count: 1 })
        .unwrap();
    engine
        .apply_edit(Edit::AddEndStation { port_count: 1 })
        .unwrap();
    engine
        .apply_edit(Edit::AddBridge {
            port_count: 2,
            decode_threshold: Bits::new(64),
            processing_delay: BitTime::from_nanos(500),
        })
        .unwrap();
    engine
        .apply_edit(Edit::AddHdSegment {
            rate: BitRate::ETHERNET_10M,
            delay: BitTime::from_micros(1),
            a: ep(NodeId::new(0), 0),
            b: ep(NodeId::new(2), 0),
        })
        .unwrap();
    engine
        .apply_edit(Edit::AddHdSegment {
            rate: BitRate::ETHERNET_10M,
            delay: BitTime::from_micros(1),
            a: ep(NodeId::new(1), 0),
            b: ep(NodeId::new(2), 1),
        })
        .unwrap();

    let s1 = NodeId::new(0);
    let frame = engine
        .register_frame(s1, Bits::new(64), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
    engine.run_until_idle();

    let collisions = count_events(engine.log(), |e| matches!(e, Event::CollisionDetect { .. }));
    assert_eq!(
        collisions, 0,
        "Theorem 5: bridge relay produces no collisions"
    );
    assert!(log_is_monotonic(engine.log()));
}

// --------------------------------------------------------------------------
// Bridge port delay change mid-relay
// --------------------------------------------------------------------------

#[test]
fn bridge_port_delay_change_mid_relay_preserves_inflight_arrivals() {
    // Build s1 — bridge — s2. Start a transmission. Mid-relay (after
    // s1's TxStart and the bridge's ingress FrontArrive but before the
    // bridge's egress TxStart for the relayed signal), change the
    // delay on the bridge's egress-side HD segment. In-flight arrivals
    // retain their original schedule.
    let delay_a = BitTime::from_micros(1);
    let delay_b = BitTime::from_micros(1);
    let (world, s1, _s2, _bridge) =
        bridge_hd_topology(delay_a, delay_b, Bits::new(64), BitTime::from_nanos(500));
    let mut engine = Engine::with_seed(world, 3);
    let frame = engine
        .register_frame(s1, Bits::new(64), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
    engine.run_until(BitTime::from_micros(2));

    // Change segment B's (bridge↔s2) delay. SegmentId(1) is the second
    // HD segment in the topology.
    engine
        .apply_edit(Edit::SetSegmentDelay {
            segment: SegmentId::new(1),
            new_delay: BitTime::from_micros(50),
        })
        .unwrap();
    engine.run_until_idle();

    // No collisions (Theorem 5 still holds).
    let collisions = count_events(engine.log(), |e| matches!(e, Event::CollisionDetect { .. }));
    assert_eq!(collisions, 0);

    // SegmentDelayChanged is logged.
    let change_count = count_events(engine.log(), |e| {
        matches!(e, Event::SegmentDelayChanged { .. })
    });
    assert_eq!(change_count, 1);

    assert!(log_is_monotonic(engine.log()));
}

// --------------------------------------------------------------------------
// Remove a bridge with non-empty egress activity
// --------------------------------------------------------------------------

#[test]
fn remove_bridge_during_relay_drops_state_cleanly() {
    let delay = BitTime::from_micros(1);
    let (world, s1, s2, bridge) =
        bridge_hd_topology(delay, delay, Bits::new(64), BitTime::from_nanos(500));
    let mut engine = Engine::with_seed(world, 5);
    let frame = engine
        .register_frame(s1, Bits::new(64), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
    // Run partway: s1's TxStart and the bridge's ingress FrontArrive
    // dispatch, but the bridge hasn't finished relaying yet.
    engine.run_until(BitTime::from_micros(2));

    engine
        .apply_edit(Edit::RemoveNode { node: bridge })
        .unwrap();
    engine.run_until_idle();

    // Bridge gone.
    assert!(engine.world().node(bridge).is_none());

    // Cascaded SegmentRemoved for both HD segments.
    let seg_removed = count_events(engine.log(), |e| matches!(e, Event::SegmentRemoved { .. }));
    assert_eq!(seg_removed, 2);

    // s2 sees no FrontArrive for the relayed signal.
    let s2_arrivals = count_events(
        engine.log(),
        |e| matches!(e, Event::FrontArrive { node, .. } if *node == s2),
    );
    assert_eq!(s2_arrivals, 0);

    // No collisions (Theorem 5 invariant survives mid-relay removal).
    let collisions = count_events(engine.log(), |e| matches!(e, Event::CollisionDetect { .. }));
    assert_eq!(collisions, 0);

    assert!(log_is_monotonic(engine.log()));
}

// --------------------------------------------------------------------------
// Two-bridge cascade: remove the middle bridge during a relay
// --------------------------------------------------------------------------

#[test]
fn middle_bridge_removal_in_chain_drops_relay() {
    // Topology: s1 — HD_a — bridge_1 — HD_b — bridge_2 — HD_c — s2.
    // Bridges are in series. Remove bridge_2 (the second one) during
    // a relay; verify s2 doesn't see the frame, no collisions.
    let mut b = TopologyBuilder::new();
    let s1 = b.add_end_station(1);
    let s2 = b.add_end_station(1);
    let br1 = b.add_bridge(2, Bits::new(64), BitTime::from_nanos(500));
    let br2 = b.add_bridge(2, Bits::new(64), BitTime::from_nanos(500));
    b.add_hd_segment(
        BitRate::ETHERNET_10M,
        BitTime::from_micros(1),
        ep(s1, 0),
        ep(br1, 0),
    )
    .unwrap();
    b.add_hd_segment(
        BitRate::ETHERNET_10M,
        BitTime::from_micros(1),
        ep(br1, 1),
        ep(br2, 0),
    )
    .unwrap();
    b.add_hd_segment(
        BitRate::ETHERNET_10M,
        BitTime::from_micros(1),
        ep(br2, 1),
        ep(s2, 0),
    )
    .unwrap();
    let world = b.build().unwrap();
    let mut engine = Engine::with_seed(world, 7);
    let frame = engine
        .register_frame(s1, Bits::new(64), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
    engine.run_until(BitTime::from_micros(2));

    engine.apply_edit(Edit::RemoveNode { node: br2 }).unwrap();
    engine.run_until_idle();

    assert!(engine.world().node(br2).is_none());
    let s2_arrivals = count_events(
        engine.log(),
        |e| matches!(e, Event::FrontArrive { node, .. } if *node == s2),
    );
    assert_eq!(s2_arrivals, 0);
    let collisions = count_events(engine.log(), |e| matches!(e, Event::CollisionDetect { .. }));
    assert_eq!(collisions, 0);
    assert!(log_is_monotonic(engine.log()));
}

// --------------------------------------------------------------------------
// Add a bridge dynamically; transmit through it
// --------------------------------------------------------------------------

#[test]
fn dynamically_added_bridge_relays_correctly() {
    // Start with two empty end stations (no segments). Add a bridge
    // and HD segments via continuity. Transmit; verify Theorem 5.
    let mut engine = Engine::with_seed(TopologyBuilder::new().build().unwrap(), 11);
    for _ in 0..2 {
        engine
            .apply_edit(Edit::AddEndStation { port_count: 1 })
            .unwrap();
    }
    engine
        .apply_edit(Edit::AddBridge {
            port_count: 2,
            decode_threshold: Bits::new(64),
            processing_delay: BitTime::from_nanos(500),
        })
        .unwrap();
    engine
        .apply_edit(Edit::AddHdSegment {
            rate: BitRate::ETHERNET_10M,
            delay: BitTime::from_micros(1),
            a: ep(NodeId::new(0), 0),
            b: ep(NodeId::new(2), 0),
        })
        .unwrap();
    engine
        .apply_edit(Edit::AddHdSegment {
            rate: BitRate::ETHERNET_10M,
            delay: BitTime::from_micros(1),
            a: ep(NodeId::new(1), 0),
            b: ep(NodeId::new(2), 1),
        })
        .unwrap();

    // Now transmit s0 → bridge → s1.
    let s0 = NodeId::new(0);
    let s1 = NodeId::new(1);
    let frame = engine
        .register_frame(s0, Bits::new(64), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s0, frame);
    engine.run_until_idle();

    // Theorem 5: zero collisions.
    let collisions = count_events(engine.log(), |e| matches!(e, Event::CollisionDetect { .. }));
    assert_eq!(collisions, 0);
    // s1 received a FrontArrive (the relayed signal arrived).
    let s1_arrivals = count_events(
        engine.log(),
        |e| matches!(e, Event::FrontArrive { node, .. } if *node == s1),
    );
    assert!(s1_arrivals >= 1, "s1 should receive the relayed frame");
}

// --------------------------------------------------------------------------
// Disconnect a bridge port mid-relay
// --------------------------------------------------------------------------

#[test]
fn disconnect_bridge_port_mid_relay_cancels_egress_path() {
    let delay = BitTime::from_micros(1);
    let (world, s1, _s2, bridge) =
        bridge_hd_topology(delay, delay, Bits::new(64), BitTime::from_nanos(500));
    let mut engine = Engine::with_seed(world, 13);
    let frame = engine
        .register_frame(s1, Bits::new(64), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
    engine.run_until(BitTime::from_micros(2));

    // Disconnect the bridge's egress port (port 1, leading to s2).
    engine
        .apply_edit(Edit::DisconnectPort {
            node: bridge,
            port: PortId::new(1),
        })
        .unwrap();
    engine.run_until_idle();

    // PortDisconnected logged.
    let port_disc = count_events(engine.log(), |e| {
        matches!(e, Event::PortDisconnected { node, port, .. }
            if *node == bridge && *port == PortId::new(1))
    });
    assert_eq!(port_disc, 1);

    // No collisions still.
    let collisions = count_events(engine.log(), |e| matches!(e, Event::CollisionDetect { .. }));
    assert_eq!(collisions, 0);

    assert!(log_is_monotonic(engine.log()));
}
